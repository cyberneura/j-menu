//! Running the selected command.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result};

/// Run `script` with the TTY fully attached.
///
/// stdin, stdout and stderr are inherited, and the process is not put in a
/// pipeline, so interactive programs work exactly as if the command had been
/// typed at the prompt: `ssh` can read a password, `vim` gets its terminal,
/// job control and window-resize signals keep working.
///
/// The menu must have restored the terminal before this is called; it is drawn
/// on the alternate screen in raw mode, and neither would be a sane starting
/// point for the child.
pub fn run(script: &str, cwd: &Path) -> Result<ExitStatus> {
    let shell = login_shell();
    Command::new(&shell)
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("failed to run {shell} -c {script:?}"))
}

/// The shell used to interpret a menu command. Shared with
/// [`crate::parallel`], which starts one of these per job.
///
/// `$SHELL` is what the user chose, so their aliases-free but familiar syntax
/// (zsh globs, bash arrays) works. `/bin/sh` is the fallback because it is the
/// one shell POSIX guarantees exists.
pub fn login_shell() -> String {
    shell_from(std::env::var("SHELL").ok())
}

/// Split out from [`login_shell`] so the fallback can be tested without
/// mutating the environment, which would race with the other tests in this
/// binary (they run on parallel threads and read `$SHELL` too).
fn shell_from(env_value: Option<String>) -> String {
    env_value
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// The exit code to report for a finished command, the way a POSIX shell
/// would: the command's own code, or `128 + signal` when it was killed.
pub fn exit_code(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code.clamp(0, 255) as u8;
    }
    // No exit code means a signal ended it. `128 + n` is what `$?` holds in
    // bash/zsh, so `j` stays interchangeable with typing the command.
    match status.signal() {
        Some(signal) => 128u8.saturating_add(signal.clamp(0, 127) as u8),
        // Neither an exit code nor a signal should be possible for a process
        // that has been waited on; report a generic failure rather than 0.
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_command_and_reports_its_exit_status() {
        let cwd = std::env::temp_dir();
        assert!(run("exit 0", &cwd).unwrap().success());
        assert!(!run("exit 3", &cwd).unwrap().success());
        assert_eq!(run("exit 3", &cwd).unwrap().code(), Some(3));
    }

    #[test]
    fn runs_in_the_given_directory() {
        let cwd = std::env::temp_dir();
        let status = run(
            &format!("test \"$(pwd -P)\" = \"$(cd {} && pwd -P)\"", cwd.display()),
            &cwd,
        )
        .unwrap();
        assert!(status.success());
    }

    #[test]
    fn reports_the_exit_code_of_the_command() {
        let cwd = std::env::temp_dir();
        assert_eq!(exit_code(run("exit 0", &cwd).unwrap()), 0);
        assert_eq!(exit_code(run("exit 42", &cwd).unwrap()), 42);
    }

    #[test]
    fn reports_128_plus_signal_for_a_killed_command() {
        let cwd = std::env::temp_dir();
        // SIGTERM is 15 everywhere this runs, so the shell reports 143.
        let status = run("kill -TERM $$", &cwd).unwrap();
        assert_eq!(exit_code(status), 143, "status was {status:?}");
    }

    #[test]
    fn falls_back_to_sh_without_a_usable_shell_setting() {
        assert_eq!(shell_from(None), "/bin/sh");
        assert_eq!(shell_from(Some(String::new())), "/bin/sh");
        assert_eq!(shell_from(Some("/bin/zsh".into())), "/bin/zsh");
    }
}
