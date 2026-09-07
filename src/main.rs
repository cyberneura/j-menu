//! `jj-menu` — a simple TUI menu launcher.
//!
//! See the README for the configuration format.

mod config;
mod exec;
mod launchers;
mod menu;
mod parallel;
mod shell_init;
mod signal;
mod ui;

use std::io::{IsTerminal, Write, stderr, stdout};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use config::Launch;
use shell_init::ShellKind;

/// Exit code used when the menu is dismissed without choosing anything.
///
/// 130 is the conventional "terminated by Ctrl-C" code, which keeps the shell
/// wrapper from evaluating an empty command.
const EXIT_CANCELLED: u8 = 130;

#[derive(Parser, Debug)]
#[command(
    name = "jj-menu",
    version,
    about = "A simple TUI menu launcher",
    long_about = None
)]
struct Args {
    /// Print the selected command to stdout instead of running it.
    ///
    /// Used by the shell wrapper so that `cd` and `export` affect the calling
    /// shell. See `jj-menu --shell-init <shell>`.
    #[arg(long)]
    print: bool,

    /// Print the shell function to add to your startup file.
    #[arg(long, value_name = "SHELL")]
    shell_init: Option<ShellKind>,

    /// Start the search for configuration files here instead of the working
    /// directory.
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// List the configuration files that were loaded, then exit.
    #[arg(long)]
    show_config: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            let mut out = stderr();
            // Unlike the menu, this can end up in a file or a pipe, so the
            // colour depends on where it is going.
            if out.is_terminal() {
                let _ = ui::theme::paint(
                    &mut out,
                    ui::theme::Style::fg(ui::theme::ERROR).bold(),
                    "jj-menu:",
                );
                let _ = writeln!(out, " {err:#}");
            } else {
                let _ = writeln!(out, "jj-menu: {err:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let args = Args::parse();

    if let Some(kind) = args.shell_init {
        print!("{}", shell_init::snippet(kind, &shell_init::program()));
        return Ok(ExitCode::SUCCESS);
    }

    // Where the calling shell is standing, which `--cwd` does not move: that
    // flag only says where to start looking for configuration files. The two
    // are the same almost always, and telling them apart is what keeps
    // `jj --cwd /project` from /tmp printing a command that then runs in /tmp.
    //
    // An option, because a process outlives its working directory being
    // deleted and that is exactly when `--cwd` earns its keep. Only the
    // printed `cd` wants this, and not knowing where the shell is means
    // printing one rather than leaving it out.
    let invoked_from = std::env::current_dir()
        .ok()
        .and_then(|dir| std::fs::canonicalize(dir).ok());

    let Some(start_dir) = args.cwd.clone().or_else(|| invoked_from.clone()) else {
        anyhow::bail!("failed to read the working directory; pass --cwd to say where to look");
    };
    // Absolute from here on. The launchers build `cd <dir> && ...` commands
    // for projects found in an ancestor, and those run with the working
    // directory already set to `start_dir` — a relative path would then be
    // resolved a second time and point somewhere else entirely.
    let start_dir = std::fs::canonicalize(&start_dir)
        .with_context(|| format!("failed to resolve {}", start_dir.display()))?;

    let config = config::load(&start_dir)?;

    if args.show_config {
        report_config(&config, &start_dir);
        return Ok(ExitCode::SUCCESS);
    }

    let items = menu::build(&config, &start_dir);
    if items.is_empty() {
        eprintln!("{}", no_entries_help());
        return Ok(ExitCode::FAILURE);
    }

    // The menu reads keys from stdin and draws on stderr, so both have to be
    // a terminal. Checking stdin alone would let `jj 2>menu.log` block on a
    // menu nobody can see, with the escape sequences going into the log.
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("no terminal available (stdin is not a TTY)");
    }
    if !stderr().is_terminal() {
        anyhow::bail!("no terminal available (stderr is not a TTY; the menu is drawn there)");
    }

    let title = format!("jj-menu — {}", start_dir.display());
    match ui::run(items, &title)? {
        ui::Outcome::Cancelled => Ok(ExitCode::from(EXIT_CANCELLED)),
        ui::Outcome::Run(Launch::Script(script), cwd) => {
            let cwd = cwd.as_deref().unwrap_or(&start_dir);
            if args.print {
                let mut out = stdout();
                writeln!(
                    out,
                    "{}",
                    in_dir_script(cwd, &script, invoked_from.as_deref())?
                )?;
                out.flush()?;
                return Ok(ExitCode::SUCCESS);
            }

            // Echoed with the `cd` the child is given as its working
            // directory, so what is on screen is what is being run. An entry
            // from an ancestor's file otherwise looks like it runs here.
            echo("$", &echo_script(cwd, &script, invoked_from.as_deref()))?;
            let status = exec::run(&script, cwd)?;
            // Pass the command's exit code through, so `jj && next` and `$?`
            // behave the way they would for a typed command.
            Ok(ExitCode::from(exec::exit_code(status)))
        }
        ui::Outcome::Run(Launch::Parallel(jobs), cwd) => {
            let cwd = cwd.as_deref().unwrap_or(&start_dir);
            // Run here even under `--print`. There is nothing to hand back to
            // the calling shell: the point of `--print` is that `cd` and
            // `export` reach the user's shell, and a group is several separate
            // processes, none of which could change it anyway. Printing them
            // as one script would mean writing the `&`-and-`wait` form of
            // whichever shell is calling — and fish, which the wrapper
            // supports, does not have it.
            for job in &jobs {
                echo("&", &echo_script(cwd, &job.script, invoked_from.as_deref()))?;
            }
            // With `--print` the wrapper is reading stdout through a command
            // substitution and evaluates whatever comes back; a job writing
            // there would be *run*, not shown. Send it to the terminal instead.
            let output = if args.print {
                parallel::Output::Stderr
            } else {
                parallel::Output::Inherit
            };
            Ok(ExitCode::from(parallel::run(&jobs, cwd, output)?))
        }
    }
}

/// Prefix a whole script with the `cd` that puts it in `dir`.
///
/// `--print` hands the command to the calling shell, which is the one process
/// whose working directory `exec::run` cannot set, so the directory has to be
/// part of the command itself. That `cd` stays in effect afterwards: a
/// directory change reaching your own shell is what the wrapper is for, and an
/// entry that would rather not move you says `run_in_current_directory`.
///
/// **`;` rather than `&&`.** `&&` binds tighter than `&`, so
/// `cd dir && server &` would put the `cd` in the background along with the
/// command and leave the rest of the script running where the shell already
/// was.
///
/// A subshell would be the tidier tool -- it would leave the calling shell
/// where it was -- but there is no form of one that bash, zsh *and* fish all
/// accept, and it would swallow the `cd` and `export` effects that are the
/// whole point of `--print`.
///
/// **The directory is checked here instead of by the `cd`.** For the same
/// reason there is no portable grouping, there is no portable "change
/// directory, and stop if that failed": `||` and `&&` reach one command, and
/// `return` is not a thing fish has outside a function. So a `cd` that fails
/// in the caller's shell would leave the rest of the script running wherever
/// that shell already was, which for a path-sensitive command is much worse
/// than not running it. Refusing here is what [`exec::run`] does on the other
/// path, where `Command::current_dir` never starts the child at all. It leaves
/// the moment between this check and the shell's `cd` uncovered, which is as
/// close as a printed command can get.
///
/// `invoked_from` is `None` when the calling shell's directory could not be
/// read, which is a deleted working directory. The `cd` is then printed
/// unconditionally: it is only ever left out as a shortcut for "you are
/// already there", and guessing that from nothing would be the wrong way to be
/// wrong.
fn in_dir_script(
    dir: &std::path::Path,
    script: &str,
    invoked_from: Option<&std::path::Path>,
) -> Result<String> {
    if invoked_from == Some(dir) {
        return Ok(script.to_string());
    }
    // `dir.join(".")` and not `dir`: entering a directory needs search
    // permission *on* it, and resolving a component inside it is what asks for
    // that. Plain `is_dir` reads the entry out of the parent instead, so it
    // still says yes for a directory whose `+x` has been taken away -- which
    // `cd` would then refuse, leaving the script to run where the shell was.
    match std::fs::metadata(dir.join(".")) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => anyhow::bail!(
            "{} is not a directory, so the entry has nowhere to run",
            dir.display()
        ),
        Err(err) => anyhow::bail!(
            "{} cannot be entered ({err}), so the entry has nowhere to run",
            dir.display()
        ),
    }
    // A shell command is text, and a path on Unix is bytes. `to_string_lossy`
    // would hand the shell a `cd` into a path with U+FFFD where the original
    // bytes were -- a different directory, and almost certainly a missing one,
    // which lands right back in the case above.
    let Some(dir) = dir.to_str() else {
        anyhow::bail!(
            "{} cannot be put in a shell command: its name is not valid UTF-8",
            dir.display()
        );
    };
    Ok(format!("cd {}; {script}", launchers::quote(dir)))
}

