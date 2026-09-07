//! Running a `parallel:` group.
//!
//! Every member gets its own `$SHELL -c`, all of them are started before any of
//! them is waited for, and the group is finished when the last one has exited —
//! the same shape as `a & b & wait` in a script.
//!
//! **Each job is put in a process group of its own** (`process_group(0)`), and
//! that is what makes stopping the group work at all:
//!
//! * A signal is sent to the *group* (`kill(-pgid)`), so it reaches the command
//!   the job's shell started, not only the shell. `sh -c "sleep 300"` execs the
//!   sleep, but `sh -c "sleep 300; echo done"` does not — it waits, and a
//!   non-interactive shell does not pass a signal on to what it is waiting for.
//!   Signalling the shell alone would leave that `sleep` running.
//! * The jobs are out of the terminal's foreground group, so a Ctrl-C is
//!   delivered to `j-menu` and to nobody else. It is then passed on exactly
//!   once — a job cannot see the same interrupt twice and mistake it for the
//!   user asking twice.
//!
//! The cost is that the jobs are background groups: they may write to the
//! terminal, but reading from it would stop them with `SIGTTIN`. They are given
//! `/dev/null` on stdin anyway, since several processes taking turns at the
//! keyboard cannot be told apart by whoever is typing. An entry that needs
//! input belongs in a plain `shell:`.
//!
//! A second stop signal escalates to `SIGKILL`, so a job that ignores the first
//! one cannot hold the menu open, and the group is only left once every child
//! has been reaped — `j` cannot return to the prompt with jobs still writing
//! to the terminal behind it.
//!
//! What is *not* followed is a process a job put in the background itself
//! (`something &`, `nohup`) and then walked away from. Once the job's own shell
//! has been reaped there is nothing left to wait on: the group is watched
//! through the child, and waiting on the process group ID instead would both
//! hang on any entry that deliberately starts a daemon and start signalling
//! into a group ID the kernel is free to hand to somebody else. A detached
//! process outliving the thing that started it is what detaching means, and a
//! plain `shell:` entry leaves one behind in exactly the same way.

use std::io::stderr;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Job;
use crate::exec::{exit_code, login_shell};

/// How often the children are checked for having exited.
///
/// The wait is a poll rather than a blocking `wait()` so that a signal is acted
/// on without depending on whether the platform restarts an interrupted wait.
/// 50 ms is below what a person notices and costs nothing measurable.
const POLL: Duration = Duration::from_millis(50);

/// Signals that mean "stop", and are passed on to the jobs.
///
/// All four are what the terminal or an operator uses to end a foreground
/// program. Since the jobs are in their own process groups, none of these
/// reaches them on its own: every one has to be forwarded, `SIGQUIT` included.
const FORWARDED: &[libc::c_int] = &[libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

/// How many stop signals have arrived that the group has not acted on yet.
///
/// A count rather than a flag: two Ctrl-C presses inside one poll interval are
/// the user asking twice, and the second one is what escalates to `SIGKILL`.
static RECEIVED: AtomicUsize = AtomicUsize::new(0);

/// The most recent of those signals, which is the one that gets passed on.
static LAST: AtomicI32 = AtomicI32::new(0);

/// What each of `FORWARDED` was set to before the group armed its handler.
/// `NOT_ARMED` marks a signal that was left alone.
///
/// `usize`, because that is what `sighandler_t` is: a real handler is a
/// function pointer and would not survive a narrower slot.
static INHERITED: [AtomicUsize; 4] = [
    AtomicUsize::new(NOT_ARMED),
    AtomicUsize::new(NOT_ARMED),
    AtomicUsize::new(NOT_ARMED),
    AtomicUsize::new(NOT_ARMED),
];

/// Not a possible `sighandler_t`: `SIG_DFL` is 0, `SIG_ERR` is -1, and a real
/// handler is a function pointer, which is never this small.
const NOT_ARMED: usize = 1;

/// The two are indexed together, so adding a signal has to add a slot.
const _: () = assert!(FORWARDED.len() == INHERITED.len());

/// Status given to a job that could not be waited for, so the group still ends.
/// The wait error itself is what gets reported; this only keeps the exit code
/// from claiming success.
const UNWAITABLE: i32 = 1 << 8;

/// Where the jobs' standard output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Inherit this process' stdout, like any other command the menu runs.
    Inherit,
    /// Write to stderr instead.
    ///
    /// Used with `--print`, where stdout is a command substitution that the
    /// shell wrapper evaluates: output landing there would be *run*, not shown.
    Stderr,
}

