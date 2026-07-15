use std::env;
use std::path::PathBuf;

pub async fn with_backup_root<O, F, T>(fut: O) -> T
where
    O: FnOnce(PathBuf) -> F,
    F: Future<Output = T>,
{
    match env::home_dir() {
        Some(home) => fut(home).await,
        None => die!("Can't find $HOME; what do I back up?"),
    }
}
