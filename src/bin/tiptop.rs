use std::borrow::Cow;
use std::env;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use indicatif::style::ProgressTracker;
use indicatif::{HumanBytes, ProgressBar, ProgressState, ProgressStyle};
use serde::de::IgnoredAny;
use serde_derive::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};

#[path = "../macros.rs"]
#[macro_use]
#[allow(unused_macros)]
mod macros;

fn main() -> Result<(), Box<dyn Error>> {
    let transcript_path = env::args_os().nth(1).ok_or("no path provided")?;
    let transcript = File::open(transcript_path)?;

    let last_stats = Arc::new(RwLock::new(ArchiveStats::default()));
    let bar = ProgressBar::new_spinner();
    bar.set_style(ArchiveStats::bar_style(&last_stats));
    bar.enable_steady_tick(Duration::from_millis(100));

    let mut warned_once = false;
    let mut warn_once = |msg: &str| {
        if !warned_once {
            bar.suspend(|| speak!("{msg}"));
        }
        warned_once = true;
    };

    let mut final_stats = None;
    let de = serde_json::Deserializer::from_reader(BufReader::new(transcript));
    for raw_log in de.into_iter::<BorgJson>() {
        let raw_event = match raw_log {
            Ok(BorgJson::CreateEvent(raw_event)) => raw_event,
            Ok(BorgJson::FinalStats(stats)) => {
                final_stats = Some(stats);
                continue;
            }
            Ok(BorgJson::Unknown(_)) => {
                warn_once("Unrecognized log entry from Borg");
                continue;
            }
            Err(err) => {
                warn_once(&format!(
                    "Ignoring further Borg output due to JSON error: {err}"
                ));
                break;
            }
        };

        let bar_message = match CreateEvent::from(raw_event) {
            CreateEvent::ProgressMessage(msg) if msg.is_empty() => continue,
            CreateEvent::ProgressMessage(msg) => Cow::Owned(msg),
            CreateEvent::ArchiveFinished => Cow::Borrowed("Finished archiving files"),
            CreateEvent::ArchiveProgress(progress) => {
                *last_stats.write().unwrap() = progress.stats;
                Cow::Owned(progress.path)
            }
            CreateEvent::UnknownType(msg_type) => {
                warn_once(&format!("Unrecognized {msg_type} event from Borg"));
                continue;
            }
        };

        bar.set_message(bar_message);
        thread::sleep(Duration::from_millis(150));
    }

    let mut duration = None;
    if let Some(stats) = final_stats {
        duration = Some(stats.archive.duration);
        *last_stats.write().unwrap() = stats.archive.stats;
    }

    bar.set_message("Waiting for Borg to exit.");
    thread::sleep(Duration::from_secs(2));

    // NOTE: Providing just one tick string introduces a panic risk.
    bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✓", "✓"]));
    if let Some(duration) = duration {
        bar.finish_with_message(format!("Finished creating archive in {duration} seconds.",));
    } else {
        bar.finish_with_message("Finished creating archive.");
    }

    Ok(())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BorgJson {
    CreateEvent(CreateJson),
    FinalStats(StatsJson),
    Unknown(IgnoredAny),
}

#[derive(Deserialize)]
struct StatsJson {
    archive: StatsArchiveJson,
}

#[derive(Deserialize)]
struct StatsArchiveJson {
    duration: f64,
    stats: ArchiveStats,
}

#[derive(Deserialize)]
struct CreateJson {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

enum CreateEvent {
    ArchiveProgress(ArchiveProgress),
    ArchiveFinished,
    ProgressMessage(String),
    UnknownType(String),
}

impl From<CreateJson> for CreateEvent {
    fn from(raw: CreateJson) -> Self {
        match raw.msg_type.as_str() {
            "archive_progress" if raw.rest.get("finished") == Some(&json!(true)) => {
                CreateEvent::ArchiveFinished
            }
            "archive_progress" => {
                serde_json::from_value::<ArchiveProgress>(JsonValue::Object(raw.rest))
                    .map(CreateEvent::ArchiveProgress)
                    .unwrap_or(CreateEvent::UnknownType(raw.msg_type))
            }
            "progress_message" => CreateEvent::ProgressMessage(
                raw.rest
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => CreateEvent::UnknownType(raw.msg_type),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ArchiveProgress {
    path: String,
    #[serde(flatten)]
    stats: ArchiveStats,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ArchiveStats {
    nfiles: u64,
    original_size: u64,
    compressed_size: u64,
    deduplicated_size: u64,
}

impl ArchiveStats {
    fn bar_style(stats: &Arc<RwLock<ArchiveStats>>) -> ProgressStyle {
        ProgressStyle::with_template("{spinner} {nfiles} N {orig} S {comp} C {ddup} D • {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key(
                "nfiles",
                ArchiveStatsTracker(Arc::clone(stats), ArchiveStats::bar_nfiles),
            )
            .with_key(
                "orig",
                ArchiveStatsTracker(Arc::clone(stats), ArchiveStats::bar_orig),
            )
            .with_key(
                "comp",
                ArchiveStatsTracker(Arc::clone(stats), ArchiveStats::bar_comp),
            )
            .with_key(
                "ddup",
                ArchiveStatsTracker(Arc::clone(stats), ArchiveStats::bar_ddup),
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
struct ArchiveStatsTracker<F>(Arc<RwLock<ArchiveStats>>, F);

impl<F, T> ProgressTracker for ArchiveStatsTracker<F>
where
    F: Fn(&ArchiveStats) -> T + Clone + Send + Sync + 'static,
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