/// Run every job at once and return once they have all finished.
///
/// The exit code is the first non-zero one, in the order the jobs are written,
/// so a failure is not swallowed by a later success. A job killed by a signal
/// reports `128 + signal`, which is what makes an interrupted group exit 130.
pub fn run(jobs: &[Job], cwd: &Path, output: Output) -> Result<u8> {
    if jobs.is_empty() {
        return Ok(0);
    }

    let shell = login_shell();
    let mut children = Children::default();

    // Armed before the first spawn: a signal arriving between the first and the
    // last spawn must not be missed, or a Ctrl-C during startup would leave the
    // jobs running.
    arm();
    let spawned = spawn_all(&mut children, jobs, &shell, cwd, output);
    let result = supervise(&mut children, spawned);
    disarm();

    // A stop signal that arrived after the last job was reaped was never acted
    // on. Die of it now that the disposition is back to what it was, so a
    // `SIGTERM` at the wrong moment is not silently swallowed.
    if let Some((_, signal)) = taken() {
        // SAFETY: raising a signal at this process, with the handler disarmed.
        unsafe { libc::raise(signal) };
    }

    result
}

/// See the group through to the end, whether or not all of it started.
///
/// A half-started group is not left to finish on its own: a job that never
/// exits by itself (a dev server, the usual reason to write a group at all)
/// would keep the menu hanging on an error it already knows about. So what did
/// start is stopped first, and only then waited for.
fn supervise(children: &mut Children, spawned: Result<()>) -> Result<u8> {
    let failed_to_start = spawned.is_err();
    if failed_to_start {
        children.signal_all(libc::SIGTERM);
    }
    // Counted as one stop already sent, so a Ctrl-C from the user during the
    // wait escalates to `SIGKILL` rather than repeating the `SIGTERM`.
    let waited = wait_all(children, usize::from(failed_to_start));

    // The spawn failure is the more useful of the two errors: it says which
    // command could not be started at all.
    spawned?;
    waited
}

/// The children of one group, with the status of each once it has been reaped.
///
/// A reaped child is dropped from the signalling loop straight away: its PID
/// belongs to the kernel again and may already name somebody else's process.
/// Until then it is either alive or a zombie, and both keep the PID — and so
/// the process group ID, which is the same number — reserved for us.
#[derive(Default)]
struct Children {
    running: Vec<Child>,
    statuses: Vec<Option<ExitStatus>>,
}

impl Children {
    fn push(&mut self, child: Child) {
        self.running.push(child);
        self.statuses.push(None);
    }

    /// Send `signal` to every job that has not been reaped yet.
    ///
    /// Sent to the whole process group of each job, so it reaches whatever the
    /// job's shell is waiting for as well as the shell itself.
    fn signal_all(&self, signal: libc::c_int) {
        for (child, status) in self.running.iter().zip(&self.statuses) {
            if status.is_some() {
                continue;
            }
            // SAFETY: a plain `kill(2)`. The negative PID addresses the group
            // led by that PID, which was set with `process_group(0)`. The child
            // has not been waited for, so neither the PID nor the group ID can
            // have been handed to anybody else. A failure means it is already
            // gone, which is fine.
            unsafe { libc::kill(-(child.id() as libc::pid_t), signal) };
        }
    }
}

fn spawn_all(
    children: &mut Children,
    jobs: &[Job],
    shell: &str,
    cwd: &Path,
    output: Output,
) -> Result<()> {
    for job in jobs {
        let child = Command::new(shell)
            .arg("-c")
            .arg(&job.script)
            .current_dir(cwd)
            // Its own process group: see the module documentation. Everything
            // about stopping the group depends on this line.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(stdout_for(output)?)
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to run {shell} -c {:?}", job.script))?;
        children.push(child);
    }
    Ok(())
}

