//! Run child processes like a shell does.

use std::io;
use std::process::ExitStatus;

use tokio::process::Command;
use tokio::signal::unix::{self as signal, SignalKind};

/// "Shells out" to a command and waits for it to finish.
///
/// Compared to merely spawning a child process and waiting for it:
///
///   - `complete` always spawns the child as the leader of an independent process group.
///   - If the child process is stopped, `complete` also stops the parent process, and continues
///     the child's process group after the parent process resumes.
///   - If the parent process is in the foreground process group of its controlling terminal,
///     `complete` switches the terminal to the child's process group, so the child exclusively
///     receives keyboard-generated signals like SIGINT and SIGTSTP.
///
/// While Tokio's process APIs (like Rust's) support a single [`Command`] spawning multiple
/// children, `complete` must make its own persistent mutations to the `Command` to ensure the
/// above behaviors, and thus takes ownership to prevent any surprises.
///
/// # Safety
///
/// `complete` must be used in a single-threaded program. It mutates shared state for the process
/// and its controlling terminal without synchronization, using facilities whose behavior in a
/// multi-threaded process is unspecified.
pub async unsafe fn complete(mut cmd: Command) -> io::Result<ExitStatus> {
    // SAFETY: We lift this module's requirement of non-concurrency to our caller.
    use self::unsafe_nonconcurrent::{stop_like, try_yield_foreground};

    let mut sigchld =
        signal::signal(SignalKind::child()).expect("tokio should have registered for SIGCHLD");

    let cmd = cmd.process_group(0);
    let mut child = cmd.spawn()?;

    let pid = child.id().expect("non-waited child should have a PID") as libc::pid_t;
    let mut foreground_guard = try_yield_foreground(pid).await;

    loop {
        tokio::select! {
            biased;
            result = child.wait() => return result,
            Some(()) = sigchld.recv() => stop_like(pid, &mut foreground_guard).await,
        }
    }
}

/// Private utilities to mutate process-global state without synchronization.
///
/// These functions are unsuitable for concurrent use, despite not being marked unsafe.
/// However, they're only accessible to [`complete`], which lifts this safety requirement to its
/// own caller. By eliminating that class of issues from consideration and shunting the unsafety of
/// FFI calls into [`unsafe_nonconcurrent_libc`] wrappers, any analysis of this module can focus
/// purely on logical correctness.
mod unsafe_nonconcurrent {
    #![forbid(unsafe_code)]

    use std::io;

    use tokio::fs::File;

    use super::unsafe_nonconcurrent_libc::*;

    /// Switches the controlling terminal's foreground process group if this process is in it.
    ///
    /// If this process isn't in the foreground (for example: it's in the background, or has no
    /// controlling terminal), then nothing happens. However, it's still correct to treat the
    /// returned [`None`] like a real guard.
    ///
    /// # Caveats
    ///
    /// [`Guard`] retains an open file descriptor for the controlling terminal of this process.
    pub async fn try_yield_foreground(to_pgid: libc::pid_t) -> Option<Guard> {
        let tty = File::open("/dev/tty").await.ok()?; // Ignore if we have no controlling terminal.

        if !tcgetpgrp(&tty).is_ok_and(|ttypgrp| ttypgrp == getpgrp()) {
            return None; // We aren't in the foreground, so we can't yield it.
        }

        match change_foreground_pgid(&tty, to_pgid) {
            Ok(()) => Some(Guard { tty }),
            Err(_) => None,
        }
    }

    /// Switches the controlling terminal's foreground process group back to its own when dropped.
    ///
    /// See [`try_yield_foreground`].
    pub struct Guard {
        tty: File,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = change_foreground_pgid(&self.tty, getpgrp());
        }
    }

    fn change_foreground_pgid(tty: &File, pgid: libc::pid_t) -> io::Result<()> {
        // If a background process calls tcsetpgrp for its controlling terminal without blocking or
        // ignoring SIGTTOU, the system sends it to all members of its process group. This is sort
        // of like a write with TOSTOP set, except it's not conditioned on TOSTOP, and the block
        // keeps the signal from even being sent (i.e. it's not delivered even after unblocking).
        //
        // We definitely need to handle this odd corner case to return ourselves to the foreground.
        // We might as well do it every time for simplicity.
        let old_mask = sigprocmask(MaskHow::Block, SigSet::one(Signal::Ttou));
        let result = tcsetpgrp(tty, pgid);
        sigprocmask(MaskHow::Set, old_mask);
        result
    }

    /// Propagates a stop of `pgid` to this process, and a continue of this process to `pgid`.
    ///
    /// If the child process represented by `pgid` isn't stopped, this function does nothing.
    ///
    /// After this process has resumed, it may have moved from the foreground to the background or
    /// vice versa. If necessary, it [yields the foreground](try_yield_foreground) to the child and
    /// writes out the new [`Guard`] for this process to claim it back.
    pub async fn stop_like(pgid: libc::pid_t, guard: &mut Option<Guard>) {
        if !WaitStatus::get(pgid).is_ok_and(|stat| stat.stopped()) {
            // Was there an error? Is the child terminated? Or still running?
            // Doesn't matter; it's not our problem.
            return;
        }

        // We might not _need_ to return to the foreground before stopping, but we shouldn't leak
        // the guard's open file descriptor for our controlling terminal.
        *guard = None;
        let _ = raise(Signal::Stop); // ...and now we have to wait for someone to resume us.

        *guard = try_yield_foreground(pgid).await;
        let _ = killpg(pgid, Signal::Cont);
    }
}

