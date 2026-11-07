//! Termination signals.
//!
//! Ctrl+C (SIGINT) is handled by `ctrlc` in `main`: the terminal delivers it
//! to the task's process too, so mq-task only has to note it. SIGTERM and
//! SIGHUP are different: they reach mq-task alone (a CI cancel, `kill`, a
//! closed terminal), so the running task has to be stopped on its behalf, and
//! its `defer` tasks must still run. This module records them.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Set once SIGTERM or SIGHUP arrived. Running tasks are stopped and no new
/// ones start; `defer` tasks (cleanup) still run.
pub static TERMINATED: AtomicBool = AtomicBool::new(false);

/// The signal number behind [`TERMINATED`] (0 when none arrived).
static SIGNAL: AtomicI32 = AtomicI32::new(0);

/// True once SIGTERM or SIGHUP arrived.
pub fn terminated() -> bool {
    TERMINATED.load(Ordering::SeqCst)
}

/// Process exit code for the signal that ended the run (`128 + signal`, like
/// a shell reports it), if there was one.
pub fn exit_code() -> Option<u8> {
    let signal = SIGNAL.load(Ordering::SeqCst);
    (signal > 0).then(|| (128 + signal).min(255) as u8)
}

#[cfg(unix)]
/// Record a termination signal. Async-signal-safe: atomic stores only.
fn note(signal: i32) {
    SIGNAL.store(signal, Ordering::SeqCst);
    TERMINATED.store(true, Ordering::SeqCst);
    crate::INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Start recording SIGTERM and SIGHUP instead of dying by them.
#[cfg(unix)]
pub fn install() {
    extern "C" fn handler(signal: libc::c_int) {
        note(signal);
    }
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `sigaction` is given a zeroed struct with a valid handler;
        // the handler only performs atomic stores.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler as *const () as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Windows has no SIGTERM; console control events are covered by `ctrlc`.
#[cfg(not(unix))]
pub fn install() {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn sigterm_is_recorded_not_fatal() {
        // Single test touching the process-wide flags.
        install();
        // SAFETY: signalling ourselves; the handler installed above runs.
        unsafe { libc::raise(libc::SIGTERM) };
        assert!(terminated());
        assert_eq!(exit_code(), Some(143));
        TERMINATED.store(false, Ordering::SeqCst);
        SIGNAL.store(0, Ordering::SeqCst);
        crate::INTERRUPTED.store(false, Ordering::SeqCst);
    }
}
