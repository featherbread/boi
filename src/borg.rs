use futures::{stream, Stream, StreamExt};
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
    Unknown(IgnoredAny),
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
        self.rest
            .get("message")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
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
        if self.rest.get("finished") == Some(&JsonValue::Bool(true)) {
            map(Progress::Finished)
        } else {
            self.rest_into(|rest| map(Progress::Running(rest)))
        }
    }
}

pub enum Event {
    Blank,
    LogMessage(LogMessage),
    ProgressPercent(Progress<ProgressPercent>),
    Unknown(Option<String>),
}

impl From<Raw> for Event {
    fn from(raw: Raw) -> Self {
        let Raw::Typed(typed) = raw else {
            return Event::Unknown(None);
        };

        match typed.msg_type.as_str() {
            "log_message" if typed.message().is_none() => Event::Blank,
            "log_message" => typed.rest_into(Event::LogMessage),

            "progress_percent" => typed.progress_into(Event::ProgressPercent),

            _ => Event::Unknown(Some(typed.msg_type)),
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
