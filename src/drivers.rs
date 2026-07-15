use std::path::Path;
use std::pin::Pin;

// See build.rs for the definition of `boi_has_driver`.
#[cfg(boi_has_driver = "apfs")]
pub mod apfs;
#[cfg(boi_has_driver = "none")]
pub mod none;

/// A dyn compatible representation of a filesystem tree that should be backed up.
///
/// The ideal backup root is a read-only filesystem snapshot, to ensure that Borg sees consistent
/// states of all files, and doesn't fail due to files being modified during archive creation.
///
/// Each driver should define a `prepare` function to create its backup root. The `prepare` and
/// `cleanup` functions should [`die`] on failure, and the consumer of the backup root should be
/// prepared for this to happen. Non-trivial `prepare` and `cleanup` implementations should
/// [report](crate::reporting) their progress, and indicate any creation or deletion of filesystem
/// snapshots used to materialize the backup root.
pub trait BackupRoot {
    /// The path to the directory that Borg should execute from.
    fn path(&self) -> &Path;

    /// Deletes temporary resources used to materialize the backup root.
    fn cleanup(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()>>>;
}