/// The same, for showing on screen before the command runs.
///
/// Cosmetic, so a directory that cannot be written as a command is shown as
/// the bare script rather than stopping the entry: on this path the child is
/// given the directory through `Command::current_dir`, which takes the bytes
/// as they are and reports its own error if they have gone stale.
fn echo_script(
    dir: &std::path::Path,
    script: &str,
    invoked_from: Option<&std::path::Path>,
) -> String {
    in_dir_script(dir, script, invoked_from).unwrap_or_else(|_| script.to_string())
}

/// Echo a command before it runs, the way a shell shows what it is doing.
///
/// `sigil` is `$` for a single command and `&` for one member of a parallel
/// group. stderr is known to be a terminal here: the menu was just drawn on it.
fn echo(sigil: &str, script: &str) -> Result<()> {
    let mut out = stderr();
    ui::theme::paint(
        &mut out,
        ui::theme::Style::fg(ui::theme::COMMAND).bold(),
        sigil,
    )?;
    writeln!(out, " {script}")?;
    out.flush()?;
    Ok(())
}

fn report_config(config: &config::Config, start_dir: &std::path::Path) {
    if config.sources.is_empty() {
        println!(
            "no configuration file found (searched from {})",
            start_dir.display()
        );
    } else {
        println!("configuration files, in load order:");
        for source in &config.sources {
            // "by default", because this is the file's setting: an entry
            // carrying `run_in_current_directory` of its own overrides it and
            // does not show up here, which a bare "runs in ..." would deny.
            match &source.cwd {
                Some(dir) => println!(
                    "  {} (entries run in {} by default)",
                    source.path.display(),
                    dir.display()
                ),
                None => println!(
                    "  {} (entries run in the working directory by default)",
                    source.path.display()
                ),
            }
        }
    }
    println!("menu entries from configuration: {}", config.menu.len());
    println!(
        "built-in launchers enabled: {}",
        config.auto_launchers.any()
    );
}

