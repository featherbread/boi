use std::env;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use tokio::fs;

use crate::child::Child;
use crate::result_of;

/// Constructs an array where all of the contained values are coerced into
/// [`OsStr`](std::ffi::OsStr) slices.
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

pub async fn in_backup_root<F, T>(args: Args, fut: F) -> T
where
    F: Future<Output = T>,
{
    let cleanup = enter_snapshot(args).await;
    let result = fut.await;
    cleanup.await;
    result
}

async fn enter_snapshot(args: Args) -> impl Future<Output = ()> {
    let Some(home_abs) = env::home_dir() else {
        die!("Can't find $HOME; what do I back up?");
    };
    let Ok(home_sub) = home_abs.strip_prefix("/") else {
        die!("$HOME isn't an absolute path; this is too confusing.");
    };

    let Ok(mount_src) = find_mount_base(&home_abs).map_err(|err| {
        die!("Can't find where $HOME is mounted ({err}); I won't be able to snapshot.")
    });

    let snapshot_date = create_local_snapshot().await;
    let snapshot_id = format!("com.apple.TimeMachine.{snapshot_date}.local");

    let mount_target = env::temp_dir().join(format!(
        "{pkg}-apfs-{pid}",
        pkg = env!("CARGO_PKG_NAME"),
        pid = std::process::id()
    ));

    speak!("$", "mkdir {dir}", dir = mount_target.display());
    if let Err(err) = fs::create_dir_all(&mount_target).await {
        die!("Failed to create mount directory ({err}); I can't mount the snapshot.");
    }

    let mount_cmdline = os_strs![
        "mount_apfs",
        "-s",
        &snapshot_id,
        &mount_src,
        mount_target.as_os_str(),
    ];
    if let Err(err) = Child::from_cmdline(&mount_cmdline)
        .null_output()
        .complete()
        .await
    {
        die!("Can't mount snapshot ({err}); I won't be able to back it up.");
    }

    let backup_root = mount_target.join(home_sub);
    speak!("$", "cd {dir}", dir = backup_root.display());
    if let Err(err) = env::set_current_dir(backup_root) {
        die!("Can't change to snapshot dir ({err}); I won't be able to back it up.");
    }

    // Returning a future for the cleanup removes lots of boilerplate compared to an RAII guard,
    // since we don't need to hand-write a struct for the values we care about sharing.
    // Any awkwardness of this approach is internal to this module.
    async move {
        speak!("$", "cd {dir}", dir = home_abs.display());
        if let Err(err) = env::set_current_dir(home_abs) {
            die!("Can't return to $HOME ({err}); I won't be able to unmount the snapshot.");
        }

        if let Err(err) =
            Child::from_cmdline(&os_strs!["diskutil", "unmount", mount_target.as_os_str()])
                .null_output()
                .complete()
                .await
        {
            die!("Failed to unmount snapshot ({err}); you should take a look at that.");
        }

        speak!("$", "rmdir {dir}", dir = mount_target.display());
        if let Err(err) = fs::remove_dir(mount_target).await {
            die!("Failed to remove mount directory ({err}); you should take a look at that.");
        }

        if args.apfs_keep_snapshot {
            return;
        }
        if let Err(err) = Child::from_cmdline(&["tmutil", "deletelocalsnapshots", &snapshot_date])
            .null_input()
            .null_output()
            .null_timezone()
            .spawn_and_background_after(Duration::from_millis(500))
            .await
        {
            die!("Failed to start snapshot cleanup ({err}); you should take a look at that.");
        }
    }
}

fn find_mount_base(path: &Path) -> io::Result<OsString> {
    // unwrap() is reasonable as file paths should never contain inner NULs.
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut stat = MaybeUninit::<libc::statfs>::uninit();

    // SAFETY: We made the path a valid C string, and only construct a raw pointer to the
    // uninitialized statfs struct.
    result_of(|| unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) })?;

    // SAFETY: stat must be initialized if we made it here, and something is deeply wrong if
    // f_mntonname isn't a valid C string.
    unsafe {
        let stat = stat.assume_init();
        let mntonname = CStr::from_ptr(stat.f_mntonname.as_ptr()).to_bytes();
        Ok(OsStr::from_bytes(mntonname).to_owned())
    }
}

async fn create_local_snapshot() -> String {
    let Ok(out) = Child::from_cmdline(&["tmutil", "localsnapshot"])
        .null_timezone()
        .capture_output()
        .await
        .map_err(|err| {
            die!("Can't make a Time Machine snapshot ({err}); you should look into that.")
        });

    const LOCAL_SNAPSHOT_MSG: &str = "Created local snapshot with date: ";

    match str::from_utf8(&out.stdout)
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix(LOCAL_SNAPSHOT_MSG)))
    {
        Some(date) => date.to_owned(),
        None => die!("Can't find the snapshot date in tmutil's output; what do I mount?"),
    }
}
