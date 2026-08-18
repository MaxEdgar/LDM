//! End-to-end test of the native messaging host: spawn the real binary, speak
//! the native-messaging framing protocol, and verify requests reach the engine
//! and downloads actually complete (spec §21, §226).

use ldm_engine::ManagerConfig;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

async fn start_manager(tmp: &std::path::Path) -> std::sync::Arc<ldm_engine::DownloadManager> {
    std::env::set_var("XDG_RUNTIME_DIR", tmp);
    let cfg = ManagerConfig {
        db_path: tmp.join("test.db"),
        runtime_dir: tmp.join("run"),
        data_dir: tmp.to_path_buf(),
        read_only: false,
    };
    let mgr = ldm_engine::DownloadManager::new(cfg).await.unwrap();
    let mut s = mgr.get_settings().await;
    s.default_dir = tmp.to_string_lossy().to_string();
    s.browser_integration_enabled = false;
    mgr.update_settings(s.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.browser_integration_enabled = true;
    mgr.update_settings(s).await.unwrap();
    mgr
}

/// Framed native-messaging message: 4-byte little-endian length + JSON.
fn nm_send(child: &mut Child, payload: &serde_json::Value) -> serde_json::Value {
    let body = serde_json::to_vec(payload).unwrap();
    let mut framed = (body.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(&body);
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&framed)
        .unwrap();
    child.stdin.as_mut().unwrap().flush().unwrap();

    let mut len_buf = [0u8; 4];
    let stdin = child.stdout.as_mut().unwrap();
    stdin.read_exact(&mut len_buf).unwrap();
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stdin.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_host_forwards_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = start_manager(dir.path()).await;

    // Wait for the IPC socket (the ipc_loop polls settings every second).
    let sock = dir.path().join("ldm/ldm.sock");
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline && !sock.exists() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(sock.exists(), "IPC socket should exist");

    let mut child = Command::new(env!("CARGO_BIN_EXE_ldm-native-host"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn native host");

    // ping.
    let resp = nm_send(&mut child, &serde_json::json!({"action": "ping"}));
    assert_eq!(resp["ok"], true, "ping: {resp}");

    // Unknown action → clean error, host stays alive.
    let resp = nm_send(&mut child, &serde_json::json!({"action": "explode"}));
    assert_eq!(resp["ok"], false);
    let resp = nm_send(&mut child, &serde_json::json!({"action": "ping"}));
    assert_eq!(resp["ok"], true, "host must survive bad input");

    // add_download with a real local URL → must complete.
    let base = ldm_test_server::start().await.base_url;
    let url = format!("{base}/file/1024b.bin");
    let resp = nm_send(
        &mut child,
        &serde_json::json!({
            "action": "add_download",
            "url": url,
            "filename": "native-host-test.bin",
            "referrer": "http://localhost/",
        }),
    );
    assert_eq!(resp["ok"], true, "add_download: {resp}");
    assert!(
        resp["data"]["download_id"].is_number(),
        "expected download_id, got: {resp}"
    );

    // The engine should complete the download.
    let mut rx = mgr.subscribe();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut completed = false;
    while Instant::now() < deadline && !completed {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(ldm_engine::EngineEvent::DownloadCompleted { download }))
                if download.filename == "native-host-test.bin" =>
            {
                completed = true;
            }
            Ok(Ok(_)) => {}
            _ => {}
        }
    }
    assert!(completed, "native-host download should complete");

    // Non-http URL → rejected with a clear error, host still alive.
    let resp = nm_send(&mut child, &serde_json::json!({"action": "add_download", "url": "file:///etc/passwd"}));
    assert_eq!(resp["ok"], false);
    assert!(resp["error"].as_str().unwrap().contains("http"), "{resp}");
    let resp = nm_send(&mut child, &serde_json::json!({"action": "ping"}));
    assert_eq!(resp["ok"], true);

    child.kill().ok();
}
