use std::env;

use indexmap::IndexMap;
use serde_derive::Deserialize;
use tokio::io;
use tokio::sync::OnceCell;

#[derive(Deserialize)]
pub struct Config {
    #[expect(unused)]
    repos: IndexMap<String, RepoConfig>,
}

#[derive(Deserialize)]
pub struct RepoConfig {
    #[expect(unused)]
    repo_url: String,
    #[expect(unused)]
    password_command: Option<String>,
    // TODO: Include timezone as a per-repo and/or global setting?
}

impl Config {
    pub async fn load() -> io::Result<&'static Config> {
        static CONFIG: OnceCell<Config> = OnceCell::const_new();
        CONFIG.get_or_try_init(Self::load_inner).await
    }

    async fn load_inner() -> io::Result<Config> {
        // TODO: This "should" care about stuff like $XDG_CONFIG_HOME, but I've clearly documented
        // that this tool is designed for my use alone.
        //
        // I can state with certainty that boi will **NEVER** use any "platform config dirs" crate
        // that resolves to ~/Library paths on macOS, regardless of how else I tweak this. It will
        // ONLY default to XDG paths that are standard on other Unix-like platforms.
        let path = match env::home_dir() {
            None => die!("Can't find $HOME; where do I load your config from?"),
            Some(mut path) => {
                path.push(".config");
                path.push("boi");
                path.push("boi.toml");
                path
            }
        };
        let content = tokio::fs::read_to_string(path).await?;
        toml::from_str(&content).map_err(io::Error::other)
    }
}
