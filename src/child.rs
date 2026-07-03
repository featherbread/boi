use std::ffi::OsStr;
use std::fmt::{self, Display};
use std::io;
use std::os::fd::OwnedFd;
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use tokio::process::ChildStdout;

use crate::config::{Config, RepoConfig};
use crate::signals;

/// A result type for execution of a [`Child`].
pub type Result<T> = std::result::Result<T, Error>;

/// A child command that executes with reasonable default settings.
///
/// The child inherits the parent's environment and standard streams unless specified otherwise.
///
/// If a timezone is set in the configuration, the child's `TZ` is set to that value by default.
/// Timezone-sensitive commands like `borg prune` need this to behave consistently, so this default
/// is chosen to limit the risk of data loss. [`Child::null_timezone`] can override this for select
/// commands, but should be used carefully.
pub struct Child {
    pending_cmd: tokio::process::Command,
    timezone_nulled: bool,
}

#[allow(dead_code)]
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
        Child {
            pending_cmd: cmd,
            timezone_nulled: false,
        }
    }

    /// Configures a child's environment to work with a single Borg repository.
    pub fn for_borg_repo(mut self, config: &RepoConfig) -> Self {
        for (key, value) in config.env() {
            self.pending_cmd.env(key, value);
        }
        self
    }

    /// Directs the child's standard output and error streams to a null device.
    pub fn null_output(mut self) -> Self {
        self.pending_cmd.stdout(Stdio::null()).stderr(Stdio::null());
        self
    }

    /// Directs the child's standard input stream to a null device.
    pub fn null_input(mut self) -> Self {
        self.pending_cmd.stdin(Stdio::null());
        self
    }

    /// Removes any `TZ` value from the child's environment.
    ///
    /// This includes the config-based override described in [the type-level documentation](Child),
    /// as well as any `TZ` value that would be inherited from the parent environment.
    ///
    /// Use this with caution, and avoid it on Borg commands.
    pub fn null_timezone(mut self) -> Self {
        self.timezone_nulled = true;
        self
    }

    /// Runs the child and waits for it to finish.
    ///
    /// Until `complete` returns, the parent ignores common termination signals under the
    /// assumption that they're sent to the entire process group.
    pub async fn complete(self) -> Result<()> {
        let mut cmd = self.into_cmd().await?;
        let mut child = cmd.spawn().map_err(Error::Launch)?;
        let _signal_guard = signals::ignore();
        Self::wait_result(child.wait().await)
    }

    /// Spawns the child and provides access to its combined standard streams.
    ///
    /// Until the first call to [`Spawn::wait`] returns, the parent ignores common termination
    /// signals under the assumption that they're sent to the entire process group.
    pub async fn spawn_with_output(self) -> Result<(Spawn, ChildStdout)> {
        let (output, stdout_in) = std::io::pipe().map_err(Error::Launch)?;
        let stderr_in = stdout_in.try_clone().map_err(Error::Launch)?;

        let output = OwnedFd::from(output);
        let output = std::process::ChildStdout::from(output);
        let output = ChildStdout::from_std(output).map_err(Error::Launch)?;

        let mut cmd = self.into_cmd().await?;
        let child = cmd
            .stdout(stdout_in)
            .stderr(stderr_in)
            .spawn()
            .map_err(Error::Launch)?;

        Ok((
            Spawn {
                child,
                signal_guard: Some(signals::ignore()),
            },
            output,
        ))
    }

    /// Spawns the child and waits up to `duration` for it to exit before leaking and ignoring it.
    ///
    /// This is intended for long-running children doing low-priority post-snapshot cleanup,
    /// and helps catch errors in their startup (like invalid arguments) without blocking the user
    /// indefinitely.
    ///
    /// Nothing special is done with the child's standard I/O streams. You should probably use
    /// [`null_input`](Self::null_input) and/or [`null_output`](Self::null_output) with children
    /// spawned in the background. However, the child is run in an independent process group, so
    /// will not receive keyboard-generated signals even if its output remains connected to a
    /// terminal.
    pub async fn spawn_and_background_after(self, duration: Duration) -> Result<()> {
        let mut cmd = self.into_cmd().await?;
        let mut child = cmd.process_group(0).spawn().map_err(Error::Launch)?;
        match tokio::time::timeout(duration, child.wait()).await {
            Err(_timeout) => Ok(()),
            Ok(wait) => Self::wait_result(wait),
        }
    }

    /// Runs the child with standard streams directed to an in-memory buffer and waits for it to
    /// finish.
    ///
    /// Nothing special is done with respect to signal handling. This is intended for short-running
    /// children that a user is unlikely to interrupt.
    pub async fn capture_output(self) -> Result<Output> {
        let mut cmd = self.into_cmd().await?;
        cmd.output().await.map_err(Error::Launch)
    }

    /// Performs final configuration of the command based on the global config file, which may need
    /// to be loaded asynchronously. This lets the rest of the builder API remain synchronous.
    async fn into_cmd(mut self) -> Result<tokio::process::Command> {
        // This is unlikely to fail in practice, since loading the config (or dying) is the first
        // thing most subcommands do. Treating it as Error::Launch means nobody has to deal with it
        // as a special case, especially with the unusual toml::de::Error Display impl.
        let config = Config::load()
            .await
            .map_err(|_| Error::Launch(io::Error::other("failed to load boi configuration")))?;

        if self.timezone_nulled {
            self.pending_cmd.env_remove("TZ");
        } else {
            self.pending_cmd.env("TZ", config.global().timezone());
        }

        let cmd = self.pending_cmd;
        Ok(cmd)
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
}

impl Display for Child {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cmd = self.pending_cmd.as_std();
        write!(f, "{cmd}", cmd = cmd.get_program().display())?;
        cmd.get_args()
            .try_for_each(|arg| write!(f, " {arg}", arg = arg.display()))
    }
}

/// A child process started by [`Child::spawn_with_output`].
pub struct Spawn {
    child: tokio::process::Child,
    signal_guard: Option<signals::IgnoreGuard>,
}

impl Spawn {
    /// Waits for the child to exit completely.
    ///
    /// The first completed wait un-ignores the termination signals ignored by
    /// [`Child::spawn_with_output`]. Subsequent calls are allowed and yield the same return value.
    pub async fn wait(&mut self) -> Result<()> {
        let result = self.child.wait().await;
        self.signal_guard = None;
        Child::wait_result(result)
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
