use std::time::Duration;

use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use serde::de::IgnoredAny;
use serde_derive::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use tokio::process::ChildStdout;

use crate::child::{self, Child, Spawn};
use crate::json::JsonStream;

#[derive(clap::Args)]
pub struct Args {
    /// Only perform repository checks (chunk CRCs)
    #[arg(long)]
    repository_only: bool,
}

pub async fn main(args: Args) -> child::Result<()> {
    let mut cmdline = vec!["borg", "check", "-v", "--progress"];
    if args.repository_only {
        cmdline.push("--repository-only");
    }
    let (spawn, output) = Child::from_cmdline(&cmdline).spawn_with_output()?;
    render(spawn, output).await;
    Ok(())
}

async fn render(mut spawn: Spawn, output: ChildStdout) {
    let style = ProgressStyle::with_template("[boi] {spinner} {bar} {pos}/{len} • {wide_msg}")
        .expect("hardcoded ProgressStyle template should be valid");

    let new_waiting_spinner = || {
        let bar = ProgressBar::no_length();
        bar.set_style(style.clone());
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.set_message("Waiting for Borg");
        bar
    };

    let mut bar = new_waiting_spinner();
    let mut warned_once = false;

    macro_rules! warn_once {
        ( $( $speak_args:tt ),* ) => {{
            if !warned_once {
                bar.suspend(|| speak!("⚑", $($speak_args),*));
            }
            warned_once = true;
        }};
    }

    let mut output_stream = JsonStream::new(output);
    while let Some(raw_log) = output_stream.next().await {
        let raw_event = match raw_log {
            Ok(BorgJson::CheckEvent(raw_event)) => raw_event,
            Ok(BorgJson::Unknown(_)) => {
                warn_once!("Unrecognized log entry from Borg");
                continue;
            }
            Err(err) => {
                warn_once!("Ignoring further Borg output due to JSON error: {err}");
                break;
            }
        };

        let progress = match CheckEvent::from(raw_event) {
            CheckEvent::Blank => continue,
            CheckEvent::ProgressPercent(progress) => progress,
            CheckEvent::ProgressFinished => {
                bar.finish_and_clear();
                bar = new_waiting_spinner();
                continue;
            }
            CheckEvent::LogMessage(msg) => {
                bar.suspend(|| speak!("⚑", "{msg}"));
                continue;
            }
            CheckEvent::Unrecognized(msg_type) => {
                warn_once!("Unrecognized {msg_type} event from Borg");
                continue;
            }
        };

        bar.set_length(progress.total);
        bar.set_position(progress.current);
        bar.set_message(progress.message);
    }

    let child_result = match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
        Ok(result) => result,
        Err(_timeout) => {
            bar.set_message("Waiting for Borg to exit");
            spawn.wait().await
        }
    };

    bar.finish_and_clear();
    match child_result {
        Ok(status) if status.success() => {
            speak!("✓", "Repository is valid");
        }
        Ok(status) => {
            bar.set_style(style.clone().tick_strings(&["✗", "✗"]));
            if let Some(code) = status.code() {
                speak!("✗", "Borg exited with code {code}");
            } else {
                speak!("✗", "Borg terminated abnormally: {status}");
            }
        }
        Err(err) => {
            speak!("✗", "Failed to wait for Borg: {err}");
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BorgJson {
    CheckEvent(CheckJson),
    Unknown(IgnoredAny),
}

#[derive(Deserialize)]
struct CheckJson {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

impl CheckJson {
    fn message(&self) -> Option<String> {
        self.rest
            .get("message")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    }
}

enum CheckEvent {
    Blank,
    ProgressPercent(ProgressPercent),
    ProgressFinished,
    LogMessage(String),
    Unrecognized(String),
}

impl From<CheckJson> for CheckEvent {
    fn from(raw: CheckJson) -> Self {
        match raw.msg_type.as_str() {
            "progress_percent" if raw.rest.get("finished") == Some(&json!(true)) => {
                CheckEvent::ProgressFinished
            }
            "progress_percent" => {
                serde_json::from_value::<ProgressPercent>(JsonValue::Object(raw.rest))
                    .map(CheckEvent::ProgressPercent)
                    .unwrap_or(CheckEvent::Unrecognized(raw.msg_type))
            }
            "log_message" => raw
                .message()
                .map(CheckEvent::LogMessage)
                .unwrap_or(CheckEvent::Blank),

            _ => CheckEvent::Unrecognized(raw.msg_type),
        }
    }
}

#[derive(Deserialize)]
struct ProgressPercent {
    current: u64,
    total: u64,
    message: String,
}
