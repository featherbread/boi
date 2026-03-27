use std::borrow::Cow;
use std::env;

use url::Url;

use crate::child::{self, Child};

pub async fn main() -> child::Result<()> {
    let Ok(repo) = env::var("BORG_REPO")
        .map_err(|err| die!("Can't read $BORG_REPO ({err}); what do I upload?"));

    let Ok(repo) = Url::parse(&repo)
        .map_err(|err| die!("Can't parse $BORG_REPO as a URL ({err}); what do I upload?"));

    if repo.scheme() != "ssh" {
        die!("$BORG_REPO isn't an ssh:// URL; where do I connect?");
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
