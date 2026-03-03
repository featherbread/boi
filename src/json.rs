use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use serde::de::DeserializeOwned;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;
use tokio_util::io::SyncIoBridge;

/// Iterates over JSON objects streamed from an [`AsyncRead`].
///
/// This synchronously parses the JSON events in another thread via [`SyncIoBridge`].
/// Why can't we use a pure async solution?
///
/// 1. We can't buffer all the raw JSON into memory (as the `SyncIoBridge` docs suggest),
///    since it may be used to update live state like progress bars.
///
/// 2. We can't iterate over lines, since not all JSON streams are guaranteed to use the JSON Lines
///    format. In particular, Borg is known to pretty-print non-log JSON output.
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

        tokio::task::spawn_blocking(move || {
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
