use std::env;
use std::fmt::Display;
use std::time::SystemTime;

use crate::child::{self, Child};

// See build.rs for the definition of `boi_has_driver`.
#[cfg(boi_has_driver = "apfs")]
mod driver_apfs;
#[cfg(boi_has_driver = "none")]
mod driver_none;

#[derive(clap::Args)]
pub struct Args {
    /// How to snapshot the home directory
    #[arg(long)]
    #[arg(default_value_t)]
    driver: DriverKind,

    #[cfg(boi_has_driver = "apfs")]
    #[command(flatten)]
    apfs: driver_apfs::Args,
}

pub async fn main(args: Args) -> child::Result<()> {
    let Ok(repo) = env::var("BORG_REPO")
        .map_err(|err| die!("Can't read $BORG_REPO ({err}); how can I back up?"));

    let Ok(ts) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        die!("System time is before the UNIX epoch; what are you doing?!?");
    };

    let backup_spec = format!("{repo}::{sec}", sec = ts.as_secs());
    let cmd = Child::from_cmdline(&[
        "borg",
        "create",
        "--exclude-from=.borg-excludes",
        "--exclude-caches",
        "--compression=auto,zstd,19",
        "--progress",
        "--stats",
        &backup_spec,
        ".",
    ]);

    let result = match args.driver {
        #[cfg(boi_has_driver = "apfs")]
        DriverKind::Apfs => driver_apfs::in_backup_root(args.apfs, cmd.complete()).await,
        #[cfg(boi_has_driver = "none")]
        DriverKind::None => driver_none::in_backup_root(cmd.complete()).await,
    };
    if let Err(err) = result {
        die!("Borg did not succeed ({err}); you should look at that.");
    }
    Ok(())
}

#[derive(Clone, clap::ValueEnum)]
pub enum DriverKind {
    #[cfg(boi_has_driver = "apfs")]
    Apfs,
    #[cfg(boi_has_driver = "none")]
    None,
}

impl Default for DriverKind {
    #[allow(unreachable_code)]
    fn default() -> Self {
        #[cfg(boi_has_driver = "apfs")]
        return DriverKind::Apfs;
        #[cfg(boi_has_driver = "none")]
        return DriverKind::None;
    }
}

impl Display for DriverKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(boi_has_driver = "apfs")]
            DriverKind::Apfs => "apfs",
            #[cfg(boi_has_driver = "none")]
            DriverKind::None => "none",
        })
    }
}
