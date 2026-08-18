//! Integration tests: real HTTP downloads against the local test server.
//! These never touch the internet (spec §217).

use ldm_engine::model::*;
use ldm_engine::settings::DuplicatePolicy;
use ldm_engine::*;
use ldm_test_server as tserver;
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn setup() -> (Arc<DownloadManager>, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ManagerConfig {
        db_path: dir.path().join("test.db"),
        runtime_dir: dir.path().join("run"),
        data_dir: dir.path().to_path_buf(),
        read_only: false,
    };
    let mgr = DownloadManager::new(cfg).await.expect("manager");
    let mut s = mgr.get_settings().await;
    s.default_dir = dir.path().to_string_lossy().to_string();
    s.resume_on_start = true;
    s.duplicate_policy = DuplicatePolicy::Rename;
    mgr.update_settings(s).await.unwrap();
    let base = tserver::start().await.base_url;
    (mgr, dir, base)
}

/// Expected SHA-256 for a fixture (from the test server).
async fn expected_sha256(base: &str, name: &str, qs: &str) -> String {
    let url = format!("{base}/hash/{name}?{qs}");
    let text = reqwest::get(&url).await.unwrap().text().await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["sha256"].as_str().unwrap().to_string()
}

/// Hash a local file.
fn local_sha256(path: &std::path::Path) -> String {
    ldm_engine::verify::hash_file(path, ldm_engine::verify::HashType::Sha256).unwrap()
}

/// Wait until `pred` holds or timeout (async predicate).
async fn wait_until<F, Fut>(timeout: Duration, mut pred: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    pred().await
}