/// Wait for every child, passing on any signal that arrives meanwhile.
///
/// `forwarded` is how many stops have already been sent before the wait began.
fn wait_all(children: &mut Children, mut forwarded: usize) -> Result<u8> {
    let mut wait_failed: Option<anyhow::Error> = None;

    loop {
        if let Some((count, signal)) = taken() {
            children.signal_all(escalate(forwarded, count, signal));
            // Added, not incremented: two stops in one interval are two asks.
            forwarded += count;
        }

        let mut alive = false;
        let mut give_up = false;
        for (child, status) in children.running.iter_mut().zip(&mut children.statuses) {
            if status.is_some() {
                continue;
            }
            match child.try_wait() {
                Ok(Some(finished)) => *status = Some(finished),
                Ok(None) => alive = true,
                Err(err) => {
                    // There is no way to learn how this job ended, and polling
                    // it again would only repeat the error. Give up on it, but
                    // take the rest of the group down with it rather than
                    // returning while some of it is still running.
                    *status = Some(ExitStatus::from_raw(UNWAITABLE));
                    wait_failed.get_or_insert_with(|| {
                        anyhow::Error::new(err).context("failed to wait for a parallel job")
                    });
                    give_up = true;
                }
            }
        }
        if give_up {
            children.signal_all(libc::SIGKILL);
            forwarded += 1;
        }
        if !alive {
            return match wait_failed {
                Some(err) => Err(err),
                None => Ok(group_exit_code(&children.statuses)),
            };
        }

        std::thread::sleep(POLL);
    }
}

/// The signal to pass on to the jobs.
///
/// The first stop is passed on as it arrived, so a job sees the `SIGINT` or
/// `SIGTERM` it would have got from the terminal and can clean up after itself.
/// Anything after that is the user asking again, and gets `SIGKILL` — including
/// a second stop that landed in the same poll interval as the first, which is
/// why `count` is looked at and not just `forwarded`.
fn escalate(forwarded: usize, count: usize, signal: libc::c_int) -> libc::c_int {
    if forwarded == 0 && count == 1 {
        signal
    } else {
        libc::SIGKILL
    }
}

/// The code the group reports: the first failure, or 0.
fn group_exit_code(statuses: &[Option<ExitStatus>]) -> u8 {
    statuses
        .iter()
        .flatten()
        .map(|status| exit_code(*status))
        .find(|code| *code != 0)
        .unwrap_or(0)
}

fn stdout_for(output: Output) -> Result<Stdio> {
    match output {
        Output::Inherit => Ok(Stdio::inherit()),
        Output::Stderr => {
            let cloned: OwnedFd = stderr()
                .as_fd()
                .try_clone_to_owned()
                .context("failed to duplicate stderr for a parallel job")?;
            Ok(Stdio::from(cloned))
        }
    }
}

/// Record a stop signal instead of dying of it, so the jobs can be dealt with
/// first.
extern "C" fn note(signal: libc::c_int) {
    // Atomic stores are on the POSIX list of what a handler may call; nothing
    // else here is.
    LAST.store(signal, Ordering::SeqCst);
    RECEIVED.fetch_add(1, Ordering::SeqCst);
}

/// The stops that have arrived since the last look: how many, and the signal
/// of the most recent one.
///
/// The count is part of the answer, not a detail. Two Ctrl-C presses inside one
/// poll interval arrive as one wake-up, and dropping the count would turn the
/// second press — the one that escalates to `SIGKILL` — into nothing.
fn taken() -> Option<(usize, libc::c_int)> {
    match RECEIVED.swap(0, Ordering::SeqCst) {
        0 => None,
        count => Some((count, LAST.load(Ordering::SeqCst))),
    }
}

fn arm() {
    RECEIVED.store(0, Ordering::SeqCst);
    for (slot, &signal) in FORWARDED.iter().enumerate() {
        // SAFETY: `note` is an `extern "C"` function that only stores to
        // atomics, which is what `signal` requires of a handler. The call hands
        // back the disposition it replaced.
        let inherited = unsafe { libc::signal(signal, note as *const () as libc::sighandler_t) };
        if inherited == libc::SIG_IGN {
            // The caller asked for this signal to be ignored. Honouring that
            // matters: the ignore is inherited by the jobs, so passing the
            // signal on would stop processes that were meant to survive it.
            // SAFETY: putting back the disposition just read.
            unsafe { libc::signal(signal, libc::SIG_IGN) };
            continue;
        }
        INHERITED[slot].store(inherited, Ordering::Release);
    }
}

