use std::borrow::Cow;
use std::env;
use std::error::Error;
use std::fmt::Write;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use indicatif::style::ProgressTracker;
use indicatif::{HumanBytes, ProgressBar, ProgressState, ProgressStyle};
use serde_derive::{Deserialize, Serialize};

fn main() -> Result<(), Box<dyn Error>> {
    let transcript_path = env::args_os().nth(1).ok_or("no path provided")?;
    let transcript = File::open(transcript_path)?;

    let de = serde_json::Deserializer::from_reader(BufReader::new(transcript));
    let logs: Vec<BorgLog> = de.into_iter().collect::<Result<_, _>>()?;

    println!("Parsed {len} JSON objects.", len = logs.len());

    let total_nfiles = Arc::new(AtomicU64::new(0));
    let total_orig_bytes = Arc::new(AtomicU64::new(0));
    let total_comp_bytes = Arc::new(AtomicU64::new(0));
    let total_ddup_bytes = Arc::new(AtomicU64::new(0));

    let style = ProgressStyle::with_template(
        "{spinner} {nfiles} N {orig} S {comp} C {ddup} D • {wide_msg}",
    )?
    .with_key("nfiles", AtomicCountTracker(Arc::clone(&total_nfiles)))
    .with_key("orig", AtomicBytesTracker(Arc::clone(&total_orig_bytes)))
    .with_key("comp", AtomicBytesTracker(Arc::clone(&total_comp_bytes)))
    .with_key("ddup", AtomicBytesTracker(Arc::clone(&total_ddup_bytes)));

    let bar = ProgressBar::new_spinner();
    bar.set_style(style);
    bar.enable_steady_tick(Duration::from_millis(100));

    for log in logs {
        let message = match log {
            BorgLog::Unknown(_) => Cow::Borrowed("Doing stuff..."),
            BorgLog::ProgressMessage { message, .. } => Cow::Owned(message),
            BorgLog::ArchiveProgress {
                nfiles,
                original_size,
                compressed_size,
                deduplicated_size,
                path,
                finished,
            } => {
                if !finished {
                    total_nfiles.store(nfiles, Ordering::Relaxed);
                    total_orig_bytes.store(original_size, Ordering::Relaxed);
                    total_comp_bytes.store(compressed_size, Ordering::Relaxed);
                    total_ddup_bytes.store(deduplicated_size, Ordering::Relaxed);
                }
                Cow::Owned(path)
            }
        };
        if !message.is_empty() {
            bar.set_message(message);
        }
        thread::sleep(Duration::from_millis(150));
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

#[derive(Clone)]
struct AtomicCountTracker(Arc<AtomicU64>);

impl ProgressTracker for AtomicCountTracker {
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(self.clone())
    }

    fn tick(&mut self, _: &ProgressState, _: Instant) {}

    fn reset(&mut self, _: &ProgressState, _: Instant) {}

    fn write(&self, _: &ProgressState, w: &mut dyn Write) {
        let _ = write!(w, "{}", self.0.load(Ordering::Relaxed));
    }
}

#[derive(Clone)]
struct AtomicBytesTracker(Arc<AtomicU64>);

impl ProgressTracker for AtomicBytesTracker {
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(self.clone())
    }

    fn tick(&mut self, _: &ProgressState, _: Instant) {}

    fn reset(&mut self, _: &ProgressState, _: Instant) {}

    fn write(&self, _: &ProgressState, w: &mut dyn Write) {
        let _ = write!(w, "{}", HumanBytes(self.0.load(Ordering::Relaxed)));
    }
}
