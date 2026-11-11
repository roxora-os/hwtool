//! SIGINT, SIGHUP and SIGTERM while a change runs.
//!
//! Programs hwtool starts run in their own process group, so Ctrl-C or a
//! closed terminal reaches hwtool only and never stops dpkg, apt or a
//! bootloader write half way. hwtool notes the signal, lets the running
//! step finish, then stops and runs the plan's cleanup.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn install() {
    for sig in [libc::SIGINT, libc::SIGHUP, libc::SIGTERM] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(
                sig,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Pretend a signal arrived (tests).
pub fn raise_for_test() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn reset() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}
