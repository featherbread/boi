use std::env;
use std::fmt::Display;

use indexmap::IndexMap;
use serde_derive::Deserialize;
use tokio::io;
use tokio::sync::OnceCell;

#[derive(Deserialize)]
pub struct Config {
    /// Common settings that may apply to any subcommand.
    global: GlobalConfig,

    /// Settings for individual Borg repositories.
    #[serde(default)]
    repos: IndexMap<String, RepoConfig>,
}

#[derive(Deserialize)]
pub struct GlobalConfig {
    /// A timezone to use for all timezone-sensitive commands, regardless of system-wide settings.
    /// Borg's `prune` command is sensitive to timezones when computing the set of archives to
    /// delete, so **you may lose data** if you change this on a mature repository.
    timezone: String,
}

#[derive(Deserialize)]
pub struct RepoConfig {
    /// The repository URL, i.e. `$BORG_REPO`; see
    /// https://borgbackup.readthedocs.io/en/stable/usage/general.html#repository-urls.
    repo_url: String,

    /// A command Borg will run to get the repository's passphrase, i.e. `$BORG_PASSCOMMAND`.
    /// This must work non-interactively, and should read the passphrase from a suitable credential
    /// manager that doesn't store passphrases in plain text on disk.
    password_command: String,
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
            Some(path) => path.join(".config").join("boi").join("boi.toml"),
            None => die!("Can't find $HOME; where do I load your config from?"),
        };
        let content = tokio::fs::read_to_string(path).await?;
        toml::from_str(&content).map_err(Into::into)
    }

    pub fn global(&self) -> &GlobalConfig {
        &self.global
    }

    pub fn one_or_die(&self) -> &RepoConfig {
        match self.repos.get_index(0) {
            Some((_, config)) if self.repos.len() == 1 => config,
            Some(_) => die!("Found more than one repo in your config; you'll need to pick one."),
            None => die!("Can't find any repos in your config; what do I operate on?"),
        }
    }

    pub fn get_or_die(&self, name: &str) -> &RepoConfig {
        match self.repos.get(name) {
            Some(config) => config,
            None => die!("Can't find any {name:?} repo in your config; what do I operate on?"),
        }
    }
}

impl GlobalConfig {
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
}

impl RepoConfig {
    pub fn env(&self) -> impl Iterator<Item = (&'static str, String)> {
        IntoIterator::into_iter([
            ("BORG_REPO", self.repo_url.clone()),
            ("BORG_PASSCOMMAND", self.password_command.clone()),
        ])
    }

    pub fn repo_url(&self) -> &str {
        &self.repo_url
    }
}

#[derive(Debug)]
pub enum Error {
    /// The config file is missing or can't be opened.
    Open(io::Error),
    /// The config file isn't valid TOML. Note that `toml::de::Error` has an unusual multi-line
    /// `Display` impl that's best rendered with a blank line separating it from earlier text.
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
            Self::Parse(err) => Display::fmt(err.message(), f),
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
