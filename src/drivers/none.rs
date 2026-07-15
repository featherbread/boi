use std::env;
use std::path::{Path, PathBuf};
use std::pin::Pin;

pub struct Home(PathBuf);

pub async fn prepare() -> Home {
    match env::home_dir() {
        Some(home) => Home(home),
        None => die!("Can't find $HOME; what do I back up?"),
    }
}

impl super::BackupRoot for Home {
    fn path(&self) -> &Path {
        &self.0
    }

    fn cleanup(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(async {})
    }
}