fn disarm() {
    for (slot, &signal) in FORWARDED.iter().enumerate() {
        let inherited = INHERITED[slot].swap(NOT_ARMED, Ordering::AcqRel);
        if inherited == NOT_ARMED {
            continue;
        }
        // SAFETY: putting back the disposition that was read from this same
        // signal while arming.
        unsafe { libc::signal(signal, inherited) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The signal handlers are process-wide, and one of the tests below raises
    /// a real signal at this process, so these tests cannot overlap.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn job(script: &str) -> Job {
        Job {
            title: script.to_string(),
            script: script.to_string(),
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("j-menu-parallel-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn runs_the_jobs_at_the_same_time() {
        let _serial = serial();
        let cwd = dir("concurrent");
        // Each job waits for a file the other one writes, so running them one
        // after the other cannot finish: the first would wait for a file that
        // is only written by a job that has not started. The wait is bounded so
        // a sequential implementation fails the test instead of hanging it.
        let wait_for = |name: &str| {
            format!(
                "i=0; until [ -f {name} ]; do i=$((i+1)); [ $i -lt 200 ] || exit 1; sleep 0.05; done"
            )
        };
        let jobs = vec![
            job(&format!("touch a; {}", wait_for("b"))),
            job(&format!("touch b; {}", wait_for("a"))),
        ];

        assert_eq!(run(&jobs, &cwd, Output::Inherit).unwrap(), 0);
    }

    #[test]
    fn returns_only_once_every_job_has_finished() {
        let _serial = serial();
        let cwd = dir("finish");
        let jobs = vec![job("sleep 0.2; touch slow"), job("true")];

        assert_eq!(run(&jobs, &cwd, Output::Inherit).unwrap(), 0);
        assert!(
            cwd.join("slow").exists(),
            "the group was left before the slowest job was done"
        );
    }

    #[test]
    fn reports_the_first_failure() {
        let _serial = serial();
        let cwd = dir("failure");
        let jobs = vec![job("true"), job("exit 3"), job("exit 4")];

        assert_eq!(run(&jobs, &cwd, Output::Inherit).unwrap(), 3);
    }

    #[test]
    fn reports_success_when_every_job_succeeds() {
        let _serial = serial();
        let cwd = dir("success");
        let jobs = vec![job("true"), job("true")];

        assert_eq!(run(&jobs, &cwd, Output::Inherit).unwrap(), 0);
    }

    #[test]
    fn every_job_gets_a_process_group_of_its_own() {
        let _serial = serial();
        let cwd = dir("pgid");
        // `$$` is the job's shell, and its process group is itself only when
        // `process_group(0)` took effect. Without it the shell would report the
        // group of the test binary, and a `kill(-pgid)` from here would signal
        // the whole test run rather than the job.
        let jobs = vec![job("test \"$(ps -o pgid= -p $$ | tr -d ' ')\" = \"$$\"")];

        assert_eq!(
            run(&jobs, &cwd, Output::Inherit).unwrap(),
            0,
            "a job must lead its own process group"
        );
    }

    #[test]
    fn an_interrupt_stops_every_job() {
        let _serial = serial();
        let cwd = dir("interrupt");
        // `$PPID` inside the job is this process: the job's shell was forked
        // from the test binary. So this is a real Ctrl-C arriving while a group
        // is running — safe to raise here precisely because the handler under
        // test records it rather than letting it kill anything. The jobs are in
        // their own process groups, so nothing but the forwarding can stop them.
        let jobs = vec![
            job("kill -INT $PPID; sleep 30"),
            job("sleep 30"),
            job("sleep 30"),
        ];

        let started = std::time::Instant::now();
        let code = run(&jobs, &cwd, Output::Inherit).unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the jobs outlived the interrupt"
        );
        assert_eq!(
            code, 130,
            "an interrupted group exits like a killed command"
        );
    }

    #[test]
    fn an_interrupt_reaches_what_a_job_is_waiting_for() {
        let _serial = serial();
        let cwd = dir("grandchild");
        // The shell does not exec here — it waits for `sleep`, and a
        // non-interactive shell passes nothing on to what it waits for. Only a
        // signal sent to the process group reaches that `sleep`, and the marker
        // file is what proves it never got to run to completion.
        let jobs = vec![job("kill -INT $PPID; sleep 20; touch survived")];

        let started = std::time::Instant::now();
        let code = run(&jobs, &cwd, Output::Inherit).unwrap();

        // The elapsed time is the assertion that matters: signalling only the
        // shell leaves the `sleep` running, and the shell then sits there
        // waiting for it — the group takes the full twenty seconds even though
        // nothing writes the marker file afterwards.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the command the job's shell was waiting for outlived the interrupt"
        );
        assert!(
            !cwd.join("survived").exists(),
            "the interrupted job ran to the end"
        );
        assert_ne!(code, 0, "an interrupted job did not succeed");
    }

    #[test]
    fn a_second_interrupt_kills_a_job_that_ignored_the_first() {
        let _serial = serial();
        let cwd = dir("escalate");
        // The job ignores `SIGINT` — and so does the `sleep` it execs, since an
        // ignored disposition survives exec — and then asks for two of them,
        // far enough apart to be two separate polls. Nothing but the escalation
        // to `SIGKILL` can end this before the half minute is up.
        let jobs = vec![job(
            "trap '' INT; kill -INT $PPID; sleep 0.5; kill -INT $PPID; sleep 30",
        )];

        let started = std::time::Instant::now();
        let code = run(&jobs, &cwd, Output::Inherit).unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the second interrupt did not escalate"
        );
        assert_eq!(code, 137, "a killed job reports 128 + SIGKILL");
    }

    #[test]
    fn two_stops_in_one_interval_kill_a_job_that_ignores_them() {
        let _serial = serial();
        let cwd = dir("escalate-pair");
        // The pair of presses staged where the test above cannot reach: both
        // are raised before the wait takes its first look, so the group has to
        // notice that it was asked twice from a single wake-up. `raise` runs
        // the handler before it returns, which is what makes this exact rather
        // than a race against the poll interval.
        let shell = login_shell();
        let mut children = Children::default();
        arm();
        spawn_all(
            &mut children,
            &[job("trap '' INT; touch ready; sleep 30")],
            &shell,
            &cwd,
            Output::Inherit,
        )
        .expect("the job starts");
        wait_for(&cwd.join("ready"));

        // SAFETY: raising a signal at this process with the handler armed.
        unsafe {
            libc::raise(libc::SIGINT);
            libc::raise(libc::SIGINT);
        }
        let started = std::time::Instant::now();
        let code = wait_all(&mut children, 0);
        disarm();

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the second of the two stops was dropped"
        );
        assert_eq!(code.unwrap(), 137, "a killed job reports 128 + SIGKILL");
    }

    /// Block until `marker` exists, so a job's setup is not raced.
    fn wait_for(marker: &std::path::Path) {
        for _ in 0..500 {
            if marker.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the job never got as far as {}", marker.display());
    }

    #[test]
    fn two_stops_before_the_next_look_are_counted_as_two() {
        let _serial = serial();
        // The pair of Ctrl-C presses the test above cannot stage: `raise` runs
        // the handler before it returns, so both are seen with no poll in
        // between. Dropping the count here is what would turn the second press
        // into nothing.
        arm();
        // SAFETY: raising a signal at this process while the handler that
        // records it — rather than dying of it — is armed.
        unsafe {
            libc::raise(libc::SIGINT);
            libc::raise(libc::SIGINT);
        }
        let seen = taken();
        disarm();

        assert_eq!(seen, Some((2, libc::SIGINT)));
    }

    #[test]
    fn only_the_first_stop_is_passed_on_as_it_arrived() {
        assert_eq!(escalate(0, 1, libc::SIGINT), libc::SIGINT);
        assert_eq!(escalate(0, 1, libc::SIGTERM), libc::SIGTERM);
        // Asked twice, whether or not the two landed in the same interval.
        assert_eq!(escalate(0, 2, libc::SIGINT), libc::SIGKILL);
        assert_eq!(escalate(1, 1, libc::SIGINT), libc::SIGKILL);
    }

    #[test]
    fn puts_the_signal_handlers_back_afterwards() {
        let _serial = serial();
        let cwd = dir("disarm");
        assert_eq!(run(&[job("true")], &cwd, Output::Inherit).unwrap(), 0);

        for &signal in FORWARDED {
            // SAFETY: reading a disposition by replacing it and putting the
            // answer straight back, so the test leaves nothing changed.
            let current = unsafe { libc::signal(signal, libc::SIG_DFL) };
            unsafe { libc::signal(signal, current) };
            assert_eq!(
                current,
                libc::SIG_DFL,
                "signal {signal} was left pointing at the group handler"
            );
        }
    }

    #[test]
    fn print_mode_keeps_the_jobs_output_off_stdout() {
        use std::os::fd::AsRawFd;

        let _serial = serial();
        let cwd = dir("print");
        let out = std::fs::File::create(cwd.join("stdout")).unwrap();
        let err = std::fs::File::create(cwd.join("stderr")).unwrap();

        // Stand in for the shell wrapper: with `--print`, stdout is a command
        // substitution whose contents get evaluated, so a job writing there
        // would be run as a command. Both descriptors are swapped for files,
        // which is what the jobs inherit.
        // SAFETY: plain `dup`/`dup2` on descriptors this test owns; the
        // originals are put back below, and `SERIAL` keeps the other tests in
        // this module out. It does not keep libtest's reporter out: that runs
        // on the harness thread and writes its `test ... ok` lines to fd 1
        // while it is swapped, so whatever lands in `stdout` is checked for
        // the job's output rather than for being empty.
        let (saved_out, saved_err) = unsafe { (libc::dup(1), libc::dup(2)) };
        unsafe {
            libc::dup2(out.as_raw_fd(), 1);
            libc::dup2(err.as_raw_fd(), 2);
        }
        let code = run(&[job("echo from-the-job")], &cwd, Output::Stderr);
        unsafe {
            libc::dup2(saved_out, 1);
            libc::dup2(saved_err, 2);
            libc::close(saved_out);
            libc::close(saved_err);
        }

        assert_eq!(code.unwrap(), 0);
        let on_stdout = std::fs::read_to_string(cwd.join("stdout")).unwrap();
        assert!(
            !on_stdout.contains("from-the-job"),
            "output on stdout would be evaluated by the shell wrapper: {on_stdout:?}"
        );
        // Matched exactly, so that a job's output being repeated or joined by
        // a stray diagnostic is caught: the reporter writes to fd 1 only, so
        // nothing but the job reaches this one. Should that ever change, this
        // is the assertion that will start failing for the reason above.
        assert_eq!(
            std::fs::read_to_string(cwd.join("stderr")).unwrap(),
            "from-the-job\n",
            "the output still has to reach the terminal"
        );
    }

    #[test]
    fn a_group_that_could_not_be_started_in_full_stops_what_did_start() {
        let _serial = serial();
        let cwd = dir("spawn-failure");
        // A spawn failure part-way through a group cannot be provoked from the
        // outside — the shell and the working directory are the same for every
        // job, so they fail for all of them or for none — so the failure is
        // handed to the code that has to cope with it. Waiting for this job to
        // finish on its own would take 30 seconds; it has to be stopped.
        let mut children = Children::default();
        let shell = login_shell();
        arm();
        spawn_all(
            &mut children,
            &[job("sleep 30; touch survived")],
            &shell,
            &cwd,
            Output::Inherit,
        )
        .expect("the first job starts");

        let started = std::time::Instant::now();
        let result = supervise(&mut children, Err(anyhow::anyhow!("could not start job 2")));
        disarm();

        assert!(result.is_err(), "the failing spawn must be reported");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the group waited for a job it should have stopped"
        );
        assert!(
            !cwd.join("survived").exists(),
            "the job that did start outlived the failure"
        );
    }

    #[test]
    fn a_group_with_nothing_in_it_succeeds() {
        let _serial = serial();
        assert_eq!(run(&[], &std::env::temp_dir(), Output::Inherit).unwrap(), 0);
    }
}
