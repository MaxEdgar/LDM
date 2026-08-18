//! LDM command-line interface.
//!
//! Shares the same engine and SQLite database as the desktop app, so a file
//! downloaded from the terminal shows up in the GUI (and vice versa).
//!
//! Examples:
//!   ldm download https://example.com/file.iso
//!   ldm fetch  https://example.com/file.zip -o archive.zip -c 16
//!   ldm probe  https://example.com/file.iso
//!   ldm list
//!   ldm pause 3 && ldm resume 3

use clap::{Args, Parser, Subcommand};
use ldm_engine::manager::AddDownloadOptions;
use ldm_engine::model::{DownloadFilter, DownloadRecord, DownloadStatus, SortField, SortOrder};
use ldm_engine::{DownloadManager, ManagerConfig, EngineEvent};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "ldm",
    version,
    about = "LDM — Linux Download Manager CLI",
    long_about = "Download and fetch files over HTTP/HTTPS with multi-connection support, \
                  resume, retries, and rate limiting. Shares the same engine and database \
                  as the LDM desktop app."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a file now (shows progress in the terminal).
    Download(DownloadArgs),
    /// Alias for `download`.
    Fetch(DownloadArgs),
    /// Probe a URL: size, range support, suggested filename.
    Probe {
        /// URL to inspect.
        url: String,
    },
    /// List downloads in the shared database.
    List {
        /// Filter: all, active, paused, completed, failed, queued, scheduled.
        #[arg(long, short, default_value = "all")]
        filter: String,
    },
    /// Pause a download by id.
    Pause {
        /// Download id (see `ldm list`).
        id: i64,
    },
    /// Resume a paused download by id.
    Resume {
        /// Download id.
        id: i64,
    },
    /// Cancel a download by id.
    Cancel {
        /// Download id.
        id: i64,
        /// Also delete the partial file.
        #[arg(long)]
        delete_partial: bool,
    },
    /// Remove a download record by id.
    Remove {
        /// Download id.
        id: i64,
        /// Also delete the downloaded file.
        #[arg(long)]
        delete_file: bool,
    },
}

#[derive(Args, Clone, Default)]
struct DownloadArgs {
    /// URL to download.
    url: String,

    /// Output filename (defaults to the server/URL filename).
    #[arg(short, long)]
    output: Option<String>,

    /// Directory to save into (defaults to ~/Downloads).
    #[arg(short = 'D', long)]
    dir: Option<String>,

    /// Number of connections (1–32, default 8).
    #[arg(short, long, default_value_t = 8)]
    connections: i32,

    /// Per-download speed limit, e.g. "5M", "1.5m", "512k", "0" for unlimited.
    #[arg(long)]
    speed_limit: Option<String>,

    /// HTTP basic username.
    #[arg(short, long)]
    username: Option<String>,

    /// HTTP basic password.
    #[arg(short, long)]
    password: Option<String>,

    /// Extra header, e.g. `-H "X-Foo: bar"` (repeatable).
    #[arg(short = 'H', long)]
    header: Vec<String>,

    /// Referrer URL.
    #[arg(long)]
    referrer: Option<String>,

    /// Expected SHA-256 checksum to verify after download.
    #[arg(long)]
    sha256: Option<String>,

    /// Verify with SHA-512 instead of SHA-256.
    #[arg(long)]
    sha512: Option<String>,

    /// Add to the queue but do not start yet.
    #[arg(long)]
    add_only: bool,

    /// Print minimal output (no progress bar).
    #[arg(long, short)]
    quiet: bool,
}

fn default_config() -> ManagerConfig {
    ManagerConfig::default()
}

/// Read-only manager for `list`/`probe`: never reconciles or touches the
/// state of downloads owned by a running desktop app.
fn read_only_config() -> ManagerConfig {
    let mut c = ManagerConfig::default();
    c.read_only = true;
    c
}

fn human_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    for (unit, div) in [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)] {
        if b >= div {
            return format!("{:.1}{}", b / div, unit);
        }
    }
    format!("{b}B")
}

fn parse_speed_limit(s: &str) -> Option<i64> {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() || t == "0" || t == "unlimited" {
        return Some(0);
    }
    let (num, mult) = if let Some(n) = t.strip_suffix("mb") {
        (n, 1_000_000f64)
    } else if let Some(n) = t.strip_suffix('m') {
        (n, 1_000_000f64)
    } else if let Some(n) = t.strip_suffix("kb") {
        (n, 1_000f64)
    } else if let Some(n) = t.strip_suffix('k') {
        (n, 1_000f64)
    } else if let Some(n) = t.strip_suffix('b') {
        (n, 1f64)
    } else {
        (t.as_str(), 1f64)
    };
    num.trim().parse::<f64>().ok().map(|v| (v * mult) as i64)
}

