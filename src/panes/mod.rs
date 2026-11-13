//! Running a `parallel:` group the way `parun` does: the screen is split into
//! one pane per command, each showing the tail of what its command writes.
//!
//! The modules under here are taken from cyberneura/parun with as few changes
//! as possible, so that a fix on either side can be carried over:
//!
//! * the screen is drawn on **stderr**, not stdout — with `--print` stdout is
//!   a command substitution that the shell wrapper evaluates;
//! * a command runs in `$SHELL -c`, like every other entry of the menu, rather
//!   than parun's `sh -c`;
//! * the Windows branches are gone, since j-menu is Unix only;
//! * the summary that follows the screen is written here, on stderr, and the
//!   exit code follows `parallel-merged`: the first failure in the order the
//!   commands are written.
//!
//! The commands' output goes into the panes and nowhere else, so nothing a
//! command writes can reach the wrapper's command substitution either.

mod process;
mod runner;
mod screen;
mod terminal;
mod text;

use std::io::{Write, stderr};
use std::path::Path;

use anyhow::Result;

use crate::config::Job;
use runner::{JobResult, Outcome};
use text::{display_width, pad_to_width};

/// Lines of a failed command's output printed after the screen has gone: the
/// panes were the only place it was shown, and the reason for the failure is
/// usually at the end.
const FAILURE_TAIL_LINES: usize = 20;

/// Code reported for a command that never ran (it could not be started, or
/// the group was stopped before it got to it).
const NOT_RUN: u8 = 1;

/// Code reported for a group stopped with Ctrl-C whose commands all exited 0
/// regardless, so that an interrupted group never looks like a clean run.
const INTERRUPTED: u8 = 130;

/// Run every job at once, each in a pane of its own, and return once they have
/// all finished and a key has been pressed.
pub fn run(jobs: &[Job], cwd: &Path) -> Result<u8> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let pane_jobs: Vec<runner::Job> = jobs
        .iter()
        .map(|job| runner::Job {
            name: job.title.clone(),
            command: job.script.clone(),
            directory: Some(cwd.to_path_buf()),
        })
        .collect();
    let close_after = jobs.iter().map(|job| job.close_after).collect();

    terminal::stop_commands_on_hangup()?;
    let outcome = match screen::run(pane_jobs.clone(), pane_jobs.len(), close_after) {
        Ok(outcome) => outcome,
        Err(error) => {
            terminal::wait_for_signal_exit();
            return Err(error);
        }
    };
    // The commands may have ended because a signal is ending the process: the
    // summary is not wanted then, and the signal is what has to end it.
    terminal::wait_for_signal_exit();

    let mut out = Vec::new();
    write_summary(&mut out, &pane_jobs, &outcome.results);
    write_failure_tails(&mut out, &pane_jobs, &outcome.results);
    if outcome.aborted {
        let not_started = outcome.results.iter().filter(|r| r.is_none()).count();
        if not_started > 0 {
            let _ = writeln!(
                out,
                "\naborted: {not_started} command{} never started",
                plural(not_started)
            );
        } else {
            let _ = writeln!(out, "\naborted");
        }
    }
    // Nothing is left to stop if the terminal has gone, so a failed write is
    // not worth an error of its own.
    let _ = stderr().write_all(&out);

    Ok(exit_code(&outcome.results, outcome.aborted))
}

