//! Local IPC for browser integration (spec §21, §113).
//!
//! Security model:
//! * Unix domain socket in the user's runtime dir, chmod 0600.
//! * Per-run random token written to a 0600 file; every message must carry it.
//! * Messages are newline-delimited JSON, capped at 1 MiB.
//! * Only two operations exist: `ping` and `add_download`. No arbitrary
//!   commands, no filesystem access beyond the download itself.
//! * The socket is never exposed on TCP.

use crate::error::{EngineError, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct IpcConfig {
    pub runtime_dir: PathBuf,
}

/// Operations the IPC surface accepts.
#[derive(Debug, Clone)]
pub enum IpcOp {
    Ping,
    AddDownload {
        url: String,
        filename: Option<String>,
        dir: Option<String>,
        referrer: Option<String>,
        cookies: Vec<(String, String)>,
        category: Option<String>,
        connections: Option<i32>,
    },
}

#[derive(Debug)]
pub struct IpcRequest {
    pub op: IpcOp,
    pub reply: oneshot::Sender<IpcResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl IpcResponse {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            error: None,
            data: Some(data),
        }
    }
    pub fn error(message: String) -> Self {
        Self {
            ok: false,
            error: Some(message),
            data: None,
        }
    }
}

#[derive(Deserialize)]
struct WireMessage {
    v: Option<u32>,
    auth: Option<String>,
    op: String,
    url: Option<String>,
    filename: Option<String>,
    dir: Option<String>,
    referrer: Option<String>,
    cookies: Option<Vec<(String, String)>>,
    category: Option<String>,
    connections: Option<i32>,
}

pub struct IpcServer {
    pub socket_path: PathBuf,
    #[allow(dead_code)]
    token: String,
    rx: Option<mpsc::UnboundedReceiver<IpcRequest>>,
    stop: Arc<AtomicBool>,
    _listener: Arc<tokio::net::UnixListener>,
}

impl IpcServer {
    pub fn start(cfg: IpcConfig) -> Result<Self> {
        let dir = cfg.runtime_dir;
        std::fs::create_dir_all(&dir)
            .map_err(|e| EngineError::permission(format!("cannot create runtime dir: {e}")))?;
        let socket_path = dir.join("ldm.sock");
        // Remove a stale socket from a previous run.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .map_err(|e| EngineError::permission(format!("cannot bind socket: {e}")))?;
        // Restrict permissions (best-effort; umask may already restrict).
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));

        let token: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        // Token file for the native-messaging host (same user only).
        let token_path = dir.join("ldm.token");
        let _ = std::fs::write(&token_path, &token);
        let _ = std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));

        let (tx, rx) = mpsc::unbounded_channel::<IpcRequest>();
        let stop = Arc::new(AtomicBool::new(false));
        let listener = Arc::new(listener);
        let server = Self {
            socket_path,
            token: token.clone(),
            rx: Some(rx),
            stop: stop.clone(),
            _listener: listener.clone(),
        };

        // Accept loop.
        let tx2 = tx.clone();
        let token2 = token.clone();
        let stop2 = stop.clone();
        tokio::spawn(async move {
            loop {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let (mut stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let token3 = token2.clone();
                let tx3 = tx2.clone();
                tokio::spawn(async move {
                    handle_connection(&mut stream, &token3, &tx3).await;
                });
            }
        });

        Ok(server)
    }

    pub fn try_recv(&mut self) -> std::result::Result<IpcRequest, mpsc::error::TryRecvError> {
        match &mut self.rx {
            Some(rx) => rx.try_recv(),
            None => Err(mpsc::error::TryRecvError::Disconnected),
        }
    }

    pub fn stop(&self) -> bool {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.socket_path);
        true
    }
}

async fn handle_connection(
    stream: &mut tokio::net::UnixStream,
    token: &str,
    tx: &mpsc::UnboundedSender<IpcRequest>,
) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        if n > MAX_MESSAGE_BYTES || line.len() > MAX_MESSAGE_BYTES {
            let _ = write_response(reader.get_mut(), IpcResponse::error("message too large".into())).await;
            return;
        }
        let response = handle_message(&line, token, tx).await;
        if !write_response(reader.get_mut(), response).await {
            return;
        }
    }
}

async fn write_response(
    stream: &mut tokio::net::UnixStream,
    resp: IpcResponse,
) -> bool {
    let json = serde_json::to_string(&resp).unwrap_or_else(|_| "{\"ok\":false}".into());
    stream.write_all(json.as_bytes()).await.is_ok()
        && stream.write_all(b"\n").await.is_ok()
        && stream.flush().await.is_ok()
}

async fn handle_message(
    line: &str,
    token: &str,
    tx: &mpsc::UnboundedSender<IpcRequest>,
) -> IpcResponse {
    let msg: WireMessage = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => return IpcResponse::error(format!("invalid message: {e}")),
    };
    // Protocol version check.
    if msg.v != Some(PROTOCOL_VERSION) {
        return IpcResponse::error(format!(
            "unsupported protocol version (got {:?}, need {PROTOCOL_VERSION})",
            msg.v
        ));
    }
    // Token authentication.
    if msg.auth.as_deref() != Some(token) {
        return IpcResponse::error("authentication failed".into());
    }
    match msg.op.as_str() {
        "ping" => IpcResponse::ok(serde_json::json!({"pong": true})),
        "add_download" => {
            let url = match msg.url {
                Some(u) => u,
                None => return IpcResponse::error("missing url".into()),
            };
            if url.len() > 4096 {
                return IpcResponse::error("url too long".into());
            }
            if let Err(e) = crate::urlutil::validate_url(&url) {
                return IpcResponse::error(e.message);
            }
            let (reply_tx, reply_rx) = oneshot::channel();
            let op = IpcOp::AddDownload {
                url,
                filename: msg.filename.filter(|f| f.len() <= 512),
                dir: msg.dir.filter(|d| d.len() <= 4096),
                referrer: msg.referrer.filter(|r| r.len() <= 4096),
                cookies: msg.cookies.unwrap_or_default().into_iter().take(64).collect(),
                category: msg.category.filter(|c| c.len() <= 128),
                connections: msg.connections.map(|c| c.clamp(1, 32)),
            };
            if tx.send(IpcRequest { op, reply: reply_tx }).is_err() {
                return IpcResponse::error("engine unavailable".into());
            }
            match tokio::time::timeout(std::time::Duration::from_secs(30), reply_rx).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(_)) => IpcResponse::error("engine dropped the request".into()),
                Err(_) => IpcResponse::error("engine timed out".into()),
            }
        }
        other => IpcResponse::error(format!("unknown operation: {other}")),
    }
}
