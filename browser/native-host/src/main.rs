//! LDM browser native-messaging host.
//!
//! Chrome and Firefox launch this binary when the LDM extension sends a
//! message. The host validates the request, then forwards it to the running
//! LDM desktop app over the engine's authenticated Unix socket
//! (`$XDG_RUNTIME_DIR/ldm/ldm.sock`, token in `ldm.token`).
//!
//! Security model (spec §113, §226):
//! * The socket is 0600 and owned by the same user; every message must carry
//!   the per-run token from the 0600 token file.
//! * The host only ever sends `ping` or `add_download` — nothing else.
//! * URLs are validated as http/https before forwarding; referrers and cookies
//!   are forwarded verbatim (they are per-download credentials, never logged).
//! * Message size is capped at 1 MiB.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const MAX_MSG: usize = 1024 * 1024;

#[derive(serde::Deserialize)]
struct HostMessage {
    action: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    referrer: Option<String>,
    #[serde(default)]
    cookies: Vec<(String, String)>,
}

#[derive(serde::Serialize)]
struct HostReply {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

impl HostReply {
    fn ok(data: serde_json::Value) -> Self {
        Self { ok: true, error: None, data: Some(data) }
    }
    fn error(message: impl Into<String>) -> Self {
        Self { ok: false, error: Some(message.into()), data: None }
    }
}

fn main() {
    // Native messaging framing: 4-byte little-endian length, then JSON.
    let mut len_buf = [0u8; 4];
    loop {
        match io::stdin().read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(_) => break, // stdin closed → browser gone.
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 || len > MAX_MSG {
            reply(HostReply::error("message too large"));
            continue;
        }
        let mut buf = vec![0u8; len];
        if io::stdin().read_exact(&mut buf).is_err() {
            break;
        }
        let resp = handle(&buf);
        reply(resp);
    }
}

fn handle(raw: &[u8]) -> HostReply {
    let msg: HostMessage = match serde_json::from_slice(raw) {
        Ok(m) => m,
        Err(e) => return HostReply::error(format!("invalid message: {e}")),
    };

    match msg.action.as_str() {
        "ping" => forward(&msg, "ping"),
        "add_download" => {
            // Validate before touching the socket (spec §225).
            let url = match &msg.url {
                Some(u) => u,
                None => return HostReply::error("missing url"),
            };
            if !is_http_url(url) {
                return HostReply::error("only http/https URLs can be downloaded");
            }
            forward(&msg, "add_download")
        }
        other => HostReply::error(format!("unknown action: {other}")),
    }
}

/// Locate the LDM runtime dir (same logic as the engine: `dirs::runtime_dir()`
/// → `$XDG_RUNTIME_DIR/ldm`, falling back to `<data_dir>/ldm/run`).
fn runtime_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(x).join("ldm");
        if p.join("ldm.sock").exists() {
            return Some(p);
        }
    }
    let data = dirs::data_dir()?;
    let p = data.join("ldm").join("run");
    if p.join("ldm.sock").exists() {
        Some(p)
    } else {
        None
    }
}

fn forward(msg: &HostMessage, op: &str) -> HostReply {
    let dir = match runtime_dir() {
        Some(d) => d,
        None => {
            return HostReply::error(
                "LDM is not running. Start the LDM desktop app first, then retry.",
            )
        }
    };
    let token = match std::fs::read_to_string(dir.join("ldm.token")) {
        Ok(t) => t.trim().to_string(),
        Err(_) => return HostReply::error("cannot read LDM auth token"),
    };
    let mut stream = match UnixStream::connect(dir.join("ldm.sock")) {
        Ok(s) => s,
        Err(e) => return HostReply::error(format!("cannot reach LDM: {e}")),
    };

    let mut wire = serde_json::json!({
        "v": 1,
        "auth": token,
        "op": op,
        "url": msg.url.as_deref().unwrap_or(""),
    });
    if let Some(f) = &msg.filename {
        wire["filename"] = serde_json::json!(f);
    }
    if let Some(r) = &msg.referrer {
        wire["referrer"] = serde_json::json!(r);
    }
    if !msg.cookies.is_empty() {
        wire["cookies"] = serde_json::json!(msg.cookies);
    }

    let mut line = serde_json::to_string(&wire).unwrap_or_default();
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        return HostReply::error("failed to reach LDM (write)");
    }

    // Read the engine's newline-delimited reply.
    let mut out = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.contains('\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let out = out.trim();
    if out.is_empty() {
        return HostReply::error("LDM returned no response");
    }
    // The engine already replies with {ok, error?, data?} — pass it through
    // untouched so the extension sees a single envelope.
    match serde_json::from_str::<EngineReply>(out) {
        Ok(v) => HostReply {
            ok: v.ok,
            error: v.error,
            data: v.data,
        },
        Err(e) => HostReply::error(format!("bad response from LDM: {e}")),
    }
}

#[derive(serde::Deserialize)]
struct EngineReply {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Write a native-messaging framed reply to stdout.
fn reply(r: HostReply) {
    let body = match serde_json::to_vec(&r) {
        Ok(b) => b,
        Err(_) => return,
    };
    let len = (body.len() as u32).to_le_bytes();
    let mut stdout = io::stdout();
    let _ = stdout.write_all(&len);
    let _ = stdout.write_all(&body);
    let _ = stdout.flush();
}
