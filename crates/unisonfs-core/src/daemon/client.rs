//! IPC client — used by the parent CLI to talk to a running daemon.

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::protocol::{Request, Response};

/// Send a single request to the daemon and await its response.
pub async fn send_request(tag: &str, req: Request) -> Result<Response> {
    let socket_path = super::socket_path(tag);
    let stream = UnixStream::connect(&socket_path)
        .await
        .with_context(|| format!("cannot connect to daemon socket {}", socket_path.display()))?;

    let (reader, mut writer) = stream.into_split();

    let mut req_str = serde_json::to_string(&req)?;
    req_str.push('\n');
    writer.write_all(req_str.as_bytes()).await?;
    writer.shutdown().await?;

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    serde_json::from_str(line.trim()).context("failed to parse daemon response")
}
