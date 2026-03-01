use std::env;

pub async fn in_backup_root<F, T>(fut: F) -> T
where
    F: Future<Output = T>,
{
    let Some(home) = env::home_dir() else {
        die!("Can't find $HOME; what do I back up?");
    };

    speak!("$", "cd {dir}", dir = home.display());
    if let Err(err) = env::set_current_dir(home) {
        die!("Can't change to $HOME ({err}); I won't be able to back it up.");
    }

    fut.await
}
