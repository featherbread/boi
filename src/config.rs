use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::{env, iter};

use indexmap::IndexMap;
use serde_derive::Deserialize;
use thiserror::Error;
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

#[derive(Deserialize, Clone)]
pub struct RepoConfig {
    /// The repository URL, i.e. `$BORG_REPO`; see
    /// <https://borgbackup.readthedocs.io/en/stable/usage/general.html#repository-urls>.
    repo_url: String,

    /// A command Borg will run to get the repository's passphrase, i.e. `$BORG_PASSCOMMAND`.
    /// This must work non-interactively, and should read the passphrase from a suitable credential
    /// manager that doesn't store passphrases in plain text on disk.
    password_command: String,

    /// The path to the borg binary on the remote host, i.e. `$BORG_REMOTE_PATH`.
    /// Required by some Borg hosting services.
    remote_path: Option<String>,

    /// Allow `boi prune --profile=aggressive` on this repo.
    #[serde(default)]
    allow_aggressive_prune: bool,
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
        let Some(path) = Self::config_path().await else {
            die!("Can't find your boi.toml; what do I do?");
        };
        let content = tokio::fs::read_to_string(path).await?;
        let config: Config = toml::from_str(&content)?;
        (!config.repos.is_empty())
            .then_some(config)
            .ok_or(Error::NoRepos)
    }

    async fn config_path() -> Option<PathBuf> {
        // If $BOI_CONFIG_DIR_PATH is explicitly set, then boi.toml _must_ be found there
        // (since you probably had a good reason to set an explicit config path).
        // TODO: Distinguish this from the case where an XDG lookup doesn't find it.
        if let Some(config_dir) = env::var_os("BOI_CONFIG_DIR_PATH") {
            let path = PathBuf::from(config_dir).join("boi.toml");
            return Self::exists(&path).await.then_some(path);
        }

        // TODO: If I used non-Unixy systems, I'd do something more comprehensive than XDG alone.
        // I can say for sure I'd never use a "platform config dirs" crate that touches ~/Library
        // on macOS; only directories that are standard on other Unixy platforms. (`etcetera` seems
        // to be the one good choice by this metric.)
        let xdg_config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::home_dir().map(|dir| dir.join(".config")));

        let xdg_config_dirs = env::var_os("XDG_CONFIG_DIRS").unwrap_or(OsString::from("/etc/xdg"));
        let xdg_config_dirs = env::split_paths(&xdg_config_dirs);

        for config_dir in iter::chain(xdg_config_home, xdg_config_dirs) {
            let path = config_dir.join("boi").join("boi.toml");
            if Self::exists(&path).await {
                return Some(path);
            }
        }

        None
    }

    async fn exists(path: impl AsRef<Path>) -> bool {
        tokio::fs::try_exists(path).await.ok() == Some(true)
    }

    pub fn global(&self) -> &GlobalConfig {
        &self.global
    }

    pub fn repos(&self) -> impl Iterator<Item = (&str, &RepoConfig)> {
        self.repos.iter().map(|(name, repo)| (name.as_str(), repo))
    }

    pub fn select_repos_or_die<'me, 'names, S>(
        &'me self,
        names: &'names [S],
    ) -> impl Iterator<Item = (&'me str, &'me RepoConfig)> + 'names
    where
        'me: 'names,
        S: AsRef<str>,
    {
        let want_set: HashSet<&str> = HashSet::from_iter(names.iter().map(AsRef::as_ref));
        if want_set.len() != names.len() {
            die!("Requested a repo more than once; remove your duplicates and try again.");
        }

        let have_set: HashSet<&str> = HashSet::from_iter(self.repos.keys().map(AsRef::as_ref));
        let missing_set = &want_set - &have_set;
        if !missing_set.is_empty() {
            die!("Requested unknown repos; what do I operate on?",);
        }

        let selected_set = &want_set & &have_set;
        self.repos()
            .filter(move |(name, _)| selected_set.contains(name))
    }

    pub fn one_or_die(&self) -> (&str, &RepoConfig) {
        debug_assert!(
            !self.repos.is_empty(),
            "config loading should have failed with no repos defined"
        );
        if self.repos.len() == 1
            && let Some((name, repo)) = self.repos.first()
        {
            (name, repo)
        } else {
            die!("Found more than one repo in your config; you'll need to pick one.");
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
    pub fn env(&self) -> impl Iterator<Item = (&'static str, &str)> {
        IntoIterator::into_iter([
            ("BORG_REPO", self.repo_url.as_str()),
            ("BORG_PASSCOMMAND", self.password_command.as_str()),
        ])
        .chain(
            self.remote_path
                .as_deref()
                .map(|path| ("BORG_REMOTE_PATH", path)),
        )
    }

    pub fn repo_url(&self) -> &str {
        &self.repo_url
    }

    pub fn allow_aggressive_prune(&self) -> bool {
        self.allow_aggressive_prune
    }
}

#[derive(Error, Debug)]
pub enum Error {
    /// The config file is missing or can't be opened.
    Open(#[from] io::Error),
    /// The config file isn't valid TOML. Note that `toml::de::Error` has an unusual multi-line
    /// `Display` impl that's best rendered with a blank line separating it from earlier text.
    Parse(#[from] toml::de::Error),
    /// The config file failed to define any repos.
    NoRepos,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(err) => Display::fmt(err, f),
            Self::Parse(err) => Display::fmt(err.message(), f),
            Self::NoRepos => f.write_str("no repos defined in config"),
        }
    }
}

impl Error {
    pub fn die(&self) -> ! {
        match self {
            Self::Open(err) => die!("Can't load your config ({err}); I can't do anything!"),
            Self::Parse(err) => die!("Can't load your config; I can't do anything!\n\n{err}"),
            Self::NoRepos => die!("Can't find any repos in your config; what do I operate on?"),
        }
    }
}