fn no_entries_help() -> String {
    "\
jj-menu: nothing to show.

Create a configuration file in this directory or an ancestor, for example
.jj-menu.yaml:

  menu:
    - title: List files
      shell: ls -la
    - title: Git log
      shell: git log --oneline --graph --decorate --all
    - title: Dev servers
      # Each entry gets its own shell and they all run at once; Ctrl-C
      # stops the lot.
      parallel:
        - shell: npm run dev
        - shell: npm run api

    - title: Deploy
      help: Pick the environment to deploy to.
      submenu:
        - title: staging
          shell: ./deploy.sh staging
        - title: production
          shell: ./deploy.sh production
    - title: Search
      shell: rg {pattern}
      args:
        - name: pattern
          prompt: Pattern
          default: TODO

Entries are also picked up automatically from package.json, Makefile,
Cargo.toml and Gradle builds.

To make `cd` inside a menu entry affect your shell, add the wrapper function:

  # ~/.zshrc
  eval \"$(jj-menu --shell-init zsh)\"
"
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_help_text_shows_a_usable_example() {
        let help = no_entries_help();
        assert!(help.contains("menu:"));
        assert!(help.contains("shell:"));
        assert!(help.contains("parallel:"));
        assert!(help.contains("--shell-init"));
    }

    #[test]
    fn the_example_in_the_help_text_parses() {
        // Keep the sample honest: extract the YAML block and load it.
        let help = no_entries_help();
        let start = help.find("  menu:").expect("the sample starts with menu:");
        let end = help
            .find("\nEntries are")
            .expect("the sample ends before the prose");
        let sample: String = help[start..end]
            .lines()
            .map(|line| line.strip_prefix("  ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");

        let parsed = config::loader::parse(std::path::Path::new("sample.yaml"), &sample)
            .expect("the sample in the help text must parse");
        assert_eq!(parsed.menu.len(), 5);
    }

    /// A directory that exists, so that only the path-building is under test.
    fn existing_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("jj-menu-main-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_script_is_printed_unchanged_when_it_already_runs_here() {
        let here = existing_dir("unchanged");
        assert_eq!(
            in_dir_script(&here, "make build", Some(&here)).unwrap(),
            "make build"
        );
    }

    #[test]
    fn a_script_from_another_directory_is_printed_with_a_cd() {
        let dir = existing_dir("quoted-'-name");
        assert_eq!(
            in_dir_script(&dir, "make build", Some(&dir.join("sub"))).unwrap(),
            format!(
                "cd {}; make build",
                launchers::quote(&dir.to_string_lossy())
            ),
            "the directory is quoted for the shell that will evaluate this"
        );
    }

    #[test]
    fn the_cd_covers_the_whole_script_not_just_its_first_command() {
        // `&&` would bind tighter than the `&`, backgrounding the `cd` along
        // with the server and leaving the next line where the shell already
        // was. Every command of the script has to end up in the directory.
        let dir = existing_dir("whole-script");
        let printed = in_dir_script(&dir, "server &\nnext", Some(&dir.join("sub"))).unwrap();
        assert!(printed.ends_with("; server &\nnext"), "{printed}");
        assert!(!printed.contains("&&"), "{printed}");
    }

    #[test]
    fn refuses_to_print_a_cd_into_a_directory_that_is_gone() {
        // The caller's shell cannot be told "cd, and stop if that failed" in a
        // way bash, zsh and fish all read the same, so a failed `cd` there
        // would run the script in whatever directory the shell was already in.
        let gone = std::env::temp_dir().join("jj-menu-main-no-such-directory");
        let _ = std::fs::remove_dir_all(&gone);
        let err = in_dir_script(&gone, "rm -rf build", Some(std::path::Path::new("/tmp")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("nowhere to run"), "{err}");
        assert!(err.contains(&gone.display().to_string()), "{err}");
    }

    #[test]
    fn prints_the_cd_when_the_callers_directory_is_unknown() {
        // A deleted working directory, which is where `--cwd` earns its keep.
        // Leaving the `cd` out is only ever the shortcut for "you are already
        // there", and there is nothing here saying so.
        let dir = existing_dir("unknown-caller");
        let printed = in_dir_script(&dir, "make build", None).unwrap();
        assert!(printed.starts_with("cd "), "{printed}");
        assert!(printed.ends_with("; make build"), "{printed}");
    }

    #[test]
    fn refuses_to_print_a_cd_into_a_directory_it_cannot_enter() {
        // A directory that still exists but has lost its search bit: `cd`
        // would refuse it, and the `;` would then run the script wherever the
        // caller's shell already was.
        let dir = existing_dir("unsearchable");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root ignores the mode, and then there is nothing to observe.
        let enforced = std::fs::metadata(dir.join(".")).is_err();
        let result = in_dir_script(&dir, "rm -rf build", Some(std::path::Path::new("/tmp")));
        // Restored before asserting, so a failure cannot leave it behind
        // unreadable for the next run.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        if enforced {
            let err = result.unwrap_err().to_string();
            assert!(err.contains("cannot be entered"), "{err}");
        }
    }

    #[test]
    fn refuses_to_print_a_cd_into_a_name_that_is_not_utf8() {
        // Lossily converting it would send the shell to a *different* path,
        // and the `;` would then run the script where the shell already was.
        let dir = existing_dir("utf8").join(OsStr::from_bytes(b"broken-\xff-name"));
        std::fs::create_dir_all(&dir).unwrap();
        let err = in_dir_script(&dir, "rm -rf build", Some(std::path::Path::new("/tmp")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not valid UTF-8"), "{err}");
    }

    #[test]
    fn the_echo_shows_the_script_rather_than_refusing_it() {
        // `Command::current_dir` takes the bytes as they are, so the entry
        // still runs; only the line shown above it loses the directory.
        let dir = existing_dir("utf8").join(OsStr::from_bytes(b"broken-\xff-name"));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            echo_script(&dir, "make build", Some(std::path::Path::new("/tmp"))),
            "make build"
        );
    }

    #[test]
    fn cancelling_uses_the_conventional_interrupt_code() {
        assert_eq!(EXIT_CANCELLED, 130);
    }
}
