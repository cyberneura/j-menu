//! Shell integration snippets.
//!
//! Running a command as a child process cannot change the calling shell, so
//! entries like `cd /tmp` or `export FOO=1` have no lasting effect. The
//! wrapper function works around that: `--print` writes the chosen command to
//! stdout instead of running it, and the function evaluates it in the current
//! shell. It also puts the command into the shell history, so the usual
//! recall and edit workflow keeps working.

/// Shells with a ready-made snippet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
}

impl std::str::FromStr for ShellKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "bash" => Ok(ShellKind::Bash),
            "zsh" => Ok(ShellKind::Zsh),
            "fish" => Ok(ShellKind::Fish),
            other => Err(format!(
                "unknown shell: {other} (expected bash, zsh or fish)"
            )),
        }
    }
}

/// Single-quote a path for `sh`, `bash`, `zsh` and `fish` alike: everything
/// inside single quotes is literal in all four, and the quote itself is the
/// only character that has to be spliced in from outside them.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The command word the snippet uses to call back into this program.
///
/// A binary reached through `PATH` stays `command jj-menu`: that survives the
/// binary being moved or upgraded and reads as intended in a startup file. One
/// invoked by a path -- `~/src/jj-menu/target/release/jj-menu --shell-init
/// zsh` -- is not on `PATH` under that name, so the running executable's own
/// path is quoted in instead; `command jj-menu` would otherwise define `jj` as
/// a call to a command the shell cannot find.
pub fn program() -> String {
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if !std::path::Path::new(&argv0).to_string_lossy().contains('/') {
        return "command jj-menu".to_string();
    }
    let path = std::env::current_exe().unwrap_or_else(|_| argv0.into());
    format!("command {}", shell_quote(&path.to_string_lossy()))
}

/// The snippet to add to the shell's startup file.
///
/// `program` is the already shell-quoted command the function calls, from
/// [`program`].
pub fn snippet(kind: ShellKind, program: &str) -> String {
    match kind {
        // `local` keeps the variable out of the interactive shell, and the
        // status check makes a cancelled menu (exit 130) a no-op.
        ShellKind::Bash => format!(
            r#"jj() {{
  local __jj_command
  __jj_command="$({program} --print "$@")" || return $?
  [ -n "$__jj_command" ] || return 0
  history -s "$__jj_command"
  eval "$__jj_command"
}}
"#
        ),
        ShellKind::Zsh => format!(
            r#"jj() {{
  local __jj_command
  __jj_command="$({program} --print "$@")" || return $?
  [ -n "$__jj_command" ] || return 0
  print -s -- "$__jj_command"
  eval "$__jj_command"
}}
"#
        ),
        // fish splits an *unquoted* command substitution on newlines, which
        // would turn a multi-line entry into several arguments. The quoted
        // form `"$(...)"` keeps it as one string (fish 3.4+).
        ShellKind::Fish => format!(
            r#"function jj
    set -l __jj_command "$({program} --print $argv)"
    set -l __jj_status $status
    test $__jj_status -eq 0; or return $__jj_status
    test -n "$__jj_command"; or return 0
    commandline -r -- "$__jj_command"
    commandline -f execute
end
"#
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_shell_names_case_insensitively() {
        assert_eq!("bash".parse::<ShellKind>().unwrap(), ShellKind::Bash);
        assert_eq!("ZSH".parse::<ShellKind>().unwrap(), ShellKind::Zsh);
        assert_eq!("Fish".parse::<ShellKind>().unwrap(), ShellKind::Fish);
        assert!("tcsh".parse::<ShellKind>().is_err());
    }

    #[test]
    fn every_snippet_defines_a_jj_entry_point() {
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            let text = snippet(kind, "command jj-menu");
            assert!(text.contains("jj"), "{kind:?}");
            assert!(text.contains("--print"), "{kind:?}");
        }
    }

    #[test]
    fn the_fish_snippet_keeps_a_multi_line_command_in_one_piece() {
        // An unquoted command substitution splits on newlines, so a `shell:`
        // list would arrive as several arguments.
        let fish = snippet(ShellKind::Fish, "command jj-menu");
        assert!(
            fish.contains(r#""$(command jj-menu --print $argv)""#),
            "the command substitution must be quoted: {fish}"
        );
        assert!(
            !fish.contains("(command jj-menu --print $argv)\n"),
            "no bare command substitution may remain: {fish}"
        );
        assert!(
            fish.contains(r#"commandline -r -- "$__jj_command""#),
            "the variable must be quoted when used: {fish}"
        );
    }

    #[test]
    fn every_snippet_stops_when_the_menu_was_dismissed() {
        // Cancelling exits non-zero and prints nothing; neither must lead to
        // evaluating an empty command.
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            let text = snippet(kind, "command jj-menu");
            assert!(text.contains("return"), "{kind:?}");
            assert!(text.contains("-n "), "{kind:?}: no empty check");
        }
    }

    #[test]
    fn snippets_call_the_binary_not_the_function_itself() {
        // Without `command`, the function would call itself in bash and zsh.
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            assert!(
                snippet(kind, "command jj-menu").contains("command jj-menu"),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_binary_outside_path_is_called_by_its_quoted_path() {
        // The path is what the whole feature is for: a snippet that still says
        // `jj-menu` would define `jj` as a command the shell cannot find.
        let program = format!("command {}", shell_quote("/opt/it's here/jj-menu"));
        let zsh = snippet(ShellKind::Zsh, &program);
        assert!(
            zsh.contains(r#"command '/opt/it'\''s here/jj-menu' --print"#),
            "{zsh}"
        );
        assert!(!zsh.contains("command jj-menu"), "{zsh}");
    }

    #[test]
    fn shell_quoting_closes_and_reopens_around_a_quote() {
        assert_eq!(shell_quote("/plain/path"), "'/plain/path'");
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }
}