fn print_record(r: &DownloadRecord) {
    let status = r.status.as_str().to_lowercase();
    let size = match (r.total_bytes, r.downloaded_bytes) {
        (Some(t), _) if t > 0 => format!(
            "{:>5} / {:<5} ({:>3.0}%)",
            human_size(r.downloaded_bytes),
            human_size(t),
            r.downloaded_bytes as f64 / t as f64 * 100.0
        ),
        _ => human_size(r.downloaded_bytes).to_string(),
    };
    let speed = if r.current_speed > 0 {
        format!(" {}/s", human_size(r.current_speed as i64))
    } else {
        String::new()
    };
    println!("{:>4}  {:<32} {:<22} {:<10}{}", r.id, r.filename, size, status, speed);
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("ldm: {msg}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Download(a) | Command::Fetch(a) => download(a).await,
        Command::Probe { url } => probe(url).await,
        Command::List { filter } => list(filter).await,
        Command::Pause { id } => with_manager(|m| async move {
            m.pause(id).await.map_err(|e| e.message)
        }).await,
        Command::Resume { id } => with_manager(|m| async move {
            m.resume(id).await.map_err(|e| e.message)
        }).await,
        Command::Cancel { id, delete_partial } => with_manager(|m| async move {
            m.cancel(id, delete_partial).await.map_err(|e| e.message)
        }).await,
        Command::Remove { id, delete_file } => with_manager(|m| async move {
            m.remove(id, delete_file).await.map_err(|e| e.message)
        }).await,
    }
}

async fn with_manager<F, Fut>(f: F) -> Result<(), String>
where
    F: FnOnce(Arc<DownloadManager>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mgr = DownloadManager::new(default_config()).await.map_err(|e| e.message)?;
    f(mgr).await
}

async fn list(filter: String) -> Result<(), String> {
    let mgr = DownloadManager::new(read_only_config()).await.map_err(|e| e.message)?;
    let f = match filter.as_str() {
        "active" => DownloadFilter::Active,
        "paused" => DownloadFilter::Paused,
        "completed" => DownloadFilter::Completed,
        "failed" => DownloadFilter::Failed,
        "queued" => DownloadFilter::Queued,
        "scheduled" => DownloadFilter::Scheduled,
        "all" => DownloadFilter::All,
        other => return Err(format!("unknown filter: {other}")),
    };
    let records = mgr
        .list_downloads(f, "", (SortField::DateAdded, SortOrder::Desc), None)
        .await
        .map_err(|e| e.message)?;
    if records.is_empty() {
        println!("no downloads");
        return Ok(());
    }
    println!("{:>4}  {:<32} {:<22} {:<10}", "ID", "NAME", "SIZE", "STATUS");
    for r in &records {
        print_record(r);
    }
    Ok(())
}

async fn probe(url: String) -> Result<(), String> {
    let mgr = DownloadManager::new(read_only_config()).await.map_err(|e| e.message)?;
    let p = mgr.probe_preview(&url).await.map_err(|e| e.message)?;
    println!("URL:            {url}");
    if let Some(f) = &p.filename {
        println!("Filename:       {f}");
    }
    match p.total_bytes {
        Some(t) => println!("Size:           {} ({} bytes)", human_size(t), t),
        None => println!("Size:           unknown"),
    }
    println!("Range support: {}", if p.ranges_supported { "yes" } else { "no" });
    if let Some(etag) = &p.etag {
        println!("ETag:           {etag}");
    }
    if let Some(lm) = &p.last_modified {
        println!("Last-Modified:  {lm}");
    }
    if let Some(srv) = &p.server {
        println!("Server:         {srv}");
    }
    if let Some(fu) = &p.final_url {
        println!("Final URL:      {fu}");
    }
    Ok(())
}

