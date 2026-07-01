use std::env;
use std::fmt::Display;

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
    pub async fn load_or_die() -> &'static Config {
        let Ok(config) = Self::load().await.map_err(|err| err.die());
        config
    }

    pub async fn load() -> Result<&'static Config, Error> {
        static CONFIG: OnceCell<Config> = OnceCell::const_new();
        CONFIG.get_or_try_init(Self::load_inner).await
    }

    async fn load_inner() -> Result<Config, Error> {
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
        toml::from_str(&content).map_err(Into::into)
    }
}

#[derive(Debug)]
pub enum Error {
    Open(io::Error),
    Parse(toml::de::Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Open(err)
    }
}

impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Self::Parse(err)
    }
}

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(err) => Display::fmt(err, f),
            Self::Parse(err) => Display::fmt(err, f),
        }
    }
}

impl Error {
    pub fn die(&self) -> ! {
        match self {
            Self::Open(err) => die!("Can't load the boi config ({err}); I can't do anything!"),
            Self::Parse(err) => die!("Can't load the boi config; I can't do anything!\n\n{err}"),
        }
    }
}
