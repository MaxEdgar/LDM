//! Standalone demo/test server (spec §216). Run with `cargo run -p ldm-test-server`.
//!
//! Examples:
//!   ldm-test-server --port 8080
//!   # then: curl http://127.0.0.1:8080/file/large.bin
//!   #       curl http://127.0.0.1:8080/file/medium.bin?slow=1000000
//!   #       curl http://127.0.0.1:8080/file/small.bin?status=429&retry_after=60

use axum::routing::get;
use ldm_test_server::{router, ServerState};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    println!("LDM test server listening on http://127.0.0.1:{port}");
    println!();
    println!("  /file/small.bin        (1 MB, ranges supported)");
    println!("  /file/medium.bin       (8 MB)");
    println!("  /file/large.bin        (64 MB)");
    println!("  /file/1024b.bin        (exactly 1024 bytes)");
    println!("  /file/x.bin?no_range=1 (no range support)");
    println!("  /file/x.bin?slow=N     (throttled to N bytes/sec)");
    println!("  /file/x.bin?fail_after=N (connection drops after N bytes)");
    println!("  /file/x.bin?status=404|429|500|503[&retry_after=60]");
    println!("  /file/x.bin?auth=1     (Basic user:pass)");
    println!("  /file/x.bin?change=1   (content/ETag changes every request)");
    println!("  /file/x.bin?wrong_range=1 (malicious Content-Range)");
    println!("  /file/x.bin?over_send=1 (sends more bytes than requested)");
    println!("  /file/x.bin?truncate=1 (sends fewer bytes than Content-Length)");
    println!("  /file/x.bin?chunked=1  (no Content-Length)");
    println!("  /file/x.bin?filename=custom.zip (Content-Disposition)");
    println!("  /redirect/3?filename=small.bin (302 chain)");
    println!("  /redirect-loop          (infinite redirect loop)");
    println!("  /no-head/x.bin          (HEAD returns 405)");
    println!("  /hash/x.bin             (expected SHA-256 of the file)");
    println!();
    let state = ServerState {
        change_counter: Arc::new(AtomicU64::new(0)),
        request_count: Arc::new(AtomicU64::new(0)),
        hits: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };
    let app = router(state).route("/health", get(|| async { "ok" }));
    axum::serve(listener, app).await.expect("serve");
}
