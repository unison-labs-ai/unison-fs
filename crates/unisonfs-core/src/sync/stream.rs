//! Layer-2 wake stream — holds the server's SSE doorbell
//! (`GET /v1/brain/changes/stream`) open and fires the delta loop's wake
//! `Notify` on every `changed` event, collapsing sync latency from the poll
//! interval to round-trip time.
//!
//! Correctness never depends on this stream: events carry no data (the delta
//! loop's cursor query is the source of truth) and the interval poll keeps
//! running underneath — stretched to a slow fallback while the stream is
//! healthy, back to the fast cadence the moment it drops.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Notify};

use crate::api::{ApiClient, ApiError};

/// No bytes for this long means the stream is dead even if TCP hasn't
/// noticed — the server heartbeats every 30s, so 90s is three misses.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Run the wake-stream loop until shutdown. Sets `healthy` while a stream is
/// open (the delta loop stretches its poll interval accordingly). Exits for
/// good if the server has no stream route (pre-feed server).
pub async fn run_stream_loop(
    api: Arc<ApiClient>,
    wake: Arc<Notify>,
    healthy: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        if *shutdown.borrow() {
            return;
        }
        match api.open_changes_stream().await {
            Ok(resp) => {
                tracing::info!("change stream connected");
                healthy.store(true, Ordering::Relaxed);
                backoff = BACKOFF_MIN;
                // Cover whatever changed while we were disconnected.
                wake.notify_one();
                let reason = consume_stream(resp, &wake, &mut shutdown).await;
                healthy.store(false, Ordering::Relaxed);
                match reason {
                    StreamEnd::Shutdown => return,
                    StreamEnd::Closed => {
                        // Server caps stream lifetime (hourly reconnect) —
                        // an orderly close reconnects immediately.
                        tracing::debug!("change stream closed; reconnecting");
                    }
                    StreamEnd::Idle => {
                        tracing::warn!("change stream idle past heartbeat window; reconnecting");
                    }
                    StreamEnd::Error(e) => {
                        tracing::warn!(error = %e, "change stream errored; reconnecting");
                    }
                }
            }
            Err(ApiError::NotFound) => {
                tracing::info!("server has no change stream; staying on interval polling");
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "change stream connect failed");
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

enum StreamEnd {
    Shutdown,
    Closed,
    Idle,
    Error(reqwest::Error),
}

/// Read the SSE byte stream, firing `wake` on every `changed` event.
async fn consume_stream(
    mut resp: reqwest::Response,
    wake: &Notify,
    shutdown: &mut watch::Receiver<bool>,
) -> StreamEnd {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = tokio::select! {
            c = tokio::time::timeout(IDLE_TIMEOUT, resp.chunk()) => c,
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return StreamEnd::Shutdown; }
                continue;
            }
        };
        match chunk {
            Err(_) => return StreamEnd::Idle,
            Ok(Err(e)) => return StreamEnd::Error(e),
            Ok(Ok(None)) => return StreamEnd::Closed,
            Ok(Ok(Some(bytes))) => {
                buf.extend_from_slice(&bytes);
                // SSE events are separated by a blank line. Heartbeat
                // comments (`: hb`) parse to no event name and are dropped.
                while let Some(pos) = find_event_boundary(&buf) {
                    let event: Vec<u8> = buf.drain(..pos + 2).collect();
                    if event_name(&event).is_some_and(|n| n == "changed") {
                        wake.notify_one();
                    }
                }
                // Runaway guard: no legitimate event is this large.
                if buf.len() > 64 * 1024 {
                    buf.clear();
                }
            }
        }
    }
}

fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn event_name(event: &[u8]) -> Option<&str> {
    for line in event.split(|&b| b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"event:") {
            let name = std::str::from_utf8(rest).ok()?.trim();
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_name() {
        assert_eq!(event_name(b"event: changed\ndata: {}\n\n"), Some("changed"));
        assert_eq!(event_name(b"event:hello\ndata: {}\n\n"), Some("hello"));
        assert_eq!(event_name(b": hb\n\n"), None);
        assert_eq!(event_name(b"data: {}\n\n"), None);
    }

    #[test]
    fn finds_boundaries_incrementally() {
        let mut buf = b"event: changed\ndata: {}".to_vec();
        assert!(find_event_boundary(&buf).is_none());
        buf.extend_from_slice(b"\n\n: hb\n\n");
        let pos = find_event_boundary(&buf).unwrap();
        let first: Vec<u8> = buf.drain(..pos + 2).collect();
        assert_eq!(event_name(&first), Some("changed"));
        assert!(find_event_boundary(&buf).is_some());
    }
}
