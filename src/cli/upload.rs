use std::borrow::Cow;
use std::env;

use url::Url;

use crate::child::{self, Child};
use crate::config::Config;

#[derive(clap::Args)]
pub struct Args {
    /// The repository to work on
    repository: Option<String>,
}

pub async fn main(args: Args) -> child::Result<()> {
    let config = Config::load_or_die().await;
    let repo = match &args.repository {
        Some(name) => config.get_or_die(name),
        None => config.one_or_die(),
    };

    let Ok(repo) = Url::parse(repo.repo_url())
        .map_err(|err| die!("Can't parse configured repo_url ({err}); what do I upload?"));

    if repo.scheme() != "ssh" {
        die!("Configured repo_url isn't an ssh:// URL; where do I connect?");
    }

    let port = repo.port().map(|port| port.to_string());
    let destination = match (repo.host_str(), repo.username()) {
        (None, _) => die!("Can't find SSH hostname of borg repo; where do I connect?"),
        (Some(host), "") => Cow::Borrowed(host),
        (Some(host), user) => Cow::Owned(format!("{user}@{host}")),
    };

    let mut cmdline = vec!["ssh", "-t"];
    if let Some(port) = port.as_ref() {
        cmdline.extend(["-p", port]);
    }
    cmdline.extend([destination.as_ref(), "boi-upload", repo.path()]);

    Child::from_cmdline(&cmdline).complete().await
}
