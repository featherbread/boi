use std::env;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use thiserror::Error;
use tokio::fs;

use crate::child::{self, Child};
use crate::reporting::{Reporter, Widget};

/// Constructs an array where all of the contained values are coerced into [`OsStr`] slices.
///
/// This makes it nicer to mix string literals with other `OsStr` values.
macro_rules! os_strs {
    ($($e:expr),* $(,)?) => { [$(::std::ffi::OsStr::new($e)),*] };
}

#[derive(clap::Args)]
#[group(id = "snapshot_apfs")]
pub struct Args {
    /// Don't delete the Time Machine snapshot after the backup
    #[arg(long)]
    apfs_keep_snapshot: bool,
}

pub async fn prepare(args: Args) -> Snapshot {
    let reporter = Reporter::new(Widget::text("Creating APFS snapshot…")).lock_repos();
    match Snapshot::create_and_mount(args).await {
        Ok(snapshot) => {
            let date = &snapshot.date;
            reporter.succeed(format_args!("Created and mounted APFS snapshot {date}."));
            snapshot
        }
        Err(err) => reporter.die(format_args!(
            "Failed to create APFS snapshot ({err}); you should look at that."
        )),
    }
}

async fn unprepare(snapshot: Snapshot) {
    let reporter = Reporter::new(Widget::text("Unmounting APFS snapshot…")).lock_repos();
    if let Err(err) = snapshot.unmount().await {
        reporter.die(format_args!(
            "Failed to unmount APFS snapshot ({err}); you should look at that."
        ));
    }
    if snapshot.keep {
        reporter.succeed("Unmounted APFS snapshot; keeping per your request.");
        return;
    }
    match snapshot.delete().await {
        Ok(()) => reporter.succeed("Unmounted APFS snapshot; deleting in background."),
        Err(err) => reporter.die(format_args!(
            "Failed to delete APFS snapshot ({err}); you should look at that."
        )),
    }
}

pub struct Snapshot {
    keep: bool,
    date: String,
    mount_target: PathBuf,
    full_path: PathBuf,
}

impl super::BackupRoot for Snapshot {
    fn path(&self) -> &Path {
        &self.full_path
    }

    fn cleanup(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(unprepare(*self))
    }
}

impl Snapshot {
    async fn create_and_mount(args: Args) -> Result<Self> {
        let home_abs = env::home_dir().ok_or(Error::HomeUnknown)?;
        let home_sub = home_abs.strip_prefix("/").or(Err(Error::HomeIsRelative))?;
        let mount_src = find_mount_base(&home_abs).map_err(Error::HomeMountBaseUnknown)?;

        let snapshot_date = create_local_snapshot().await?;
        let snapshot_id = format!("com.apple.TimeMachine.{snapshot_date}.local");

        // The APFS driver only makes sense on Apple platforms, which default to providing per-user
        // temporary directories with secure read and write permissions. The PID is used solely to
        // limit collisions with previous boi instances that failed to fully clean up this state,
        // and NOT as a dangerously defective strategy for generating unpredictable temporary paths.
        let mount_target = env::temp_dir().join(format!(
            "{pkg}-apfs-{pid}",
            pkg = env!("CARGO_PKG_NAME"),
            pid = std::process::id()
        ));

        fs::create_dir(&mount_target)
            .await
            .map_err(Error::MountTargetCreateFailed)?;

        let mount_cmdline = os_strs![
            "mount_apfs",
            "-s",
            &snapshot_id,
            &mount_src,
            mount_target.as_os_str()
        ];
        Child::from_cmdline(&mount_cmdline)
            .null_output()
            .complete()
            .await
            .map_err(Error::MountFailed)?;

        let full_path = mount_target.join(home_sub);

        Ok(Self {
            keep: args.apfs_keep_snapshot,
            date: snapshot_date,
            mount_target,
            full_path,
        })
    }

    async fn unmount(&self) -> Result<()> {
        Child::from_cmdline(&os_strs![
            "diskutil",
            "unmount",
            self.mount_target.as_os_str()
        ])
        .null_output()
        .complete()
        .await
        .map_err(|err| Error::UnmountFailed(self.mount_target.clone(), err))?;

        fs::remove_dir(&self.mount_target)
            .await
            .map_err(|err| Error::MountTargetCleanupFailed(self.mount_target.clone(), err))?;

        Ok(())
    }

    async fn delete(&self) -> Result<()> {
        Child::from_cmdline(&["tmutil", "deletelocalsnapshots", &self.date])
            .null_input()
            .null_output()
            .null_timezone()
            .spawn_and_background_after(Duration::from_millis(500))
            .await
            .map_err(|err| Error::SnapshotCleanupFailed(self.date.clone(), err))
    }
}

fn find_mount_base(path: &Path) -> io::Result<OsString> {
    // unwrap() is reasonable as file paths should never contain inner NULs.
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut stat = MaybeUninit::<libc::statfs>::uninit();

    // SAFETY: We made the path a valid C string, and only construct a raw pointer to the
    // uninitialized statfs struct.
    let result = unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: stat must be initialized if we made it here, and something is deeply wrong if
    // f_mntonname isn't a valid C string.
    unsafe {
        let stat = stat.assume_init();
        let mntonname = CStr::from_ptr(stat.f_mntonname.as_ptr()).to_bytes();
        Ok(OsStr::from_bytes(mntonname).to_owned())
    }
}

async fn create_local_snapshot() -> Result<String> {
    let out = Child::from_cmdline(&["tmutil", "localsnapshot"])
        .null_timezone()
        .capture_output()
        .await
        .map_err(Error::SnapshotCreateFailed)?;

    const LOCAL_SNAPSHOT_MSG: &str = "Created local snapshot with date: ";

    match str::from_utf8(&out.stdout)
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix(LOCAL_SNAPSHOT_MSG)))
    {
        Some(date) => Ok(date.to_owned()),
        None => Err(Error::SnapshotMissingDate),
    }
}

type Result<T> = std::result::Result<T, Error>;

// TODO: child::Error should include the command line, instead of me rewriting each command here
// where it could get stale.
#[derive(Error, Debug)]
enum Error {
    #[error("can't determine $HOME")]
    HomeUnknown,
    #[error("$HOME is a non-absolute path")]
    HomeIsRelative,
    #[error("can't find where $HOME is mounted: {0}")]
    HomeMountBaseUnknown(io::Error),
    #[error("tmutil localsnapshot failed: {0}")]
    SnapshotCreateFailed(child::Error),
    #[error("can't find snapshot date in tmutil localsnapshot output")]
    SnapshotMissingDate,
    #[error("failed to create mount directory: {0}")]
    MountTargetCreateFailed(io::Error),
    #[error("mount_apfs -s failed: {0}")]
    MountFailed(child::Error),
    #[error("diskutil unmount {0} failed: {1}")]
    UnmountFailed(PathBuf, child::Error),
    #[error("failed to clean up {0}: {1}")]
    MountTargetCleanupFailed(PathBuf, io::Error),
    #[error("tmutil deletelocalsnapshots {0} failed: {1}")]
    SnapshotCleanupFailed(String, child::Error),
}