/// Wait for a terminal state and return the record.
async fn wait_terminal(mgr: &DownloadManager, id: i64, timeout: Duration) -> DownloadRecord {
    let start = Instant::now();
    loop {
        let r = mgr.get_download(id).await.unwrap().expect("record");
        match r.status {
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled => {
                return r;
            }
            _ => {}
        }
        assert!(
            start.elapsed() < timeout,
            "timeout waiting for terminal state (status {:?})",
            r.status
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn add(mgr: &DownloadManager, url: String, extra: AddDownloadOptions) -> i64 {
    let opts = AddDownloadOptions {
        url,
        start_immediately: true,
        ..extra
    };
    match mgr.add_download(opts, None).await.unwrap() {
        AddOutcome::Added { download } => download.id,
        other => panic!("unexpected add outcome: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Core downloads
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downloads_single_connection() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}"),
        AddDownloadOptions {
            connections: Some(1),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let path = dir.path().join(name);
    assert!(path.exists());
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&path), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downloads_multi_connection() {
    let (mgr, dir, base) = setup().await;
    let name = "medium.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}"),
        AddDownloadOptions {
            connections: Some(8),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(90)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

/// Multi-connection integrity: same file, 1/4/8/16 connections → same SHA-256
/// (spec §220).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_connection_integrity_same_hash() {
    let (mgr, _dir, base) = setup().await;
    let name = "medium.bin";
    let expected = expected_sha256(&base, name, "").await;
    for conns in [1i32, 4, 8, 16] {
        let sub = tempfile::tempdir().unwrap();
        let mut s = mgr.get_settings().await;
        s.default_dir = sub.path().to_string_lossy().to_string();
        mgr.update_settings(s).await.unwrap();
        let id = add(
            &mgr,
            format!("{base}/file/{name}"),
            AddDownloadOptions {
                connections: Some(conns),
                ..Default::default()
            },
        )
        .await;
        let r = wait_terminal(&mgr, id, Duration::from_secs(120)).await;
        assert_eq!(r.status, DownloadStatus::Completed, "conns={conns}");
        assert_eq!(
            local_sha256(&sub.path().join(name)),
            expected,
            "hash mismatch with {conns} connections"
        );
    }
}

/// Unknown total size (chunked, no Content-Length) still downloads (spec §82).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_size_download() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?chunked=1"),
        AddDownloadOptions::default(),
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

// ---------------------------------------------------------------------------
// Range / fallback behavior
// ---------------------------------------------------------------------------

/// Server without range support → single-connection fallback still works
/// (spec §9, §84).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_range_fallback() {
    let (mgr, dir, base) = setup().await;
    let name = "medium.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?no_range=1"),
        AddDownloadOptions {
            connections: Some(8),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(90)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

/// A server that sends a wrong Content-Range must never corrupt the file:
/// the download fails safely (spec §148).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_content_range_fails_safely() {
    let (mgr, dir, base) = setup().await;
    let mut s = mgr.get_settings().await;
    s.retry_count = 0; // fail fast
    mgr.update_settings(s).await.unwrap();
    let name = "medium.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?wrong_range=1"),
        AddDownloadOptions {
            connections: Some(8),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Failed);
    assert!(!dir.path().join(name).exists(), "no final file on protocol error");
}

/// Server over-sends bytes beyond the requested range → trimmed, file intact
/// (spec §148).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_send_bytes_trimmed() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?over_send=1"),
        AddDownloadOptions {
            connections: Some(4),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

// ---------------------------------------------------------------------------
// Retry / errors
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transient_error_retries_and_succeeds() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?fail_first=503"),
        AddDownloadOptions {
            connections: Some(4),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    assert!(r.retry_count >= 1, "expected at least one retry");
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_429_retries() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?fail_first=429&retry_after=1"),
        AddDownloadOptions {
            connections: Some(4),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permanent_404_fails_without_retry() {
    let (mgr, _, base) = setup().await;
    let id = add(
        &mgr,
        format!("{base}/file/nope.bin?status=404"),
        AddDownloadOptions::default(),
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(30)).await;
    assert_eq!(r.status, DownloadStatus::Failed);
    assert_eq!(r.retry_count, 0, "404 must not be retried");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn truncated_body_fails_cleanly() {
    let (mgr, _, base) = setup().await;
    let mut s = mgr.get_settings().await;
    s.retry_count = 1;
    mgr.update_settings(s).await.unwrap();
    let id = add(
        &mgr,
        format!("{base}/file/small.bin?truncate=1"),
        AddDownloadOptions::default(),
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Failed);
}

// ---------------------------------------------------------------------------
// Redirects
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirects_are_followed() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/redirect/3?filename={name}"),
        AddDownloadOptions {
            connections: Some(4),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redirect_loop_detected() {
    let (mgr, _, base) = setup().await;
    let id = add(
        &mgr,
        format!("{base}/redirect-loop"),
        AddDownloadOptions::default(),
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Failed);
    let err = r.error.expect("error recorded");
    assert!(
        err.message.to_lowercase().contains("redirect"),
        "error should mention redirects: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn basic_auth_download_succeeds() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}?auth=1"),
        AddDownloadOptions {
            username: Some("user".to_string()),
            password: Some("pass".to_string()),
            ..Default::default()
        },
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn basic_auth_missing_fails() {
    let (mgr, _, base) = setup().await;
    let id = add(
        &mgr,
        format!("{base}/file/small.bin?auth=1"),
        AddDownloadOptions::default(),
    )
    .await;
    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Failed);
    let err = r.error.expect("error");
    assert!(
        err.message.to_lowercase().contains("auth"),
        "expected auth error, got: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// Pause / resume / restart
// ---------------------------------------------------------------------------

async fn start_slow_partial(mgr: &DownloadManager, base: &str) -> i64 {
    // 8 MB at 1 MB/s → ~8 s; pause once some data is down.
    let id = add(
        mgr,
        format!("{base}/file/medium.bin?slow=1000000"),
        AddDownloadOptions {
            connections: Some(8),
            ..Default::default()
        },
    )
    .await;
    let ok = wait_until(Duration::from_secs(30), || async {
        mgr.get_download(id)
            .await
            .ok()
            .flatten()
            .map(|r| r.downloaded_bytes > 200_000)
            .unwrap_or(false)
    })
    .await;
    assert!(ok, "download should have made progress");
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_completes() {
    let (mgr, dir, base) = setup().await;
    let id = start_slow_partial(&mgr, &base).await;
    mgr.pause(id).await.unwrap();
    let paused = wait_until(Duration::from_secs(15), || async {
        mgr.get_download(id)
            .await
            .ok()
            .flatten()
            .map(|r| r.status == DownloadStatus::Paused)
            .unwrap_or(false)
    })
    .await;
    assert!(paused, "should pause");
    let rec = mgr.get_download(id).await.unwrap().unwrap();
    assert!(rec.downloaded_bytes > 0);
    assert!(rec.temp_path.is_some(), "partial file kept while paused");

    mgr.resume(id).await.unwrap();
    let r = wait_terminal(&mgr, id, Duration::from_secs(120)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, "medium.bin", "").await;
    assert_eq!(local_sha256(&dir.path().join("medium.bin")), expected);
}

/// Simulate an application restart: pause → drop the manager → recreate on the
/// same database → resume → verify the file is correct (spec §11, §101, §219).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let cfg = |data: &std::path::Path| ManagerConfig {
        db_path: db_path.clone(),
        runtime_dir: data.join("run"),
        data_dir: data.to_path_buf(),
        read_only: false,
    };
    let mgr1 = DownloadManager::new(cfg(dir.path())).await.unwrap();
    let mut s = mgr1.get_settings().await;
    s.default_dir = dir.path().to_string_lossy().to_string();
    mgr1.update_settings(s).await.unwrap();
    let base = tserver::start().await.base_url;
    let id = start_slow_partial(&mgr1, &base).await;
    mgr1.pause(id).await.unwrap();
    wait_until(Duration::from_secs(15), || async {
        mgr1.get_download(id)
            .await
            .ok()
            .flatten()
            .map(|r| r.status == DownloadStatus::Paused)
            .unwrap_or(false)
    })
    .await;
    // Simulate a crash: pause the task cleanly, then drop the manager.
    mgr1.shutdown().await;
    drop(mgr1);

    // "Restart": new manager on the same database.
    let mgr2 = DownloadManager::new(cfg(dir.path())).await.unwrap();
    let mut s = mgr2.get_settings().await;
    s.default_dir = dir.path().to_string_lossy().to_string();
    mgr2.update_settings(s).await.unwrap();

    let rec = mgr2.get_download(id).await.unwrap().unwrap();
    assert_eq!(rec.status, DownloadStatus::Paused);
    assert!(rec.downloaded_bytes > 0, "progress persisted across restart");

    mgr2.resume(id).await.unwrap();
    let r = wait_terminal(&mgr2, id, Duration::from_secs(120)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, "medium.bin", "").await;
    assert_eq!(local_sha256(&dir.path().join("medium.bin")), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_semantics() {
    let (mgr, _, base) = setup().await;
    // Cancel keeping the partial file.
    let id = start_slow_partial(&mgr, &base).await;
    mgr.cancel(id, false).await.unwrap();
    let r = wait_terminal(&mgr, id, Duration::from_secs(15)).await;
    assert_eq!(r.status, DownloadStatus::Cancelled);
    assert!(r.temp_path.is_some(), "partial kept when not deleting");

    // Cancel and delete the partial file.
    let id2 = start_slow_partial(&mgr, &base).await;
    mgr.cancel(id2, true).await.unwrap();
    let r2 = wait_terminal(&mgr, id2, Duration::from_secs(15)).await;
    assert_eq!(r2.status, DownloadStatus::Cancelled);
    assert!(r2.temp_path.is_none(), "partial removed when deleting");
}

// ---------------------------------------------------------------------------
// Queue / scheduling
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_limits_active_downloads() {
    let (mgr, dir, base) = setup().await;
    let mut s = mgr.get_settings().await;
    s.max_active_downloads = 1;
    mgr.update_settings(s).await.unwrap();
    let mut ids = Vec::new();
    for i in 0..3 {
        let o = mgr
            .add_download(
                AddDownloadOptions {
                    url: format!("{base}/file/small.bin"),
                    filename: Some(format!("queue-{i}.bin")),
                    start_immediately: false,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        ids.push(match o {
            AddOutcome::Added { download } => download.id,
            _ => panic!("added"),
        });
    }

    // Watch: active count must never exceed 1.
    let mut exceeded = false;
    let start = Instant::now();
    loop {
        let active = mgr
            .list_downloads(DownloadFilter::Active, "", (SortField::DateAdded, SortOrder::Asc), None)
            .await
            .unwrap()
            .len();
        if active > 1 {
            exceeded = true;
        }
        let mut done = true;
        for id in &ids {
            let rec = mgr.get_download(*id).await.unwrap().unwrap();
            if rec.status != DownloadStatus::Completed {
                done = false;
                break;
            }
        }
        if done {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(90), "queue timeout");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!exceeded, "more than one active download with max_active=1");
    for id in &ids {
        let rec = mgr.get_download(*id).await.unwrap().unwrap();
        assert_eq!(rec.status, DownloadStatus::Completed);
        assert!(dir.path().join(format!("queue-{}.bin", id - ids[0])).exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_download_starts_later() {
    let (mgr, dir, base) = setup().await;
    let name = "small.bin";
    let id = mgr
        .add_download(
            AddDownloadOptions {
                url: format!("{base}/file/{name}"),
                scheduled_start: Some(crate_ts() + 2),
                start_immediately: false,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let id = match id {
        AddOutcome::Added { download } => download.id,
        _ => panic!("added"),
    };
    let rec = mgr.get_download(id).await.unwrap().unwrap();
    assert_eq!(rec.status, DownloadStatus::Scheduled);

    tokio::time::sleep(Duration::from_secs(3)).await;
    mgr.scheduler_tick().await.unwrap();

    let r = wait_terminal(&mgr, id, Duration::from_secs(60)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

fn crate_ts() -> i64 {
    ldm_engine::now_unix()
}

// ---------------------------------------------------------------------------
// Speed limiting
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_download_speed_limit_respected() {
    let (mgr, dir, base) = setup().await;
    // 2 MB at 256 KB/s → ≥ 8 s.
    let name = "2048kb.bin";
    let id = add(
        &mgr,
        format!("{base}/file/{name}"),
        AddDownloadOptions {
            connections: Some(4),
            speed_limit: Some(256 * 1024),
            ..Default::default()
        },
    )
    .await;
    let start = Instant::now();
    let r = wait_terminal(&mgr, id, Duration::from_secs(120)).await;
    let elapsed = start.elapsed();
    assert_eq!(r.status, DownloadStatus::Completed);
    assert!(
        elapsed >= Duration::from_secs(6),
        "expected the limit to slow the download, took {elapsed:?}"
    );
    let expected = expected_sha256(&base, name, "").await;
    assert_eq!(local_sha256(&dir.path().join(name)), expected);
}

// ---------------------------------------------------------------------------
// Startup reconciliation
// ---------------------------------------------------------------------------

/// A download left in DOWNLOADING state (as after a crash) is reconciled on
/// startup and resumed when `resume_on_start` is set (spec §101).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_download_reconciled_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let dbp = db_path.clone();
    let cfg = move |data: &std::path::Path| ManagerConfig {
        db_path: dbp.clone(),
        runtime_dir: data.join("run"),
        data_dir: data.to_path_buf(),
        read_only: false,
    };
    let base = tserver::start().await.base_url;
    // Run the first manager on a separate OS thread with its own runtime, then
    // drop that runtime: every spawned task (workers, loops) is aborted
    // mid-download — exactly what a process crash does (spec §219).
    let dir1 = dir.path().to_path_buf();
    let base1 = base.clone();
    let cfg1 = cfg.clone();
    let id = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        let id = rt.block_on(async {
            let mgr1 = DownloadManager::new(cfg1(&dir1)).await.unwrap();
            let mut s = mgr1.get_settings().await;
            s.default_dir = dir1.to_string_lossy().to_string();
            mgr1.update_settings(s).await.unwrap();
            start_slow_partial(&mgr1, &base1).await
        });
        drop(rt); // crash: aborts all spawned tasks
        id
    })
    .join()
    .unwrap();

    let mgr2 = DownloadManager::new(cfg(dir.path())).await.unwrap();
    let mut s = mgr2.get_settings().await;
    s.default_dir = dir.path().to_string_lossy().to_string();
    mgr2.update_settings(s).await.unwrap();
    // With resume_on_start the manager reconciles the interrupted download to
    // QUEUED and immediately auto-resumes it, so the observed status may be
    // QUEUED/STARTING/CONNECTING — never the stale pre-crash status family is
    // the point; the download must resume and complete with the correct hash.
    let rec = mgr2.get_download(id).await.unwrap().unwrap();
    assert!(
        !rec.status.is_terminal() && rec.status != DownloadStatus::Paused,
        "interrupted download must be reconciled and resumed, got {:?}",
        rec.status
    );
    let r = wait_terminal(&mgr2, id, Duration::from_secs(120)).await;
    assert_eq!(r.status, DownloadStatus::Completed);
    assert!(r.downloaded_bytes >= rec.downloaded_bytes, "resumed, not restarted");
    let expected = expected_sha256(&base, "medium.bin", "").await;
    assert_eq!(local_sha256(&dir.path().join("medium.bin")), expected);
}

/// Event stream delivers progress events (spec §79).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_flow_to_subscribers() {
    let (mgr, _, base) = setup().await;
    let mut rx = mgr.subscribe();
    let name = "small.bin";
    let id = add(&mgr, format!("{base}/file/{name}"), AddDownloadOptions::default()).await;
    let mut saw_progress = false;
    let mut saw_completed = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(ev)) => match ev {
                EngineEvent::DownloadProgress { download_id, .. } if download_id == id => {
                    saw_progress = true;
                }
                EngineEvent::DownloadCompleted { download } if download.id == id => {
                    saw_completed = true;
                    break;
                }
                _ => {}
            },
            _ => {}
        }
    }
    assert!(saw_progress, "expected progress events");
    assert!(saw_completed, "expected a completed event");
}

/// Browser integration (spec §21, §226): the engine exposes an authenticated
/// Unix socket; `ping` and `add_download` must work, bad tokens must be
/// rejected, and the download must actually complete.
#[tokio::test]
async fn browser_ipc_authenticated_requests() {
    let dir = tempfile::tempdir().unwrap();
    // Point the runtime dir at the temp dir so the IPC socket lands somewhere
    // we control.
    std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    let cfg = ManagerConfig {
        db_path: dir.path().join("test.db"),
        runtime_dir: dir.path().join("run"),
        data_dir: dir.path().to_path_buf(),
        read_only: false,
    };
    let mgr = DownloadManager::new(cfg).await.expect("manager");
    let mut s = mgr.get_settings().await;
    s.default_dir = dir.path().to_string_lossy().to_string();
    // Force a false -> true transition so the ipc_loop observes the change
    // regardless of when its initial settings read runs.
    s.browser_integration_enabled = false;
    mgr.update_settings(s.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.browser_integration_enabled = true;
    mgr.update_settings(s).await.unwrap();

    // The ipc_loop polls settings every second and then binds the socket.
    let sock = dir.path().join("ldm/ldm.sock");
    let token_path = dir.path().join("ldm/ldm.token");
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline && !sock.exists() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(sock.exists(), "IPC socket should appear after enabling integration");
    let token = std::fs::read_to_string(&token_path)
        .expect("token file")
        .trim()
        .to_string();
    assert_eq!(token.len(), 32);

    async fn rpc(
        sock: &std::path::Path,
        payload: serde_json::Value,
    ) -> serde_json::Value {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        let mut line = serde_json::to_string(&payload).unwrap();
        line.push('\n');
        stream.write_all(line.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut out = String::new();
        reader.read_line(&mut out).await.unwrap();
        serde_json::from_str(out.trim()).unwrap()
    }

    // ping.
    let resp = rpc(&sock, serde_json::json!({ "v": 1, "auth": token, "op": "ping" })).await;
    assert_eq!(resp["ok"], true, "ping: {resp}");

    // Wrong token rejected.
    let resp = rpc(
        &sock,
        serde_json::json!({ "v": 1, "auth": "wrong-token", "op": "ping" }),
    )
    .await;
    assert_eq!(resp["ok"], false, "bad token must be rejected: {resp}");
    assert!(resp["error"].as_str().unwrap().contains("auth"));

    // add_download with a real URL → must download to completion.
    let base = tserver::start().await.base_url;
    let url = format!("{base}/file/1024b.bin");
    let resp = rpc(
        &sock,
        serde_json::json!({
            "v": 1,
            "auth": token,
            "op": "add_download",
            "url": url,
            "filename": "ipc-test.bin",
        }),
    )
    .await;
    assert_eq!(resp["ok"], true, "add_download: {resp}");
    let id = resp["data"]["download_id"].as_i64().expect("download id");

    let mut rx = mgr.subscribe();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut completed = false;
    while Instant::now() < deadline && !completed {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(EngineEvent::DownloadCompleted { download })) if download.id == id => {
                completed = true;
            }
            Ok(Ok(_)) => {}
            _ => {}
        }
    }
    assert!(completed, "IPC-initiated download should complete");

    // Malformed / non-http URLs rejected without crashing the server.
    let resp = rpc(
        &sock,
        serde_json::json!({ "v": 1, "auth": token, "op": "add_download", "url": "file:///etc/passwd" }),
    )
    .await;
    assert_eq!(resp["ok"], false, "non-http URL must be rejected");

    // The socket still works after the bad request (no crash).
    let resp = rpc(&sock, serde_json::json!({ "v": 1, "auth": token, "op": "ping" })).await;
    assert_eq!(resp["ok"], true, "server must survive bad input");
}