/// Wrappers for libc functions that mutate process-global state without synchronization.
///
/// Think of this as a bad clone of a tiny fraction of the "nix" crate, which I've created since my
/// goal is to practice designing Rust abstractions, not merely to write a Borg wrapper CLI.
///
/// Many of these functions are unsuitable for concurrent use, and must only be (transitively)
/// imported by functions that lift this safety requirement to their callers.
mod unsafe_nonconcurrent_libc {
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;

    use tokio::fs::File;

    use crate::result_of;

    /// Creates enums for sets of values represented by C integers.
    ///
    /// Each enum includes an `as_raw` method that yields the exact value originally defined in the
    /// macro invocation, which compared to a `#[repr(C)]` enum avoids annoying casts on each use.
    /// That said, the enum is defined with the discriminants cast to `isize`, since it's hard to
    /// imagine a platform whose C `int` is wider than the native pointer type.
    macro_rules! c_int_enum {
        (
            $(#[$eattr:meta])*
            $vis:vis $name:ident {
                $(
                    $(#[$vattr:meta])*
                    $variant:ident = $value:expr
                ),*
                $(,)?
            }
        ) => {
            $(#[$eattr])*
            $vis enum $name {
                $( $(#[$vattr])* $variant = $value as isize ),*
            }

            impl $name {
                /// Returns the C representation of this value.
                fn as_raw(&self) -> ::libc::c_int {
                    match self {
                        $( Self::$variant => $value, )*
                    }
                }
            }
        };
    }

    pub fn getpgrp() -> libc::pid_t {
        // SAFETY: Takes no arguments and is documented to always succeed.
        unsafe { libc::getpgrp() }
    }

    pub fn tcgetpgrp(tty: &File) -> io::Result<libc::pid_t> {
        // SAFETY: This could return a semantically invalid PGID if the controlling terminal has no
        // foreground process group, but that's still valid at a type level.
        result_of(|| unsafe { libc::tcgetpgrp(tty.as_raw_fd()) })
    }

    pub fn tcsetpgrp(tty: &File, pgid: libc::pid_t) -> io::Result<()> {
        // SAFETY: pgid is untrusted, but tcsetpgrp handles invalid values.
        result_of(|| unsafe { libc::tcsetpgrp(tty.as_raw_fd(), pgid) }).and(Ok(()))
    }

    pub struct WaitStatus(libc::c_int);

    impl WaitStatus {
        pub fn get(pid: libc::pid_t) -> io::Result<WaitStatus> {
            let mut stat_loc = 0 as libc::c_int;
            let options = libc::WNOHANG | libc::WUNTRACED;

            // SAFETY: The call should be able to handle invalid PIDs. Otherwise, we control the
            // arguments so we know they're good.
            result_of(|| unsafe { libc::waitpid(pid, &mut stat_loc, options) })
                .map(|_| WaitStatus(stat_loc))
        }

        pub fn stopped(&self) -> bool {
            libc::WIFSTOPPED(self.0)
        }
    }

    c_int_enum! {
        /// A minimal set of Unix signal numbers that are definitely valid.
        pub Signal {
            Stop = libc::SIGSTOP,
            Cont = libc::SIGCONT,
            Ttou = libc::SIGTTOU,
        }
    }

    pub fn killpg(pgid: libc::pid_t, signal: Signal) -> io::Result<()> {
        // SAFETY: We know the signal is part of a well-defined set, and the call itself handles
        // invalid pgid arguments.
        result_of(|| unsafe { libc::killpg(pgid, signal.as_raw()) }).and(Ok(()))
    }

    pub fn raise(signal: Signal) -> io::Result<()> {
        // SAFETY: We know the signal is part of a well-defined set.
        result_of(|| unsafe { libc::raise(signal.as_raw()) }).and(Ok(()))
    }

    /// A set of Unix signals.
    #[derive(Clone, Copy)]
    pub struct SigSet(libc::sigset_t);

    impl SigSet {
        pub fn empty() -> Self {
            // SAFETY: sigemptyset is one of the valid ways to initialize a sigset_t.
            unsafe {
                let mut mask = MaybeUninit::<libc::sigset_t>::uninit();
                libc::sigemptyset(mask.as_mut_ptr());
                SigSet(mask.assume_init())
            }
        }

        pub fn add(mut self, signal: Signal) -> Self {
            // SAFETY: We received a known-valid signal through our enum.
            unsafe { libc::sigaddset(&mut self.0, signal.as_raw()) };
            self
        }

        pub fn one(signal: Signal) -> Self {
            Self::empty().add(signal)
        }
    }

    c_int_enum! {
        /// Describes how [`sigprocmask`] should behave.
        pub MaskHow {
            /// Blocks signals in the provided set, in addition to those already blocked.
            Block = libc::SIG_BLOCK,
            /// Replaces the current signal mask with the provided set.
            Set = libc::SIG_SETMASK,
        }
    }

    /// Changes the current signal mask according to `how`, and returns the old mask.
    pub fn sigprocmask(how: MaskHow, mask: SigSet) -> SigSet {
        let mut old_mask = SigSet::empty();

        // SAFETY: We've wrapped everything in safe types; it should all be valid here.
        result_of(|| unsafe { libc::sigprocmask(how.as_raw(), &mask.0, &mut old_mask.0) })
            .map(|_| old_mask)
            .expect("sigprocmask should only fail for invalid how values")
    }
}