async fn download(a: DownloadArgs) -> Result<(), String> {
    if a.url.trim().is_empty() {
        return Err("missing URL — usage: ldm download <url>".to_string());
    }
    let mgr = DownloadManager::new(default_config()).await.map_err(|e| e.message)?;

    // Speed limit: per-download limit given on the CLI.
    let speed_limit = match &a.speed_limit {
        Some(s) => Some(
            parse_speed_limit(s)
                .ok_or_else(|| format!("invalid speed limit: {s}"))?,
        ),
        None => None,
    };

    // Headers from -H "K: V".
    let mut headers = Vec::new();
    for h in &a.header {
        let Some((k, v)) = h.split_once(':') else {
            return Err(format!("invalid header (expected \"Name: value\"): {h}"));
        };
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }

    let verify = if let Some(h) = &a.sha256 {
        Some((h.clone(), "sha256".to_string()))
    } else if let Some(h) = &a.sha512 {
        Some((h.clone(), "sha512".to_string()))
    } else {
        None
    };

    let opts = AddDownloadOptions {
        url: a.url.trim().to_string(),
        filename: a.output.clone(),
        dir: a.dir.clone(),
        connections: Some(a.connections.clamp(1, 32)),
        start_immediately: !a.add_only,
        speed_limit,
        username: a.username.clone(),
        password: a.password.clone(),
        headers,
        referrer: a.referrer.clone(),
        verify_hash: verify.as_ref().map(|(h, _)| h.clone()),
        verify_type: verify.as_ref().map(|(_, t)| t.clone()),
        ..Default::default()
    };

    // CLI convenience: automatically rename when a file with the same name
    // already exists (like `wget`), instead of asking interactively.
    use ldm_engine::settings::DuplicatePolicy;
    let outcome = mgr
        .add_download(opts, Some(DuplicatePolicy::Rename))
        .await
        .map_err(|e| e.message)?;
    let id = match outcome {
        ldm_engine::manager::AddOutcome::Added { download } => download.id,
        ldm_engine::manager::AddOutcome::Skipped { reason } => {
            println!("{reason}");
            return Ok(());
        }
        other => {
            return Err(format!("could not add download: {other:?}"));
        }
    };
    if a.add_only {
        println!("added download #{id} to the queue");
        return Ok(());
    }

    // Subscribe before starting so we don't miss terminal events.
    let mut rx = mgr.subscribe();
    // In case it finished between add and subscribe, re-check.
    let mut last_progress = None::<(i64, Option<i64>, u64, Option<i64>, f64)>;
    let start = Instant::now();
    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(EngineEvent::DownloadProgress { download_id, downloaded_bytes, total_bytes, speed, eta_seconds, percentage, .. }) if download_id == id => {
                        last_progress = Some((downloaded_bytes, total_bytes, speed, eta_seconds, percentage.unwrap_or(0.0)));
                        if !a.quiet {
                            draw_progress(downloaded_bytes, total_bytes, speed, eta_seconds, percentage.unwrap_or(0.0), start.elapsed());
                        }
                    }
                    Ok(EngineEvent::DownloadCompleted { download }) if download.id == id => {
                        if !a.quiet { eprint!("\r\x1b[K"); }
                        println!(
                            "downloaded {} ({}) in {:.1}s",
                            download.filename,
                            human_size(download.downloaded_bytes),
                            start.elapsed().as_secs_f64()
                        );
                        return Ok(());
                    }
                    Ok(EngineEvent::DownloadFailed { download, error }) if download.id == id => {
                        if !a.quiet { eprint!("\r\x1b[K"); }
                        eprintln!("download {} failed: {error}", download.filename);
                        return Err(error);
                    }
                    Ok(EngineEvent::DownloadCancelled { download_id }) if download_id == id => {
                        if !a.quiet { eprint!("\r\x1b[K"); }
                        return Err("download cancelled".to_string());
                    }
                    Ok(EngineEvent::DownloadRetrying { download_id, attempt, next_retry_in_seconds, error }) if download_id == id => {
                        if !a.quiet { eprint!("\r\x1b[K"); }
                        eprintln!("retrying (attempt {attempt}) in {next_retry_in_seconds}s: {error}");
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                // Fallback poll: if events were missed, check the record.
                if let Ok(Some(r)) = mgr.get_download(id).await {
                    if r.status.is_terminal() && last_progress.is_none() {
                        if r.status == DownloadStatus::Completed {
                            println!("downloaded {} ({})", r.filename, human_size(r.downloaded_bytes));
                            return Ok(());
                        }
                        if let Some(e) = &r.error {
                            return Err(e.message.clone());
                        }
                        return Err("download did not complete".to_string());
                    }
                }
            }
        }
    }
}

fn draw_progress(downloaded: i64, total: Option<i64>, speed: u64, eta: Option<i64>, pct: f64, elapsed: Duration) {
    let total = total.unwrap_or(downloaded.max(1));
    let pct = if total > 0 { pct } else { 0.0 };
    let width = 30usize;
    let filled = ((pct / 100.0) * width as f64) as usize;
    let bar: String = format!("{}{}", "█".repeat(filled), "░".repeat(width.saturating_sub(filled)));
    let eta_txt = match eta {
        Some(e) if e > 0 => format!("{:>5}s", e),
        _ => "   -- ".to_string(),
    };
    let speed_txt = if speed > 0 {
        format!("{}/s", human_size(speed as i64))
    } else {
        "----".to_string()
    };
    let total_txt = human_size(total.max(downloaded));
    let down_txt = human_size(downloaded);
    let _ = elapsed;
    eprint!(
        "\r\x1b[K{:>5.1}% [{bar}] {down_txt} / {total_txt}  {speed_txt:>10}  ETA {eta_txt}",
        pct
    );
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_limits_parse() {
        assert_eq!(parse_speed_limit("0"), Some(0));
        assert_eq!(parse_speed_limit("unlimited"), Some(0));
        assert_eq!(parse_speed_limit("512k"), Some(512_000));
        assert_eq!(parse_speed_limit("1.5M"), Some(1_500_000));
        assert_eq!(parse_speed_limit("2mb"), Some(2_000_000));
        assert_eq!(parse_speed_limit("10"), Some(10));
        assert_eq!(parse_speed_limit("bogus"), None);
    }
}
