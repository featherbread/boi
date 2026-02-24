use std::borrow::Cow;
use std::env;
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::thread;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};
use serde_derive::{Deserialize, Serialize};

fn main() -> Result<(), Box<dyn Error>> {
    let transcript_path = env::args_os().nth(1).ok_or("no path provided")?;
    let transcript = File::open(transcript_path)?;

    let de = serde_json::Deserializer::from_reader(BufReader::new(transcript));
    let logs: Vec<BorgLog> = de.into_iter().collect::<Result<_, _>>()?;

    println!("Found {len} log entries.", len = logs.len());

    let bar = ProgressBar::new_spinner();
    bar.set_style(ProgressStyle::with_template("{spinner} {wide_msg}")?);

    for log in logs {
        bar.tick();
        bar.set_message(match log {
            BorgLog::ArchiveProgress { path, .. } => Cow::Owned(path),
            BorgLog::ProgressMessage { message, .. } => Cow::Owned(message),
            BorgLog::Unknown(_) => Cow::Borrowed("Doing stuff..."),
        });
        thread::sleep(Duration::from_millis(20));
    }

    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum BorgLog {
    #[serde(rename = "archive_progress")]
    ArchiveProgress {
        #[serde(default)]
        nfiles: u64,
        #[serde(default)]
        original_size: u64,
        #[serde(default)]
        compressed_size: u64,
        #[serde(default)]
        deduplicated_size: u64,
        #[serde(default)]
        path: String,
        #[serde(default)]
        finished: bool,
    },

    #[serde(rename = "progress_message")]
    ProgressMessage {
        #[serde(default)]
        msgid: String,
        #[serde(default)]
        message: String,
    },

    #[serde(untagged)]
    Unknown(serde_json::Value),
}
