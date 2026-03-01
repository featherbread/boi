//! Temporarily ignore common termination signals.
//!
//! This is arguably the simplest way to deal with signals while running child processes that
//! perform their own signal handling and graceful termination. Children default to running in the
//! same process group as their parent, and receive their own copies of keyboard-generated signals
//! like SIGINT without the parent having to forward them. If the parent terminated itself on these
//! same signals, the children would be left to finish in the background or get terminated by
//! SIGPIPE while reporting their cleanup progress to the dead parent.
//!
//! # Caveats
//!
//! A manual `kill` on the parent process (rather than its process group) has no effect on its
//! children, which may seem unintuitive to an inexperienced Unix operator.
//!
//! The first call to [`ignore`] registers Tokio-compatible handlers for the supported signals.
//! For the remainder of the program's life, these signals are processed by a background task that
//! emulates their original behavior if no [`IgnoreGuard`] is live. The emulation's behavior should
//! be indistinguishable from the system's default handlers to an external observer, but the
//! implementation could conflict with other manipulation of the signal handlers in rare cases.
//! It's best not to combine this module with any other form of signal handling.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

use futures::StreamExt;
use signal_hook_tokio::Signals;

static IGNORE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// See [`ignore`].
pub struct IgnoreGuard;

/// Ignore common termination signals until the [`IgnoreGuard`] is dropped.
///
/// If multiple guards are created, signals remain ignored until all of them are dropped.
/// If enough guards are created to overflow a `usize`, weird things may happen, so don't do that.
///
/// # Panics
///
/// `ignore` panics if it fails to initialize the required signal behavior.
pub fn ignore() -> IgnoreGuard {
    setup();
    IGNORE_COUNT.fetch_add(1, Ordering::Relaxed);
    IgnoreGuard
}

impl Drop for IgnoreGuard {
    fn drop(&mut self) {
        IGNORE_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

fn setup() {
    static SETUP_ONCE: Once = Once::new();

    SETUP_ONCE.call_once(|| {
        use signal_hook::consts::signal::*;

        let mut signals = Signals::new([SIGHUP, SIGINT, SIGQUIT])
            .expect("signal handlers should have been registered");

        tokio::spawn(async move {
            while let Some(signal) = signals.next().await {
                if IGNORE_COUNT.load(Ordering::Relaxed) == 0 {
                    let _ = signal_hook::low_level::emulate_default_handler(signal);
                }
            }
        });
    });
}
