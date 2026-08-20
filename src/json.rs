//! Parse live streams of arbitrary JSON output.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use futures::Stream;
use serde::de::DeserializeOwned;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;
use tokio_util::io::SyncIoBridge;

/// Iterates over JSON objects streamed from an [`AsyncRead`].
///
/// Each stream spawns an independent thread (outside of any Tokio thread pool) to synchronously
/// parse JSON events via [`SyncIoBridge`], which terminates at the end of the stream or after the
/// stream is dropped.
///
/// # Why [`SyncIoBridge`]?
///
/// A purely asynchronous implementation would either:
///
/// 1. Require full buffering, inhibiting the use of these streams to update live state like
///    terminal progress bars
///
/// 2. Impose additional restrictions on the input format, like requiring JSON Lines with no
///    possibility for pretty-printing
///
/// For boi in particular, lifting the second restriction simplifies compatibility with Borg
/// versions that don't implement `$BORG_JSON_INDENT` (v1.4.5+) without requiring the test harness
/// to emulate Borg's splitting of pretty and non-pretty JSON between stdout and stderr.
pub struct JsonStream<T>(mpsc::UnboundedReceiver<serde_json::Result<T>>)
where
    T: DeserializeOwned + Send + 'static;

impl<T> JsonStream<T>
where
    T: DeserializeOwned + Send + 'static,
{
    pub fn new<R>(reader: R) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let sync_reader = SyncIoBridge::new(reader);
        let (tx, rx) = mpsc::unbounded_channel();

        thread::spawn(move || {
            serde_json::Deserializer::from_reader(sync_reader)
                .into_iter()
                .try_for_each(|log| tx.send(log))
        });

        Self(rx)
    }
}

impl<T> Stream for JsonStream<T>
where
    T: DeserializeOwned + Send + 'static,
{
    type Item = serde_json::Result<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}
