use std::env;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
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

pub async fn with_backup_root<O, F, T>(args: Args, op: O) -> T
where
    O: FnOnce(PathBuf) -> F,
    F: Future<Output = T>,
{
    let reporter = Reporter::new(Widget::text("Creating APFS snapshot…")).lock_repos();
    let snapshot = match enter_snapshot(args).await {
        Ok(snapshot) => {
            reporter.succeed(format_args!(
                "Created and mounted APFS snapshot {}.",
                snapshot.date,
            ));
            snapshot
        }
        Err(err) => {
            reporter.die(format_args!(
                "Failed to create APFS snapshot ({err}); you should look at that."
            ));
        }
    };

    let result = op(snapshot.path).await;

    let reporter = Reporter::new(Widget::text("Unmounting APFS snapshot…")).lock_repos();
    match snapshot.cleanup.await {
        Ok(action) => {
            reporter.succeed(match action {
                Cleanup::Kept => "Unmounted APFS snapshot; keeping per your request.",
                Cleanup::Deleting => "Unmounted APFS snapshot; deleting in background.",
            });
            result
        }
        Err(err) => {
            reporter.die(format_args!(
                "Failed to clean up APFS snapshot ({err}); you should look at that."
            ));
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

struct Snapshot<F> {
    path: PathBuf,
    date: String,
    cleanup: F,
}

enum Cleanup {
    Kept,
    Deleting,
}

async fn enter_snapshot(args: Args) -> Result<Snapshot<impl Future<Output = Result<Cleanup>>>> {
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

    // Returning a future for the cleanup removes lots of boilerplate compared to an RAII guard,
    // since we don't need to hand-write a struct for the values we care about sharing.
    // Any awkwardness of this approach is internal to this module.
    Ok(Snapshot {
        path: mount_target.join(home_sub),
        date: snapshot_date.clone(),
        cleanup: async move {
            Child::from_cmdline(&os_strs!["diskutil", "unmount", mount_target.as_os_str()])
                .null_output()
                .complete()
                .await
                .map_err(|err| Error::UnmountFailed(mount_target.clone(), err))?;

            fs::remove_dir(&mount_target)
                .await
                .map_err(|err| Error::MountTargetCleanupFailed(mount_target, err))?;

            if args.apfs_keep_snapshot {
                return Ok(Cleanup::Kept);
            }

            Child::from_cmdline(&["tmutil", "deletelocalsnapshots", &snapshot_date])
                .null_input()
                .null_output()
                .null_timezone()
                .spawn_and_background_after(Duration::from_millis(500))
                .await
                .map_err(|err| Error::SnapshotCleanupFailed(snapshot_date, err))?;

            Ok(Cleanup::Deleting)
        },
    })
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
