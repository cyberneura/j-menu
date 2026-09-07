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

    let start_dir = match args.cwd {
        Some(dir) => dir,
        None => std::env::current_dir().context("failed to read the working directory")?,
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
        ui::Outcome::Run(Launch::Script(script)) => {
            if args.print {
                let mut out = stdout();
                writeln!(out, "{script}")?;
                out.flush()?;
                return Ok(ExitCode::SUCCESS);
            }

            echo("$", &script)?;
            let status = exec::run(&script, &start_dir)?;
            // Pass the command's exit code through, so `jj && next` and `$?`
            // behave the way they would for a typed command.
            Ok(ExitCode::from(exec::exit_code(status)))
        }
        ui::Outcome::Run(Launch::Parallel(jobs)) => {
            // Run here even under `--print`. There is nothing to hand back to
            // the calling shell: the point of `--print` is that `cd` and
            // `export` reach the user's shell, and a group is several separate
            // processes, none of which could change it anyway. Printing them
            // as one script would mean writing the `&`-and-`wait` form of
            // whichever shell is calling — and fish, which the wrapper
            // supports, does not have it.
            for job in &jobs {
                echo("&", &job.script)?;
            }
            // With `--print` the wrapper is reading stdout through a command
            // substitution and evaluates whatever comes back; a job writing
            // there would be *run*, not shown. Send it to the terminal instead.
            let output = if args.print {
                parallel::Output::Stderr
            } else {
                parallel::Output::Inherit
            };
            Ok(ExitCode::from(parallel::run(&jobs, &start_dir, output)?))
        }
    }
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
        for path in &config.sources {
            println!("  {}", path.display());
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

    #[test]
    fn cancelling_uses_the_conventional_interrupt_code() {
        assert_eq!(EXIT_CANCELLED, 130);
    }
}
