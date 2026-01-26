use std::ffi::OsStr;
use std::fmt::{self, Display};
use std::io;
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

mod foreground;

/// A result type for execution of a [`Child`].
pub type Result<T> = std::result::Result<T, Error>;

/// A child command that executes with reasonable default settings.
///
/// The child inherits the parent's environment and standard streams unless specified otherwise.
///
/// Each command line is [spoken](speak) before running it, like `set -x` in a shell.
///
/// If `BOI_TZ` is set in the environment, the child's `TZ` is set to that value by default.
/// Timezone-sensitive commands like `borg prune` need this to behave consistently, so this default
/// is chosen to limit the risk of data loss. [`Child::null_timezone`] can override this for select
/// commands, but should be used carefully.
pub struct Child(tokio::process::Command);

#[allow(unused)]
impl Child {
    /// Constructs a new [`Child`] from a command name followed by arguments.
    ///
    /// # Panics
    ///
    /// If `cmdline` is empty. This is less compile-time safe, but more ergonomic.
    pub fn from_cmdline<S: AsRef<OsStr>>(cmdline: &[S]) -> Self {
        let (program, args) = cmdline.split_first().expect("cmdline should not be empty");
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args);
        if let Some(tz) = std::env::var_os("BOI_TZ") {
            cmd.env("TZ", tz);
        }
        Child(cmd)
    }

    /// Directs the child's standard output and error streams to a null device.
    pub fn null_output(mut self) -> Self {
        self.0.stdout(Stdio::null()).stderr(Stdio::null());
        self
    }

    /// Directs the child's standard input stream to a null device.
    pub fn null_input(mut self) -> Self {
        self.0.stdin(Stdio::null());
        self
    }

    /// Removes any `TZ` value from the child's environment.
    ///
    /// This includes `BOI_TZ` overrides as described in [the type-level documentation](Child),
    /// as well as any `TZ` value that would be inherited from the parent environment.
    ///
    /// Use this with caution, and avoid it on Borg commands.
    pub fn null_timezone(mut self) -> Self {
        self.0.env_remove("TZ");
        self
    }

    /// Runs the child and waits for it to finish.
    ///
    /// When possible, the child is run as the foreground process of the current controlling
    /// terminal, so that it receives all keyboard-generated signals.
    pub async fn complete(self) -> Result<()> {
        speak!("{self}");

        // SAFETY: This call is unsafe for concurrent use. As of writing, this entire program does
        // all work in a single async task on a single-threaded runtime.
        wait_result(unsafe { foreground::complete(self.0).await })
    }

    /// Spawns the child and waits up to `duration` for it to exit before leaking and ignoring it.
    ///
    /// This is intended for long-running children doing low-priority post-snapshot cleanup,
    /// and helps catch errors in their startup (like invalid arguments) without blocking the user
    /// indefinitely.
    ///
    /// Nothing special is done with the child's standard I/O streams. You should probably use
    /// [`null_input`](Self::null_input) and/or [`null_output`](Self::null_output) with children
    /// spawned in the background.
    pub async fn spawn_and_background_after(mut self, duration: Duration) -> Result<()> {
        speak!("{self} &");
        let mut child = self.0.spawn().map_err(Error::Launch)?;
        match tokio::time::timeout(duration, child.wait()).await {
            Err(_timeout) => Ok(()),
            Ok(wait) => wait_result(wait),
        }
    }

    /// Runs the child with standard streams directed to an in-memory buffer and waits for it to
    /// finish.
    ///
    /// Unlike [`complete`](Self::complete), nothing special is done with the terminal or signal
    /// handling. This is intended for short-running children that a user is unlikely to interrupt.
    pub async fn capture_output(mut self) -> Result<Output> {
        speak!("{self}");
        self.0.output().await.map_err(Error::Launch)
    }
}

impl Display for Child {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cmd = self.0.as_std();
        write!(f, "$ {cmd}", cmd = cmd.get_program().display())?;
        cmd.get_args()
            .try_for_each(|arg| write!(f, " {arg}", arg = arg.display()))
    }
}

fn wait_result(result: std::result::Result<ExitStatus, io::Error>) -> Result<()> {
    match result {
        Err(err) => Err(Error::Launch(err)),
        Ok(exit) if exit.success() => Ok(()),
        Ok(exit) => match exit.code() {
            Some(code) => Err(Error::ExitCode(code)),
            None => Err(Error::Killed),
        },
    }
}

/// An error while executing a [`Child`].
#[derive(Debug)]
pub enum Error {
    Launch(io::Error),
    ExitCode(i32),
    Killed,
}

impl Error {
    /// Terminates the current process with a generic message that something went wrong.
    ///
    /// If the error came from a child exiting with a non-zero code, `die` terminates this process
    /// with that same code. Otherwise, it terminates with code 1.
    pub fn die(&self) -> ! {
        let code = match self {
            Error::Launch(_) | Error::Killed => 1,
            Error::ExitCode(code) => *code,
        };
        die!(
            code = code,
            "Something went wrong ({self}); you might need do something about that."
        );
    }
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Launch(err) => write!(f, "failed to launch child: {err}"),
            Error::ExitCode(code) => write!(f, "child exited with code {code}"),
            Error::Killed => write!(f, "child terminated abnormally"),
        }
    }
}
