//! Application state: engine handle, GTK widgets, view state, event routing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use glib::ToValue;
use gtk::prelude::*;
use ldm_engine::{
    AddOutcome, DownloadFilter, DownloadManager, DownloadRecord, DownloadStatus, EngineEvent,
    ProbeInfo, SortField, SortOrder,
};

/// Messages delivered on the GTK main loop.
pub enum UiMsg {
    Engine(EngineEvent),
    AddResult(Result<AddOutcome, String>),
    ProbeResult(Result<ProbeInfo, String>),
    Refresh,
}

/// Sidebar view state.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    All,
    Active,
    Paused,
    Queued,
    Completed,
    Failed,
    Scheduled,
    Category(String),
    Queue(String),
}

/// Human-readable byte size (SI units, like IDM): 8.4 MB, 1.2 GB, 512 KB.
pub fn format_bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    for (unit, div) in [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)] {
        if n >= div {
            let v = n / div;
            return if v >= 10.0 {
                format!("{v:.0} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
    }
    format!("{n:.0} B")
}

/// ETA like IDM: "1m 27s", "3h 12m", "45s".
pub fn format_eta(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

pub struct App {
    pub manager: std::sync::Arc<DownloadManager>,
    pub rt: tokio::runtime::Handle,
    pub ui_tx: glib::Sender<UiMsg>,
    pub gtk_app: gtk::Application,

    // Widgets (built in window.rs).
    pub window: RefCell<Option<gtk::ApplicationWindow>>,
    pub store: gtk::ListStore,
    pub tree: gtk::TreeView,
    pub search: gtk::SearchEntry,
    pub sidebar: gtk::ListBox,
    pub status_label: gtk::Label,

    // View state.
    pub view: RefCell<View>,
    pub search_text: RefCell<String>,
    /// Sidebar row index -> view (kept in sync by window.rs).
    pub sidebar_views: RefCell<Vec<View>>,
    pub queue_names: RefCell<Vec<String>>,

    // Record cache id -> record, and id -> row iter.
    pub records: RefCell<HashMap<i64, DownloadRecord>>,
    pub row_iters: RefCell<HashMap<i64, gtk::TreeIter>>,
    /// Latest per-segment progress per download (connection bars).
    pub seg_progress: RefCell<HashMap<i64, Vec<ldm_engine::events::SegmentProgress>>>,

    // Real-time speed graph.
    pub speed_history: RefCell<Vec<u64>>,
    pub speed_graph: RefCell<Option<gtk::DrawingArea>>,
    /// Connection-bars panel (per-segment progress of the selected download).
    pub conn_bars: RefCell<Option<gtk::DrawingArea>>,
    /// The panel container (shown/hidden when a download is selected).
    pub conn_panel: RefCell<Option<gtk::Box>>,

    // Status-bar aggregates.
    pub active_count: Cell<usize>,
    pub queued_count: Cell<usize>,
    pub total_speed: Cell<u64>,
    pub total_bytes: Cell<i64>,

    pub tray: RefCell<Option<crate::tray::Tray>>,
}

// Column indices for the download list store.
pub const COL_ID: i32 = 0;
pub const COL_NAME: i32 = 1;
pub const COL_SIZE: i32 = 2;
pub const COL_PROGRESS: i32 = 3;
pub const COL_SPEED: i32 = 4;
pub const COL_STATUS: i32 = 5;
pub const COL_CATEGORY: i32 = 6;
pub const COL_URL: i32 = 7;
pub const COL_DIR: i32 = 8;
pub const COL_ETA: i32 = 9;
pub const COL_STATUS_COLOR: i32 = 10;

/// Display color for a download status (readable in light and dark themes).
pub fn status_color(status: &str) -> &'static str {
    match status {
        "completed" => "#2e9e5b",
        "failed" | "cancelled" => "#e5534b",
        "downloading" | "connecting" | "starting" => "#3b82f6",
        "paused" => "#b48ead",
        "queued" | "scheduled" | "waiting" => "#8a94a3",
        _ => "#8a94a3",
    }
}

/// Friendly display label for a status.
pub fn status_label(status: &str) -> String {
    match status {
        "downloading" => "Downloading".to_string(),
        "connecting" => "Connecting…".to_string(),
        "starting" => "Starting…".to_string(),
        "completed" => "Completed".to_string(),
        "failed" => "Failed".to_string(),
        "cancelled" => "Cancelled".to_string(),
        "paused" => "Paused".to_string(),
        "queued" => "Queued".to_string(),
        "scheduled" => "Scheduled".to_string(),
        "waiting" => "Waiting".to_string(),
        "verifying" => "Verifying…".to_string(),
        "retrying" => "Retrying…".to_string(),
        other => other.to_string(),
    }
}

impl App {
    pub fn new(
        manager: std::sync::Arc<DownloadManager>,
        rt: tokio::runtime::Handle,
        ui_tx: glib::Sender<UiMsg>,
        gtk_app: &gtk::Application,
    ) -> Rc<Self> {
        let store = gtk::ListStore::new(&[
            glib::Type::I64,    // COL_ID
            glib::Type::STRING, // COL_NAME
            glib::Type::STRING, // COL_SIZE
            glib::Type::I32,    // COL_PROGRESS
            glib::Type::STRING, // COL_SPEED
            glib::Type::STRING, // COL_STATUS
            glib::Type::STRING, // COL_CATEGORY
            glib::Type::STRING, // COL_URL
            glib::Type::STRING, // COL_DIR
            glib::Type::STRING, // COL_ETA
            glib::Type::STRING, // COL_STATUS_COLOR
        ]);
        let tree = gtk::TreeView::with_model(&store);
        Rc::new(Self {
            manager,
            rt,
            ui_tx,
            gtk_app: gtk_app.clone(),
            window: RefCell::new(None),
            store,
            tree,
            search: gtk::SearchEntry::new(),
            sidebar: gtk::ListBox::new(),
            status_label: gtk::Label::new(Some("Ready")),
            view: RefCell::new(View::All),
            search_text: RefCell::new(String::new()),
            sidebar_views: RefCell::new(Vec::new()),
            queue_names: RefCell::new(Vec::new()),
            records: RefCell::new(HashMap::new()),
            row_iters: RefCell::new(HashMap::new()),
            seg_progress: RefCell::new(HashMap::new()),
            speed_history: RefCell::new(Vec::new()),
            speed_graph: RefCell::new(None),
            conn_bars: RefCell::new(None),
            conn_panel: RefCell::new(None),
            active_count: Cell::new(0),
            queued_count: Cell::new(0),
            total_speed: Cell::new(0),
            total_bytes: Cell::new(0),
            tray: RefCell::new(None),
        })
    }

    pub fn on_msg(self: &Rc<Self>, msg: UiMsg) {
        match msg {
            UiMsg::Engine(ev) => self.on_engine_event(ev),
            UiMsg::AddResult(res) => self.on_add_result(res),
            UiMsg::ProbeResult(_) => {}
            UiMsg::Refresh => {
                let _ = self.refresh();
            }
        }
    }

    fn on_engine_event(self: &Rc<Self>, ev: EngineEvent) {
        match ev {
            EngineEvent::DownloadProgress {
                download_id,
                downloaded_bytes,
                total_bytes,
                speed,
                avg_speed: _,
                peak_speed: _,
                eta_seconds: _,
                percentage,
                status,
                segments,
            } => {
                self.update_progress_row(
                    download_id,
                    downloaded_bytes,
                    total_bytes,
                    speed,
                    percentage,
                    &status,
                    &segments,
                );
                // Feed the real-time speed graph.
                self.speed_history.borrow_mut().push(speed);
                let mut h = self.speed_history.borrow_mut();
                let over = h.len().saturating_sub(120);
                if over > 0 {
                    h.drain(0..over);
                }
                if let Some(da) = self.speed_graph.borrow().as_ref() {
                    da.queue_draw();
                }
            }
            EngineEvent::DownloadCreated { download } => {
                self.records.borrow_mut().insert(download.id, download);
                let _ = self.refresh();
            }
            EngineEvent::DownloadCompleted { download } => {
                self.records.borrow_mut().insert(download.id, download);
                let _ = self.refresh();
                let settings = self.rt.block_on(self.manager.get_settings());
                crate::notify::on_completed(&settings);
            }
            EngineEvent::DownloadFailed { download, error } => {
                self.records.borrow_mut().insert(download.id, download);
                let _ = self.refresh();
                let settings = self.rt.block_on(self.manager.get_settings());
                crate::notify::on_failed(&settings, &error);
            }
            EngineEvent::DownloadCancelled { download_id } => {
                // Note: the immutable borrow must end before the mutable one;
                // an `if let` condition keeps its temporary alive for the
                // whole block and would panic with "RefCell already borrowed".
                let rec = self.records.borrow().get(&download_id).cloned();
                if let Some(r) = rec {
                    self.records.borrow_mut().insert(download_id, r);
                }
                let _ = self.refresh();
            }
            EngineEvent::DownloadRemoved { download_id } => {
                self.records.borrow_mut().remove(&download_id);
                let _ = self.refresh();
            }
            EngineEvent::DownloadUpdated { download } => {
                self.records.borrow_mut().insert(download.id, download);
                let _ = self.refresh();
            }
            EngineEvent::DownloadPaused { download_id }
            | EngineEvent::DownloadResumed { download_id }
            | EngineEvent::DownloadVerifying { download_id } => {
                self.touch(download_id);
            }
            EngineEvent::DownloadRetrying { .. } => {
                let _ = self.refresh();
            }
            EngineEvent::QueueChanged => {
                self.rebuild_sidebar_queues();
            }
            EngineEvent::DownloadsIdle => {
                crate::dialogs::maybe_after_completion(self);
            }
            EngineEvent::ClipboardUrlDetected { url, filename } => {
                crate::dialogs::show_clipboard_prompt(self, url, filename);
            }
            EngineEvent::BrowserDownloadRequested {
                url,
                filename,
                referrer,
            } => {
                crate::dialogs::show_browser_prompt(self, url, filename, referrer);
            }
            _ => {}
        }
    }

    /// Lightweight re-fetch for status-only events.
    fn touch(&self, id: i64) {
        if let Ok(Some(rec)) = self.rt.block_on(self.manager.get_download(id)) {
            self.records.borrow_mut().insert(id, rec);
        }
        let _ = self.refresh();
    }

    fn on_add_result(self: &Rc<Self>, res: Result<AddOutcome, String>) {
        match res {
            Ok(AddOutcome::Added { .. }) => {
                let _ = self.refresh();
            }
            Ok(AddOutcome::NeedsDuplicateDecision {
                url,
                filename,
                dir,
                existing_path,
            }) => {
                crate::dialogs::show_duplicate_decision(self, url, filename, dir, existing_path);
            }
            Ok(AddOutcome::Skipped { reason }) => {
                crate::dialogs::show_error(self, &reason);
            }
            Err(e) => {
                crate::dialogs::show_error(self, &e);
            }
        }
    }

    // ------------------------------------------------------------------
    // Refresh / render
    // ------------------------------------------------------------------

    pub fn refresh(&self) -> Result<(), String> {
        let (filter, category) = match self.view.borrow().clone() {
            View::All => (DownloadFilter::All, None),
            View::Active => (DownloadFilter::Active, None),
            View::Paused => (DownloadFilter::Paused, None),
            View::Queued => (DownloadFilter::Queued, None),
            View::Completed => (DownloadFilter::Completed, None),
            View::Failed => (DownloadFilter::Failed, None),
            View::Scheduled => (DownloadFilter::Scheduled, None),
            View::Category(c) => (DownloadFilter::All, Some(c)),
            View::Queue(_) => (DownloadFilter::All, None),
        };
        let search = self.search_text.borrow().clone();
        let selected_id = self.selected_id();
        let records = self
            .rt
            .block_on(async {
                self.manager
                    .list_downloads(
                        filter,
                        &search,
                        (SortField::DateAdded, SortOrder::Desc),
                        category.as_deref(),
                    )
                    .await
            })
            .map_err(|e| e.to_string())?;

        let mut records = records;
        if let View::Queue(q) = self.view.borrow().clone() {
            records.retain(|r| r.queue_name.as_deref() == Some(q.as_str()));
        }

        self.store.clear();
        self.row_iters.borrow_mut().clear();
        self.records.borrow_mut().clear();

        let mut active = 0usize;
        let mut queued = 0usize;
        let mut speed = 0u64;
        let mut total = 0i64;
        for r in &records {
            let iter = self.store.append();
            self.fill_row(&iter, r);
            self.row_iters.borrow_mut().insert(r.id, iter);
            self.records.borrow_mut().insert(r.id, r.clone());
            if r.status.is_active() {
                active += 1;
                speed += r.current_speed;
            }
            if r.status == DownloadStatus::Queued {
                queued += 1;
            }
            if let Some(t) = r.total_bytes {
                total += t;
            }
        }
        self.active_count.set(active);
        self.queued_count.set(queued);
        self.total_speed.set(speed);
        self.total_bytes.set(total);
        self.update_status_bar();

        if let Some(id) = selected_id {
            if let Some(iter) = self.row_iters.borrow().get(&id).copied() {
                let path = self.store.path(&iter);
                self.tree.selection().select_iter(&iter);
                if let Some(path) = path {
                    self.tree
                        .scroll_to_cell(Some(&path), None::<&gtk::TreeViewColumn>, false, 0.0, 0.0);
                }
            }
        }
        Ok(())
    }

    pub fn selected_id(&self) -> Option<i64> {
        let sel = self.tree.selection();
        match sel.selected() {
            Some((_, iter)) => self.store.value(&iter, COL_ID).get::<i64>().ok(),
            None => None,
        }
    }

    fn update_progress_row(
        &self,
        id: i64,
        downloaded: i64,
        total: Option<i64>,
        speed: u64,
        percentage: Option<f64>,
        status: &str,
        segments: &[ldm_engine::events::SegmentProgress],
    ) {
        if !segments.is_empty() {
            self.seg_progress
                .borrow_mut()
                .insert(id, segments.to_vec());
            // Keep the connection-bars panel in sync when this download is selected.
            if self.selected_id() == Some(id) {
                if let Some(bars) = self.conn_bars.borrow().as_ref() {
                    bars.queue_draw();
                }
            }
        }
        let Some(iter) = self.row_iters.borrow().get(&id).copied() else {
            return;
        };
        let Some(mut rec) = self.records.borrow().get(&id).cloned() else {
            return;
        };
        let delta = speed as i64 - rec.current_speed as i64;
        rec.downloaded_bytes = downloaded;
        rec.total_bytes = total;
        rec.current_speed = speed;
        self.total_speed
            .set((self.total_speed.get() as i64 + delta).max(0) as u64);
        self.records.borrow_mut().insert(id, rec.clone());
        self.fill_row(&iter, &rec);
        let pct = percentage
            .unwrap_or_else(|| {
                total
                    .filter(|t| *t > 0)
                    .map(|t| downloaded as f64 / t as f64 * 100.0)
                    .unwrap_or(0.0)
            })
            .clamp(0.0, 100.0) as i32;
        self.store.set_value(&iter, COL_PROGRESS as u32, &pct.to_value());
        self.store.set_value(&iter, COL_SPEED as u32, &format!(
            "{}/s",
            ldm_engine::model::fmt::bytes(speed as i64)
        ).to_value());
        self.store.set_value(&iter, COL_STATUS as u32, &status_label(status).to_value());
        self.store
            .set_value(&iter, COL_STATUS_COLOR as u32, &status_color(status).to_value());
        let eta_text = rec
            .eta_seconds
            .filter(|s| *s > 0)
            .map(format_eta)
            .unwrap_or_else(|| "—".to_string());
        self.store.set_value(&iter, COL_ETA as u32, &eta_text.to_value());
        self.update_status_bar();
    }

    /// (Re)draw the connection bars for the selected download.
    pub fn redraw_conn_bars(&self) {
        if let Some(da) = self.conn_bars.borrow().as_ref() {
            da.queue_draw();
        }
    }

    fn fill_row(&self, iter: &gtk::TreeIter, r: &DownloadRecord) {
        let pct = r
            .total_bytes
            .filter(|t| *t > 0)
            .map(|t| r.downloaded_bytes as f64 / t as f64 * 100.0)
            .unwrap_or(0.0)
            .clamp(0.0, 100.0) as i32;
        let size_text = match r.total_bytes {
            Some(t) => format!(
                "{} / {}",
                ldm_engine::model::fmt::bytes(r.downloaded_bytes),
                ldm_engine::model::fmt::bytes(t)
            ),
            None => ldm_engine::model::fmt::bytes(r.downloaded_bytes),
        };
        let speed_text = if r.status.is_active() {
            format!("{}/s", ldm_engine::model::fmt::bytes(r.current_speed as i64))
        } else {
            "—".to_string()
        };
        let eta_text = match (r.eta_seconds, r.status.is_active()) {
            (Some(s), true) if s > 0 => format_eta(s),
            _ => "—".to_string(),
        };
        let status = r.status.as_str();
        self.store.set_value(iter, COL_ID as u32, &r.id.to_value());
        self.store.set_value(iter, COL_NAME as u32, &r.filename.to_value());
        self.store.set_value(iter, COL_SIZE as u32, &size_text.to_value());
        self.store.set_value(iter, COL_PROGRESS as u32, &pct.to_value());
        self.store.set_value(iter, COL_SPEED as u32, &speed_text.to_value());
        self.store.set_value(iter, COL_STATUS as u32, &status_label(status).to_value());
        self.store.set_value(iter, COL_CATEGORY as u32, &r.category.to_value());
        self.store.set_value(iter, COL_URL as u32, &r.url.to_value());
        self.store.set_value(iter, COL_DIR as u32, &r.dir_path.to_value());
        self.store.set_value(iter, COL_ETA as u32, &eta_text.to_value());
        self.store
            .set_value(iter, COL_STATUS_COLOR as u32, &status_color(status).to_value());
    }

    fn update_status_bar(&self) {
        let speed = ldm_engine::model::fmt::bytes(self.total_speed.get() as i64);
        let total = ldm_engine::model::fmt::bytes(self.total_bytes.get());
        self.status_label.set_text(&format!(
            "{} active  ·  {} queued  ·  {} total  ·  {} total size",
            self.active_count.get(),
            self.queued_count.get(),
            speed,
            total
        ));
    }

    pub fn current_view(&self) -> View {
        self.view.borrow().clone()
    }

    pub fn set_view(&self, view: View) {
        *self.view.borrow_mut() = view;
        let _ = self.refresh();
    }

    pub fn set_search(&self, text: &str) {
        *self.search_text.borrow_mut() = text.to_string();
        let _ = self.refresh();
    }

    pub fn rebuild_sidebar_queues(self: &Rc<Self>) {
        crate::window::rebuild_queues(self);
    }

    pub fn setup_tray(self: &Rc<Self>) {
        *self.tray.borrow_mut() = crate::tray::Tray::new(self);
    }

    pub fn apply_theme(&self) {
        let settings = self.rt.block_on(self.manager.get_settings());
        crate::theme::apply_theme(settings.theme);
    }
}
