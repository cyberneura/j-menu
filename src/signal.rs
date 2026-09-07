//! Putting the terminal back when the process is killed.
//!
//! [`crate::ui::TerminalGuard`] covers every exit Rust controls — a return, an
//! error, a panic — because `Drop` runs on all of them. A default-terminating
//! signal is the one that it cannot cover: the process dies inside the kernel,
//! nothing unwinds, and the shell is left in raw mode with the cursor hidden
//! and the alternate screen still on. `kill`, a logout, and a window manager
//! closing the terminal all take that path.
//!
//! So the terminal settings are captured before raw mode is entered and put
//! back from a signal handler. Only async-signal-safe calls are used there:
//! `tcsetattr`, `write` and `raise` are on the POSIX list, whereas crossterm's
//! own teardown takes a lock and could deadlock against the code the signal
//! interrupted.
//!
//! The handler restores the default disposition and re-raises, so the process
//! still dies of the signal it was sent — the exit status a caller sees is the
//! one it would have seen anyway.

use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// The terminal settings from before raw mode, or null while the menu is not
/// open. Read from a signal handler, so it is an atomic pointer to a leaked
/// allocation rather than anything that needs locking or dropping.
static ORIGINAL: AtomicPtr<libc::termios> = AtomicPtr::new(ptr::null_mut());

/// Signals that terminate by default and can arrive while the menu is up.
///
/// `SIGINT` and `SIGQUIT` are included for the case where the menu is killed
/// from another terminal: raw mode turns off the keyboard's own Ctrl-C and
/// Ctrl-\, so they cannot come from the user typing at the menu.
///
/// `SIGPIPE` is deliberately absent. Rust ignores it so that a closed pipe
/// surfaces as an `EPIPE` error, and handling it here would turn
/// `j-menu --print | head` from a clean write error into a killed process.
const TERMINATING: &[libc::c_int] = &[libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

/// What each of `TERMINATING` was set to before the menu armed its handler,
/// so that disarming puts back what was inherited rather than assuming the
/// default. A shell running `trap '' TERM` passes its ignore down, and turning
/// that into the default would change how the command the menu runs behaves.
///
/// `NOT_ARMED` marks a slot the menu did not take over.
static INHERITED: [AtomicUsize; 4] = [
    AtomicUsize::new(NOT_ARMED),
    AtomicUsize::new(NOT_ARMED),
    AtomicUsize::new(NOT_ARMED),
    AtomicUsize::new(NOT_ARMED),
];

/// Not a possible `sighandler_t`: `SIG_DFL` is 0, `SIG_ERR` is -1, and a real
/// handler is a function pointer.
const NOT_ARMED: usize = 1;

/// The two are indexed together, so adding a signal has to add a slot.
const _: () = assert!(TERMINATING.len() == INHERITED.len());

/// Capture the terminal state and arm the handlers.
///
/// Must be called *before* raw mode is entered, since what it captures is
/// exactly the state to go back to. Does nothing when stderr is not a
/// terminal: there is nothing to restore, and nothing was broken.
pub fn arm_terminal_restore() {
    let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `tcgetattr` only writes to the pointer, which is a valid
    // uninitialised `termios`. A non-zero return means it wrote nothing.
    if unsafe { libc::tcgetattr(libc::STDERR_FILENO, original.as_mut_ptr()) } != 0 {
        return;
    }
    // SAFETY: `tcgetattr` returned 0, so the value is initialised.
    let original = Box::into_raw(Box::new(unsafe { original.assume_init() }));
    if ORIGINAL
        .compare_exchange(
            ptr::null_mut(),
            original,
            Ordering::Release,
            Ordering::Relaxed,
        )
        .is_err()
    {
        // The menu is already open and its state is the one to go back to.
        // SAFETY: this pointer came from `Box::into_raw` just above and was
        // not published, so nothing else can reach it.
        drop(unsafe { Box::from_raw(original) });
        return;
    }

    for (slot, &signal) in TERMINATING.iter().enumerate() {
        // SAFETY: `restore` is a plain `extern "C"` function and uses only
        // async-signal-safe calls, which is what `signal` requires of it.
        // `signal` hands back the disposition it replaced.
        let inherited = unsafe { libc::signal(signal, restore as *const () as libc::sighandler_t) };
        if inherited == libc::SIG_IGN {
            // The caller asked for this signal to be ignored, so it was never
            // going to end the process and there is nothing to restore.
            // Honouring that matters: the ignore is inherited by the command
            // the menu runs.
            // SAFETY: putting back the disposition just read.
            unsafe { libc::signal(signal, libc::SIG_IGN) };
            continue;
        }
        INHERITED[slot].store(inherited, Ordering::Release);
    }
}

/// Put the handlers back to their default and stop restoring.
///
/// Called when the menu closes, because what happens next is a child command
/// running with the terminal it wants: a `SIGTERM` arriving while `vim` is up
/// must not reset the terminal out from under it. The captured settings are
/// leaked rather than freed, so a signal already on its way cannot read a
/// dangling pointer.
pub fn disarm_terminal_restore() {
    for (slot, &signal) in TERMINATING.iter().enumerate() {
        let inherited = INHERITED[slot].swap(NOT_ARMED, Ordering::AcqRel);
        if inherited == NOT_ARMED {
            continue;
        }
        // SAFETY: putting back the disposition that was read from this same
        // signal while arming.
        unsafe { libc::signal(signal, inherited) };
    }
    ORIGINAL.store(ptr::null_mut(), Ordering::Release);
}

/// Put the terminal back, then die of the signal that arrived.
extern "C" fn restore(signal: libc::c_int) {
    let original = ORIGINAL.load(Ordering::Acquire);
    if !original.is_null() {
        // SAFETY: the pointer is a leaked `Box<termios>` that outlives the
        // process, and `tcsetattr` only reads through it.
        unsafe { libc::tcsetattr(libc::STDERR_FILENO, libc::TCSANOW, original) };
    }
    // Show the cursor and leave the alternate screen. Written by hand because
    // this cannot go through crossterm, which locks.
    const RESET: &[u8] = b"\x1b[?25h\x1b[?1049l";
    // SAFETY: a plain write of a static buffer to a raw fd. A short or failed
    // write is not worth handling here — the process is on its way out.
    unsafe {
        libc::write(libc::STDERR_FILENO, RESET.as_ptr().cast(), RESET.len());
        // Die of this signal the way the process would have. A signal armed
        // here was never inherited as ignored, so the default is what it had.
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}
