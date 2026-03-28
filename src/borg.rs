use std::fmt::{self, Display};

use futures::{Stream, StreamExt, stream};
use indicatif::HumanBytes;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_derive::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use tokio::io::AsyncRead;

use crate::json::JsonStream;

pub fn stream<R>(reader: R) -> impl Stream<Item = serde_json::Result<Event>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut raw_stream: JsonStream<Raw> = JsonStream::new(reader);
    stream::poll_fn(move |cx| raw_stream.poll_next_unpin(cx).map_ok(Event::from))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Raw {
    Typed(TypedRaw),
    CreateStats(CreateStatsRaw),
    Unknown(IgnoredAny),
}

#[derive(Deserialize)]
struct CreateStatsRaw {
    archive: ArchiveComplete,
}

#[derive(Deserialize)]
struct TypedRaw {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

impl TypedRaw {
    fn message(&self) -> Option<&str> {
        self.rest["message"].as_str().filter(|s| !s.is_empty())
    }

    fn rest_into<T, F>(self, map: F) -> Event
    where
        F: FnOnce(T) -> Event,
        T: DeserializeOwned,
    {
        serde_json::from_value::<T>(JsonValue::Object(self.rest))
            .map(map)
            .unwrap_or(Event::Unknown(Some(self.msg_type)))
    }

    fn progress_into<T, F>(self, map: F) -> Event
    where
        F: FnOnce(Progress<T>) -> Event,
        T: DeserializeOwned,
    {
        if self.rest["finished"] == JsonValue::Bool(true) {
            map(Progress::Finished)
        } else {
            self.rest_into(|rest| map(Progress::Running(rest)))
        }
    }
}

pub enum Event {
    Blank,
    LogMessage(LogMessage),
    ProgressMessage(String),
    ProgressPercent(Progress<ProgressPercent>),
    ArchiveProgress(Progress<ArchiveProgress>),
    ArchiveComplete(ArchiveComplete),
    Unknown(Option<String>),
}

impl From<Raw> for Event {
    fn from(raw: Raw) -> Self {
        match raw {
            Raw::Unknown(_) => Event::Unknown(None),
            Raw::CreateStats(stats) => Event::ArchiveComplete(stats.archive),
            Raw::Typed(typed) => match typed.msg_type.as_str() {
                "log_message" => match typed.message() {
                    Some(_) => typed.rest_into(Event::LogMessage),
                    None => Event::Blank,
                },

                "progress_message" => match typed.message() {
                    Some(msg) => Event::ProgressMessage(msg.to_owned()),
                    None => Event::Blank,
                },

                "progress_percent" => typed.progress_into(Event::ProgressPercent),
                "archive_progress" => typed.progress_into(Event::ArchiveProgress),

                _ => Event::Unknown(Some(typed.msg_type)),
            },
        }
    }
}

#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(field_identifier)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
    Other(String),
}

#[derive(Deserialize)]
pub struct LogMessage {
    #[serde(rename = "levelname")]
    pub level: LogLevel,
    pub message: String,
}

pub enum Progress<T> {
    Finished,
    Running(T),
}

#[derive(Deserialize)]
pub struct ProgressPercent {
    pub current: u64,
    pub total: u64,
    pub message: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct ArchiveProgress {
    pub path: String,
    #[serde(flatten)]
    pub stats: ArchiveStats,
}

#[derive(Deserialize)]
pub struct ArchiveComplete {
    pub duration: f64,
    pub stats: ArchiveStats,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct ArchiveStats {
    pub nfiles: u64,
    pub original_size: u64,
    pub compressed_size: u64,
    pub deduplicated_size: u64,
}

impl Display for ArchiveStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{nfiles} N {orig} S {comp} C {ddup} D",
            nfiles = self.nfiles,
            orig = HumanBytes(self.original_size),
            comp = HumanBytes(self.compressed_size),
            ddup = HumanBytes(self.deduplicated_size),
        )
    }
}
