//! The Unix socket: one JSON line in, one JSON line out, then the connection closes.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use buzz_hive_core::supervisor_wire::{Reply, MAX_LINE_BYTES};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::calls::Service;

/// How long a client has to send its line.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Binds the socket with mode 0600, owned by the configured user, replacing a socket a
/// stopped supervisor left behind.
pub fn bind(service: &Service) -> Result<UnixListener, String> {
    use std::os::unix::fs::FileTypeExt;
    let path = &service.config().socket;
    if let Ok(found) = std::fs::symlink_metadata(path) {
        if !found.file_type().is_socket() {
            return Err(format!("{} exists and is not a socket", path.display()));
        }
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("bind {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    if let Some(uid) = service.config().socket_uid {
        std::os::unix::fs::chown(path, Some(uid), None)
            .map_err(|e| format!("chown {}: {e}", path.display()))?;
    }
    Ok(listener)
}

pub async fn serve(service: Arc<Service>, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let service = service.clone();
                tokio::spawn(async move { connection(&service, stream).await });
            }
            Err(error) => {
                tracing::error!(%error, "accept failed");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn connection(service: &Service, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    // One byte past the limit tells a line that is too long from one that just fits.
    let mut reader = BufReader::new(read.take(MAX_LINE_BYTES as u64 + 1));
    let mut line = String::new();
    let reply = match tokio::time::timeout(READ_TIMEOUT, reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 && n <= MAX_LINE_BYTES && line.ends_with('\n') => {
            service.handle(line.trim_end(), now_ms()).await
        }
        _ => Reply::failed("unauthenticated", "the request is not a complete line"),
    };
    let mut text = serde_json::to_string(&reply).expect("a reply serializes");
    text.push('\n');
    let _ = write.write_all(text.as_bytes()).await;
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
