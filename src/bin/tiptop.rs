use std::borrow::Cow;
use std::env;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs::File;
use std::io::BufReader;
use std::mem;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use indicatif::style::ProgressTracker;
use indicatif::{HumanBytes, ProgressBar, ProgressState, ProgressStyle};
use serde_derive::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};

fn main() -> Result<(), Box<dyn Error>> {
    let transcript_path = env::args_os().nth(1).ok_or("no path provided")?;
    let transcript = File::open(transcript_path)?;

    let last_progress = Arc::new(RwLock::new(ArchiveProgress::default()));
    let bar = ProgressBar::new_spinner();
    bar.set_style(ArchiveProgress::bar_style(&last_progress));
    bar.enable_steady_tick(Duration::from_millis(100));

    let de = serde_json::Deserializer::from_reader(BufReader::new(transcript));
    for raw_log in de.into_iter::<RawBorgLog>() {
        let event = BorgEvent::from(raw_log?);
        let bar_message = match event {
            BorgEvent::Unknown => Cow::Borrowed("Doing some stuff..."),
            BorgEvent::ArchiveFinished => Cow::Borrowed("Finished archiving files."),
            BorgEvent::ProgressMessage(msg) => Cow::Owned(msg),
            BorgEvent::ArchiveProgress(mut progress) => {
                let path = mem::take(&mut progress.path);
                *last_progress.write().unwrap() = progress;
                Cow::Owned(path)
            }
        };
        if !bar_message.is_empty() {
            bar.set_message(bar_message);
        }
        thread::sleep(Duration::from_millis(150));
    }

    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)] // TODO: Better handling of missing "type" field.
struct RawBorgLog {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

enum BorgEvent {
    ArchiveProgress(ArchiveProgress),
    ArchiveFinished,
    ProgressMessage(String),
    Unknown,
}

impl From<RawBorgLog> for BorgEvent {
    fn from(raw: RawBorgLog) -> Self {
        match raw.msg_type.as_str() {
            "archive_progress" if raw.rest.get("finished") == Some(&json!(true)) => {
                BorgEvent::ArchiveFinished
            }
            "archive_progress" => BorgEvent::ArchiveProgress(
                serde_json::from_value(JsonValue::Object(raw.rest)).unwrap(),
            ),
            "progress_message" => BorgEvent::ProgressMessage(
                raw.rest
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => BorgEvent::Unknown,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ArchiveProgress {
    nfiles: u64,
    original_size: u64,
    compressed_size: u64,
    deduplicated_size: u64,
    path: String,
}

impl ArchiveProgress {
    fn bar_style(progress: &Arc<RwLock<ArchiveProgress>>) -> ProgressStyle {
        ProgressStyle::with_template("{spinner} {nfiles} N {orig} S {comp} C {ddup} D • {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key(
                "nfiles",
                ArchiveProgressTracker(Arc::clone(progress), ArchiveProgress::bar_nfiles),
            )
            .with_key(
                "orig",
                ArchiveProgressTracker(Arc::clone(progress), ArchiveProgress::bar_orig),
            )
            .with_key(
                "comp",
                ArchiveProgressTracker(Arc::clone(progress), ArchiveProgress::bar_comp),
            )
            .with_key(
                "ddup",
                ArchiveProgressTracker(Arc::clone(progress), ArchiveProgress::bar_ddup),
            )
    }

    fn bar_nfiles(&self) -> u64 {
        self.nfiles
    }

    fn bar_orig(&self) -> HumanBytes {
        HumanBytes(self.original_size)
    }

    fn bar_comp(&self) -> HumanBytes {
        HumanBytes(self.compressed_size)
    }

    fn bar_ddup(&self) -> HumanBytes {
        HumanBytes(self.deduplicated_size)
    }
}

#[derive(Clone)]
struct ArchiveProgressTracker<F>(Arc<RwLock<ArchiveProgress>>, F);

impl<F, T> ProgressTracker for ArchiveProgressTracker<F>
where
    F: Fn(&ArchiveProgress) -> T + Clone + Send + Sync + 'static,
    T: Display,
{
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(self.clone())
    }

    fn tick(&mut self, _: &ProgressState, _: Instant) {}

    fn reset(&mut self, _: &ProgressState, _: Instant) {}

    fn write(&self, _: &ProgressState, w: &mut dyn fmt::Write) {
        let stat = self.1(&self.0.read().unwrap());
        let _ = write!(w, "{}", stat);
    }
}