/// The code the group reports: the first failure in the order the jobs are
/// written, or 0 — and never 0 for a group the user stopped.
fn exit_code(results: &[Option<JobResult>], aborted: bool) -> u8 {
    let first_failure = results
        .iter()
        .map(|result| match result {
            None => NOT_RUN,
            Some(result) => match &result.outcome {
                Outcome::Succeeded => 0,
                Outcome::Failed(status) => crate::exec::exit_code(*status),
                Outcome::NotRun(_) => NOT_RUN,
            },
        })
        .find(|code| *code != 0);
    match first_failure {
        Some(code) => code,
        None if aborted => INTERRUPTED,
        None => 0,
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn write_summary(out: &mut Vec<u8>, jobs: &[runner::Job], results: &[Option<JobResult>]) {
    let _ = writeln!(out, "\nSummary");
    let name_width = jobs
        .iter()
        .map(|job| display_width(&job.name))
        .max()
        .unwrap_or(0);
    let rows: Vec<(String, String, String)> = results
        .iter()
        .map(|result| match result {
            None => ("not started".to_owned(), "-".to_owned(), String::new()),
            Some(result) => {
                let note = match &result.outcome {
                    Outcome::NotRun(message) => format!("  ({message})"),
                    _ => String::new(),
                };
                (
                    result.outcome.label(),
                    screen::format_duration(result.elapsed),
                    note,
                )
            }
        })
        .collect();
    // Wide enough for the widest label present, `terminated by signal` say,
    // rather than a fixed width that the odd one would push out of line.
    let status_width = rows
        .iter()
        .map(|(status, ..)| status.len())
        .max()
        .unwrap_or(0);
    for (job, (status, elapsed, note)) in jobs.iter().zip(rows) {
        // The name came from a configuration file in the checkout, and the
        // summary is written outside the panes, where nothing strips it.
        let name = text::plain_text(&job.name);
        let mut line = format!(
            "  {status:<status_width$} {elapsed:>7}  {}",
            pad_to_width(&name, name_width)
        );
        line.push_str(&note);
        let _ = writeln!(out, "{}", line.trim_end());
    }
}

/// The tail of each failed command's output, printed once the panes that
/// showed it are gone.
fn write_failure_tails(out: &mut Vec<u8>, jobs: &[runner::Job], results: &[Option<JobResult>]) {
    for (job, result) in jobs.iter().zip(results) {
        let Some(result) = result else {
            continue;
        };
        if !matches!(result.outcome, Outcome::Failed(_)) || result.output.is_empty() {
            continue;
        }
        let lines: Vec<&str> = result.output.lines().collect();
        let skip = lines.len().saturating_sub(FAILURE_TAIL_LINES);
        let shown = lines.len() - skip;
        let _ = writeln!(
            out,
            "\n\u{2500}\u{2500} {}: last {shown} line{} of output",
            text::plain_text(&job.name),
            plural(shown)
        );
        for line in &lines[skip..] {
            // Printed as the command wrote it, the way it would have reached
            // the terminal without the panes in between.
            let _ = writeln!(out, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::time::Duration;

    fn result(outcome: Outcome) -> Option<JobResult> {
        Some(JobResult {
            outcome,
            output: String::new(),
            elapsed: Duration::ZERO,
        })
    }

    fn exited(code: i32) -> Outcome {
        Outcome::Failed(ExitStatus::from_raw(code << 8))
    }

    #[test]
    fn exits_with_the_first_failure_in_the_order_written() {
        let results = vec![
            result(Outcome::Succeeded),
            result(exited(3)),
            result(exited(4)),
        ];
        assert_eq!(exit_code(&results, false), 3);
    }

    #[test]
    fn exits_zero_when_every_command_succeeded() {
        let results = vec![result(Outcome::Succeeded), result(Outcome::Succeeded)];
        assert_eq!(exit_code(&results, false), 0);
    }

    #[test]
    fn a_command_killed_by_a_signal_reports_128_plus_the_signal() {
        let results = vec![result(Outcome::Failed(ExitStatus::from_raw(libc::SIGINT)))];
        assert_eq!(exit_code(&results, true), 130);
    }

    #[test]
    fn a_stopped_group_never_reports_success() {
        let results = vec![result(Outcome::Succeeded), result(Outcome::Succeeded)];
        assert_eq!(exit_code(&results, true), INTERRUPTED);
    }

    #[test]
    fn a_command_that_never_ran_is_a_failure() {
        assert_eq!(exit_code(&[None], true), NOT_RUN);
        assert_eq!(
            exit_code(&[result(Outcome::NotRun("no shell".into()))], false),
            NOT_RUN
        );
    }

    #[test]
    fn the_summary_names_every_command_with_its_outcome() {
        let jobs = vec![
            runner::Job {
                name: "web".into(),
                command: "npm run dev".into(),
                directory: None,
            },
            runner::Job {
                name: "api\u{1b}[31m".into(),
                command: "npm run api".into(),
                directory: None,
            },
        ];
        let mut failed = result(exited(2)).unwrap();
        failed.output = (1..=30).map(|n| format!("line {n}\n")).collect();
        let results = vec![result(Outcome::Succeeded), Some(failed)];

        let mut out = Vec::new();
        write_summary(&mut out, &jobs, &results);
        write_failure_tails(&mut out, &jobs, &results);
        let text = String::from_utf8(out).unwrap();

        assert!(text.contains("ok"), "{text}");
        assert!(text.contains("exit 2"), "{text}");
        assert!(
            !text.contains('\u{1b}'),
            "a name's escapes are dropped: {text}"
        );
        assert!(text.contains("api: last 20 lines of output"), "{text}");
        assert!(
            !text.contains("line 10\n"),
            "only the tail is printed: {text}"
        );
        assert!(text.contains("line 30\n"), "{text}");
    }
}
