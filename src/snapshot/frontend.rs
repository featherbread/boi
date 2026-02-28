use std::borrow::Cow;
use std::fmt::{self, Display};
use std::iter;
use std::pin::Pin;
use std::sync::{mpsc, Arc, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::{Stream, StreamExt};
use indicatif::style::ProgressTracker;
use indicatif::{HumanBytes, ProgressBar, ProgressState, ProgressStyle};
use serde::de::IgnoredAny;
use serde_derive::Deserialize;
use serde_json::{json, Error as JsonError, Map as JsonMap, Value as JsonValue};
use tokio::io::AsyncRead;
use tokio::process::ChildStdout;
use tokio::sync::oneshot;
use tokio_util::io::SyncIoBridge;

use crate::child::Spawn;

pub async fn render(mut spawn: Spawn, output: ChildStdout) {
    let last_stats = Arc::new(RwLock::new(ArchiveStats::default()));
    let bar = ProgressBar::new_spinner();
    bar.set_style(ArchiveStats::bar_style(&last_stats));
    bar.enable_steady_tick(Duration::from_millis(100));
    bar.set_message("Waiting for Borg to start");

    let mut warned_once = false;
    let mut warn_once = |msg: &str| {
        if !warned_once {
            bar.suspend(|| speak!("{msg}"));
        }
        warned_once = true;
    };

    let mut final_stats = None;
    let mut borg_reader = BorgJsonReader::new(output);
    while let Some(raw_log) = borg_reader.next().await {
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
            CreateEvent::LogMessage(msg) => {
                bar.suspend(|| speak!("⚑ {msg}"));
                continue;
            }
            CreateEvent::UnknownType(msg_type) => {
                warn_once(&format!("Unrecognized {msg_type} event from Borg"));
                continue;
            }
        };

        bar.set_message(bar_message);
    }

    let mut duration = None;
    if let Some(stats) = final_stats {
        duration = Some(stats.archive.duration);
        *last_stats.write().unwrap() = stats.archive.stats;
    }

    let child_result = match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
        Ok(result) => result,
        Err(_timeout) => {
            bar.set_message("Waiting for Borg to exit");
            spawn.wait().await
        }
    };

    match child_result {
        Ok(status) if status.success() => {
            // NOTE: Providing just one tick string introduces a panic risk.
            bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✓", "✓"]));
            if let Some(duration) = duration {
                bar.finish_with_message(format!("Created archive in {duration} seconds"));
            } else {
                bar.finish_with_message("Created archive");
            }
        }
        Ok(status) => {
            bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✗", "✗"]));
            if let Some(code) = status.code() {
                bar.finish_with_message(format!("Borg exited with code {code}"));
            } else {
                bar.finish_with_message(format!("Borg terminated abnormally: {status}"));
            }
        }
        Err(err) => {
            bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✗", "✗"]));
            bar.finish_with_message(format!("Failed to wait for Borg: {err}"));
        }
    }
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
    LogMessage(String),
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
            "log_message" => CreateEvent::LogMessage(
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
        ProgressStyle::with_template(
            "[boi] {spinner} {nfiles} N {orig} S {comp} C {ddup} D • {wide_msg}",
        )
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

/// Iterates over Borg JSON events piped from an [`AsyncRead`].
///
/// This synchronously parses the JSON events in another thread via [`SyncIoBridge`].
/// Why can't we use a pure async solution?
///
/// 1. We can't buffer all the raw JSON into memory (as the `SyncIoBridge` docs suggest),
///    since it's used to update a live progress bar while Borg is running.
///
/// 2. We can't iterate over lines, since Borg isn't guaranteed to emit one JSON object per line.
///    In particular, current Borg versions pretty-format the final stats object, so a line-based
///    iterator would choke on it.
struct BorgJsonReader(SyncBridge<Result<BorgJson, JsonError>>);

impl BorgJsonReader {
    fn new<R>(reader: R) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let sync_reader = SyncIoBridge::new(reader);
        let (clients, bridge) = SyncBridge::new();

        tokio::task::spawn_blocking(move || {
            let de = serde_json::Deserializer::from_reader(sync_reader);
            for (client, log) in iter::zip(clients, de.into_iter()) {
                client.send(log);
            }
        });

        Self(bridge)
    }
}

impl Stream for BorgJsonReader {
    type Item = Result<BorgJson, JsonError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }
}

/// Connects a synchronous producer to an asynchronous consumer.
///
/// [`SyncBridge::new`] returns an iterator over _clients_ that are asynchronously awaiting values
/// from the stream. The synchronous producer can:
///
///   - Yield an item to a single client by sending it
///   - Yield [`None`] to a single client by dropping the client
///   - End the stream by dropping the iterator
///
/// The client iterator is designed to be [zipped](Iterator::zip) with a regular synchronous
/// iterator, and a typical implementation is to send items to clients for as long as the zipped
/// iterator yields both.
///
/// # Cancel safety
///
/// Futures derived from the [`Stream`] are cancel safe. They may be dropped or used freely in
/// [`tokio::select`] statements without losing items.
struct SyncBridge<T> {
    clients_tx: mpsc::Sender<BridgeClient<T>>,
    next_reply: Option<Pin<Box<oneshot::Receiver<T>>>>,
}

impl<T> SyncBridge<T> {
    fn new() -> (impl Iterator<Item = BridgeClient<T>>, Self) {
        let (clients_tx, clients_rx) = mpsc::channel::<BridgeClient<T>>();
        (
            iter::from_fn(move || clients_rx.recv().ok()),
            Self {
                clients_tx,
                next_reply: None,
            },
        )
    }
}

impl<T> Stream for SyncBridge<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut rx = match self.next_reply.take() {
            Some(rx) => rx,
            None => {
                let (tx, rx) = oneshot::channel();
                match self.clients_tx.send(BridgeClient(tx)) {
                    Ok(()) => Box::pin(rx),
                    Err(_) => return Poll::Ready(None),
                }
            }
        };

        if let Poll::Ready(reply) = rx.as_mut().poll(cx) {
            return Poll::Ready(reply.ok());
        }

        self.next_reply = Some(rx);
        Poll::Pending
    }
}

struct BridgeClient<T>(oneshot::Sender<T>);

impl<T> BridgeClient<T> {
    fn send(self, item: T) {
        let _ = self.0.send(item);
    }
}
