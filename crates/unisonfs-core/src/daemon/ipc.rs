//! Unix socket IPC server for the daemon.

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Notify;

use super::protocol::{Request, Response};

/// Run the IPC server until the shutdown notify fires.
pub async fn serve(
    socket_path: &std::path::Path,
    shutdown: Arc<Notify>,
    handle_request: impl Fn(Request) -> Response + Send + Sync + 'static,
) -> Result<()> {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    let handle_request = Arc::new(handle_request);

    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _)) => {
                        let handle_request = handle_request.clone();
                        tokio::spawn(async move {
                            let (reader, mut writer) = stream.into_split();
                            let mut reader = BufReader::new(reader);
                            let mut line = String::new();
                            if reader.read_line(&mut line).await.is_ok() && !line.is_empty() {
                                if let Ok(req) = serde_json::from_str::<Request>(line.trim()) {
                                    let resp = handle_request(req);
                                    let mut resp_str = serde_json::to_string(&resp).unwrap_or_default();
                                    resp_str.push('\n');
                                    let _ = writer.write_all(resp_str.as_bytes()).await;
                                }
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
            _ = shutdown.notified() => break,
        }
    }

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}
