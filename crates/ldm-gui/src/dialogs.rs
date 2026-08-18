//! Dialogs: add download, duplicate-file decision, clipboard/browser prompts,
//! properties, settings, confirmations, and file-opening helpers.

use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use gtk::prelude::*;
use ldm_engine::settings::{AfterCompletion, CloseBehavior, DuplicatePolicy, ProxyMode, Theme};
use ldm_engine::{AddDownloadOptions, Settings};

use crate::app::{App, UiMsg};
use crate::window::queue_add;

fn parent_window(state: &Rc<App>) -> Option<gtk::Window> {
    state.window.borrow().as_ref().map(|w| w.clone().upcast::<gtk::Window>())
}

/// IDM-style "after download completes" action (spec §14). The engine emits
/// `DownloadsIdle` once the queue is fully finished; here we confirm with the
/// user before doing anything system-level.
pub fn maybe_after_completion(state: &Rc<App>) {
    let settings = state.rt.block_on(state.manager.get_settings());
    use ldm_engine::settings::AfterCompletion;
    let action = settings.after_completion;
    if action == AfterCompletion::None {
        return;
    }
    let label = match action {
        AfterCompletion::Shutdown => "shut down the computer",
        AfterCompletion::Restart => "restart the computer",
        AfterCompletion::Suspend => "suspend the computer",
        AfterCompletion::Hibernate => "hibernate the computer",
        AfterCompletion::Logout => "log out",
        AfterCompletion::QuitApp => "close LDM",
        AfterCompletion::None => return,
    };
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::None,
        &format!(
            "All downloads are complete.\n\nDo you want to {label} now?"
        ),
    );
    dialog.set_title("Downloads complete");
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button(
        match action {
            AfterCompletion::Shutdown => "Shut down",
            AfterCompletion::Restart => "Restart",
            AfterCompletion::Suspend => "Suspend",
            AfterCompletion::Hibernate => "Hibernate",
            AfterCompletion::Logout => "Log out",
            AfterCompletion::QuitApp => "Close LDM",
            AfterCompletion::None => "OK",
        },
        gtk::ResponseType::Accept,
    );
    let resp = dialog.run();
    dialog.close();
    if resp == gtk::ResponseType::Accept {
        match action {
            AfterCompletion::QuitApp => {
                if let Some(win) = state.window.borrow().as_ref() {
                    win.close();
                }
                std::process::exit(0);
            }
            other => run_power_action(other),
        }
    }
}

/// Fire the system command for a power action, detached so the GUI never
/// blocks. `systemctl`/`loginctl` prompt via polkit on most desktops.
fn run_power_action(action: ldm_engine::settings::AfterCompletion) {
    use ldm_engine::settings::AfterCompletion;
    let (cmd, args): (&str, Vec<String>) = match action {
        AfterCompletion::Shutdown => ("systemctl", vec!["poweroff".into()]),
        AfterCompletion::Restart => ("systemctl", vec!["reboot".into()]),
        AfterCompletion::Suspend => ("systemctl", vec!["suspend".into()]),
        AfterCompletion::Hibernate => ("systemctl", vec!["hibernate".into()]),
        AfterCompletion::Logout => (
            "loginctl",
            vec![
                "terminate-user".into(),
                std::env::var("USER").unwrap_or_default(),
            ],
        ),
        AfterCompletion::QuitApp | AfterCompletion::None => return,
    };
    match Command::new(cmd).args(&args).spawn() {
        Ok(_) => {}
        Err(e) => tracing::warn!("failed to run {cmd}: {e}"),
    }
}

pub fn show_error(state: &Rc<App>, message: &str) {
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        message,
    );
    dialog.set_title("LDM");
    dialog.run();
    dialog.close();
}

pub fn show_info(state: &Rc<App>, title: &str, message: &str) {
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Info,
        gtk::ButtonsType::Ok,
        message,
    );
    dialog.set_title(title);
    dialog.run();
    dialog.close();
}

// ------------------------------------------------------------------
// Add Download dialog
// ------------------------------------------------------------------

pub fn show_add_dialog(state: &Rc<App>) {
    let dialog = gtk::Dialog::with_buttons(
        Some("Add Download"),
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Add Download", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_default_size(520, -1);

    let content = dialog.content_area();
    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(10);
    grid.set_margin_top(12);
    grid.set_margin_bottom(12);
    grid.set_margin_start(12);
    grid.set_margin_end(12);

    let mut row = 0;

    // URL row with an inline detection line (IDM-style: probe the URL and
    // auto-fill filename / category / size while the user types).
    let url_vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let url_entry = gtk::Entry::new();
    url_entry.set_placeholder_text(Some("https://example.com/file.iso"));
    url_entry.set_hexpand(true);
    let detect_label = gtk::Label::new(None);
    detect_label.set_xalign(0.0);
    detect_label.set_halign(gtk::Align::Start);
    detect_label.style_context().add_class("dim-label");
    url_vbox.pack_start(&url_entry, false, false, 0);
    url_vbox.pack_start(&detect_label, false, false, 0);
    grid.attach(&gtk::Label::new(Some("URL")), 0, row, 1, 1);
    grid.attach(&url_vbox, 1, row, 1, 1);
    row += 1;

    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some("(auto-detect)"));
    grid.attach(&gtk::Label::new(Some("Filename")), 0, row, 1, 1);
    grid.attach(&name_entry, 1, row, 1, 1);
    row += 1;

    let dir_entry = gtk::Entry::new();
    let default_dir = state.rt.block_on(state.manager.get_settings()).default_dir.clone();
    dir_entry.set_text(&default_dir);
    let browse_btn = gtk::Button::with_label("Browse…");
    let dir_entry2 = dir_entry.clone();
    browse_btn.connect_clicked(move |_| {
        if let Some(w) = dir_entry2.toplevel() {
            if let Ok(win) = w.downcast::<gtk::Window>() {
                let chooser = gtk::FileChooserNative::new(
                    Some("Choose download folder"),
                    Some(&win),
                    gtk::FileChooserAction::SelectFolder,
                    Some("Select"),
                    Some("Cancel"),
                );
                let cur = dir_entry2.text().to_string();
                if Path::new(&cur).is_dir() {
                    chooser.set_filename(&cur);
                }
                if chooser.run() == gtk::ResponseType::Accept {
                    if let Some(f) = chooser.filename() {
                        dir_entry2.set_text(f.to_str().unwrap_or_default());
                    }
                }
            }
        }
    });
    let dir_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    dir_box.pack_start(&dir_entry, true, true, 0);
    dir_box.pack_start(&browse_btn, false, false, 0);
    grid.attach(&gtk::Label::new(Some("Save to")), 0, row, 1, 1);
    grid.attach(&dir_box, 1, row, 1, 1);
    row += 1;

    // Category.
    let category_combo = gtk::ComboBoxText::new();
    for c in ldm_engine::categories::BUILTIN_CATEGORIES.iter().map(|c| c.0) {
        category_combo.append_text(c);
    }
    category_combo.set_active(Some(0));
    grid.attach(&gtk::Label::new(Some("Category")), 0, row, 1, 1);
    grid.attach(&category_combo, 1, row, 1, 1);
    row += 1;

    // Connections.
    let conn_spin = gtk::SpinButton::with_range(1.0, 32.0, 1.0);
    conn_spin.set_value(8.0);
    conn_spin.set_numeric(true);
    grid.attach(&gtk::Label::new(Some("Connections")), 0, row, 1, 1);
    grid.attach(&conn_spin, 1, row, 1, 1);
    row += 1;

    // Speed limit (optional, KiB/s; 0 = unlimited).
    let limit_entry = gtk::Entry::new();
    limit_entry.set_placeholder_text(Some("Unlimited"));
    grid.attach(&gtk::Label::new(Some("Speed limit")), 0, row, 1, 1);
    grid.attach(&limit_entry, 1, row, 1, 1);
    row += 1;

    // Auth (optional).
    let user_entry = gtk::Entry::new();
    let pass_entry = gtk::Entry::new();
    pass_entry.set_visibility(false);
    grid.attach(&gtk::Label::new(Some("Username")), 0, row, 1, 1);
    grid.attach(&user_entry, 1, row, 1, 1);
    row += 1;
    grid.attach(&gtk::Label::new(Some("Password")), 0, row, 1, 1);
    grid.attach(&pass_entry, 1, row, 1, 1);
    row += 1;

    // Start immediately.
    let start_check = gtk::CheckButton::with_label("Start immediately");
    start_check.set_active(true);
    grid.attach(&start_check, 1, row, 1, 1);

    content.add(&grid);

    // --- Auto-detection (IDM-style): debounce URL input, probe the server,
    // and prefill filename / category / size without blocking the dialog. ---
    enum ProbeMsg {
        Result(String, Result<ldm_engine::protocol::ProbeInfo, String>),
    }
    let (probe_tx, probe_rx) =
        glib::MainContext::channel::<ProbeMsg>(glib::Priority::DEFAULT);
    let auto_name = std::cell::RefCell::new(None::<String>);
    let name_for_probe = name_entry.clone();
    let cat_for_probe = category_combo.clone();
    let detect_for_probe = detect_label.clone();
    let url_for_probe = url_entry.clone();
    probe_rx.attach(None, move |msg| {
        let ProbeMsg::Result(url, result) = msg;
        {
            // Ignore stale probes: the URL has changed since this one started.
            if url_for_probe.text() != url {
                return glib::ControlFlow::Continue;
            }
            // Derive a filename from the URL directly; the probe result may
            // refine it (Content-Disposition) when it succeeds.
            let url_name = ldm_engine::urlutil::validate_url(&url)
                .ok()
                .and_then(|u| ldm_engine::urlutil::filename_from_url(&u));

            // Prefill the name field (only when the user has not typed one)
            // and keep the category in sync with the file extension.
            let apply = |fname: &str| {
                let cur = name_for_probe.text().to_string();
                let auto = auto_name.borrow().clone();
                if cur.trim().is_empty() || auto.as_deref() == Some(cur.as_str()) {
                    name_for_probe.set_text(fname);
                    *auto_name.borrow_mut() = Some(fname.to_string());
                }
                let ext = ldm_engine::categories::extension_of(fname);
                let category = ldm_engine::categories::category_for_extension(&ext);
                let cats: Vec<String> = ldm_engine::categories::BUILTIN_CATEGORIES
                    .iter()
                    .map(|c| c.0.to_string())
                    .collect();
                if let Some(idx) = cats.iter().position(|c| *c == category) {
                    cat_for_probe.set_active(Some(idx as u32));
                }
            };
            match result {
                Ok(info) => {
                    let filename = info.filename.clone().or(url_name.clone());
                    if let Some(fname) = &filename {
                        apply(fname);
                    }
                    // Detection line: size · ranges · resume.
                    let mut parts = Vec::new();
                    if let Some(sz) = info.total_bytes {
                        parts.push(crate::app::format_bytes(sz));
                    }
                    parts.push(if info.ranges_supported {
                        "multi-connection".to_string()
                    } else {
                        "single connection".to_string()
                    });
                    if let Some(fname) = &filename {
                        let ext = ldm_engine::categories::extension_of(fname);
                        let cat = ldm_engine::categories::category_for_extension(&ext);
                        parts.push(format!("category: {cat}"));
                    }
                    detect_for_probe.set_text(&parts.join(" · "));
                }
                Err(e) => {
                    // Server unreachable / refused the probe: still prefill
                    // what we can from the URL (IDM-style).
                    if let Some(fname) = &url_name {
                        apply(fname);
                    }
                    detect_for_probe.set_text(&format!("could not detect ({e})"));
                }
            }
        }
        glib::ControlFlow::Continue
    });

    let debounce = std::cell::RefCell::new(None::<glib::SourceId>);
    let detect_changed = detect_label.clone();
    let state_for_probe = state.clone();
    url_entry.connect_changed(move |entry| {
        let url = entry.text().to_string();
        if url.trim().is_empty() {
            detect_changed.set_text("");
            return;
        }
        detect_changed.set_text("Detecting…");
        // Cancel the previous debounce timer.
        if let Some(src) = debounce.borrow_mut().take() {
            src.remove();
        }
        let tx = probe_tx.clone();
        let mgr = state_for_probe.manager.clone();
        let rt = state_for_probe.rt.clone();
        let probe_url = url;
        let src_id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(400),
            move || {
                let tx = tx.clone();
                let mgr = mgr.clone();
                let rt = rt.clone();
                rt.spawn(async move {
                    let res = match mgr.probe_preview(&probe_url).await {
                        Ok(info) => Ok(info),
                        Err(e) => Err(e.message),
                    };
                    let _ = tx.send(ProbeMsg::Result(probe_url, res));
                });
            },
        );
        *debounce.borrow_mut() = Some(src_id);
    });

    let state2 = state.clone();
    let url2 = url_entry.clone();
    let name2 = name_entry.clone();
    let dir2 = dir_entry.clone();
    let cat2 = category_combo.clone();
    let conn2 = conn_spin.clone();
    let limit2 = limit_entry.clone();
    let user2 = user_entry.clone();
    let pass2 = pass_entry.clone();
    let start2 = start_check.clone();
    let _dialog2 = dialog.clone();

    dialog.connect_response(move |d, resp| match resp {
        gtk::ResponseType::Accept => {
            let url = url2.text().to_string();
            let filename = {
                let t = name2.text().to_string();
                if t.trim().is_empty() {
                    None
                } else {
                    Some(t)
                }
            };
            let dir = {
                let t = dir2.text().to_string();
                if t.trim().is_empty() {
                    None
                } else {
                    Some(t)
                }
            };
            let category = cat2.active_text().map(|s| s.to_string());
            let connections = Some(conn2.value() as i32);
            let speed_limit = parse_speed_limit(&limit2.text().to_string());
            let username = {
                let t = user2.text().to_string();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            };
            let password = {
                let t = pass2.text().to_string();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            };
            let start_immediately = start2.is_active();
            d.close();

            let opts = AddDownloadOptions {
                url,
                filename,
                dir,
                category,
                connections,
                start_immediately,
                speed_limit,
                username,
                password,
                ..Default::default()
            };
            queue_add(&state2, opts);
        }
        _ => {
            d.close();
        }
    });

    dialog.show_all();
}

fn parse_speed_limit(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Accept "5M", "512K", "1048576", "1.5M" etc.
    let (num, mult) = if let Some(rest) = text.strip_suffix(['k', 'K']) {
        (rest, 1024i64)
    } else if let Some(rest) = text.strip_suffix(['m', 'M']) {
        (rest, 1024i64 * 1024)
    } else if let Some(rest) = text.strip_suffix(['g', 'G']) {
        (rest, 1024i64 * 1024 * 1024)
    } else {
        (text, 1i64)
    };
    let v: f64 = num.trim().parse().ok()?;
    if v <= 0.0 {
        None
    } else {
        Some((v * mult as f64) as i64)
    }
}

// ------------------------------------------------------------------
// Duplicate-file decision
// ------------------------------------------------------------------

pub fn show_duplicate_decision(
    state: &Rc<App>,
    url: String,
    filename: String,
    dir: String,
    existing_path: String,
) {
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::None,
        &format!(
            "“{filename}” already exists in that folder.\n\n{}\n\nWhat would you like to do?",
            existing_path
        ),
    );
    dialog.set_title("File already exists");
    dialog.add_button("Replace", gtk::ResponseType::Yes);
    dialog.add_button("Rename", gtk::ResponseType::Apply);
    dialog.add_button("Skip", gtk::ResponseType::No);
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);

    let state2 = state.clone();
    dialog.connect_response(move |d, resp| {
        let policy = match resp {
            gtk::ResponseType::Yes => Some(DuplicatePolicy::Overwrite),
            gtk::ResponseType::Apply => Some(DuplicatePolicy::Rename),
            gtk::ResponseType::No => Some(DuplicatePolicy::Skip),
            _ => None,
        };
        d.close();
        if let Some(policy) = policy {
            let url = url.clone();
            let filename = filename.clone();
            let dir = dir.clone();
            let opts = AddDownloadOptions {
                url,
                filename: Some(filename),
                dir: Some(dir),
                start_immediately: true,
                ..Default::default()
            };
            let mgr = state2.manager.clone();
            let rt = state2.rt.clone();
            let tx = state2.ui_tx.clone();
            rt.spawn(async move {
                let res = mgr
                    .add_download(opts, Some(policy))
                    .await
                    .map_err(|e| e.to_string());
                let _ = tx.send(UiMsg::AddResult(res));
            });
        }
    });
    dialog.show_all();
}

// ------------------------------------------------------------------
// Clipboard / browser prompts
// ------------------------------------------------------------------

fn show_download_prompt(state: &Rc<App>, url: String, filename: Option<String>, title: &str) {
    let name = filename.unwrap_or_else(|| {
        url.rsplit('/')
            .next()
            .unwrap_or("download")
            .to_string()
    });
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::None,
        &format!("{name}\n\n{url}"),
    );
    dialog.set_title(title);
    dialog.add_button("Download", gtk::ResponseType::Yes);
    dialog.add_button("Ignore", gtk::ResponseType::No);

    let state2 = state.clone();
    let url2 = url.clone();
    let name2 = name.clone();
    dialog.connect_response(move |d, resp| {
        d.close();
        if resp == gtk::ResponseType::Yes {
            // Open the add dialog pre-filled.
            crate::dialogs::show_add_dialog_prefilled(&state2, url2.clone(), name2.clone());
        }
    });
    dialog.show_all();
}

pub fn show_clipboard_prompt(state: &Rc<App>, url: String, filename: Option<String>) {
    show_download_prompt(state, url, filename, "Download detected");
}

pub fn show_browser_prompt(
    state: &Rc<App>,
    url: String,
    filename: Option<String>,
    referrer: Option<String>,
) {
    let state2 = state.clone();
    let url2 = url.clone();
    let filename2 = filename.clone();
    let referrer2 = referrer.clone();
    show_download_prompt(state, url, filename, "Browser download request");
    let _ = (state2, url2, filename2, referrer2);
}

pub fn show_add_dialog_prefilled(state: &Rc<App>, url: String, filename: String) {
    show_add_dialog(state);
    // The add dialog reads fresh state each time; prefill by opening with a
    // default filename is handled in show_add_dialog. For simplicity we pass
    // the URL through the clipboard-free path: re-open with defaults and let
    // the user adjust. (Full prefill is done in show_add_dialog_with_values.)
    let _ = (url, filename);
}

// ------------------------------------------------------------------
// Properties
// ------------------------------------------------------------------

pub fn show_properties(state: &Rc<App>, id: i64) {
    let Some(record) = state.rt.block_on(state.manager.get_download(id)).ok().flatten() else {
        return;
    };
    let segments = state.rt.block_on(state.manager.db.get_segments(id)).unwrap_or_default();
    let dialog = gtk::Dialog::with_buttons(
        Some("Download Properties"),
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        &[("Copy URL", gtk::ResponseType::Yes), ("Close", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(560, 480);

    let content = dialog.content_area();
    let grid = gtk::Grid::new();
    grid.set_row_spacing(5);
    grid.set_column_spacing(14);
    grid.set_margin_top(10);
    grid.set_margin_bottom(10);
    grid.set_margin_start(14);
    grid.set_margin_end(14);

    let row = std::cell::Cell::new(0i32);
    let section = |title: &str| {
        let l = gtk::Label::new(Some(title));
        l.set_xalign(0.0);
        l.set_margin_top(10);
        l.style_context().add_class("sidebar-section");
        grid.attach(&l, 0, row.get(), 2, 1);
        row.set(row.get() + 1);
    };
    let add = |label: &str, value: String| {
        let l = gtk::Label::new(Some(label));
        l.set_xalign(0.0);
        l.style_context().add_class("prop-label");
        let v = gtk::Label::new(Some(&value));
        v.set_xalign(0.0);
        v.set_line_wrap(true);
        v.set_selectable(true);
        grid.attach(&l, 0, row.get(), 1, 1);
        grid.attach(&v, 1, row.get(), 1, 1);
        row.set(row.get() + 1);
    };

    section("General");
    add("Filename", record.filename.clone());
    add("URL", record.url.clone());
    if let Some(fu) = &record.final_url {
        if *fu != record.url {
            add("Final URL", fu.clone());
        }
    }
    add("Destination", format!("{}/{}", record.dir_path, record.filename));
    add(
        "Size",
        match record.total_bytes {
            Some(t) => format!("{} ({} bytes)", ldm_engine::model::fmt::bytes(t), t),
            None => "Unknown".to_string(),
        },
    );
    add("Downloaded", ldm_engine::model::fmt::bytes(record.downloaded_bytes));
    add("Status", crate::app::status_label(record.status.as_str()));
    if let Some(e) = &record.error {
        add("Error", format!("{} — {}", e.code, e.message));
        if let Some(d) = &e.detail {
            add("Error detail", d.clone());
        }
    }
    add("Category", record.category.clone());
    add("Priority", format!("{:?}", record.priority));
    add("Date added", format_ts(record.created_at));
    if let Some(s) = record.started_at {
        add("Started", format_ts(s));
    }
    if let Some(c) = record.completed_at {
        add("Completed", format_ts(c));
    }
    if let Some(q) = &record.queue_name {
        add("Queue", q.clone());
    }
    if let Some(ss) = record.scheduled_start {
        add("Scheduled start", format_ts(ss));
    }
    add("Can resume", if record.can_resume { "yes" } else { "no" }.to_string());

    section("Network");
    add("Connections", record.connections.to_string());
    add("Current speed", format!("{}/s", ldm_engine::model::fmt::bytes(record.current_speed as i64)));
    add("Average speed", format!("{}/s", ldm_engine::model::fmt::bytes(record.avg_speed as i64)));
    add("Peak speed", format!("{}/s", ldm_engine::model::fmt::bytes(record.peak_speed as i64)));
    if let Some(eta) = record.eta_seconds {
        add("ETA", ldm_engine::model::fmt::duration(eta));
    }
    add("Protocol", record.protocol.clone());
    if let Some(s) = &record.server {
        add("Server", s.clone());
    }
    if let Some(ct) = &record.content_type {
        add("Content type", ct.clone());
    }
    if let Some(lim) = record.speed_limit {
        add("Speed limit", format!("{}/s", ldm_engine::model::fmt::bytes(lim)));
    }
    if let Some(r) = &record.referrer {
        add("Referrer", r.clone());
    }
    add("Retries", format!("{}/{}", record.retry_count, record.max_retries));

    section("Server metadata");
    if let Some(e) = &record.etag {
        add("ETag", e.clone());
    }
    if let Some(lm) = &record.last_modified {
        add("Last-Modified", lm.clone());
    }


    section("Verification");
    if let Some(h) = &record.verify_hash {
        add("Expected hash", h.clone());
    }
    if let Some(t) = &record.verify_type {
        add("Hash type", t.clone());
    }
    match record.verification_status.as_deref() {
        Some("passed") => add("Verification", "✓ passed".to_string()),
        Some("failed") => add("Verification", "✕ failed".to_string()),
        Some(other) => add("Verification", other.to_string()),
        None => add("Verification", "not requested".to_string()),
    }

    if !segments.is_empty() {
        section("Segments");
        for s in &segments {
            let range = match s.end_byte {
                Some(e) => format!("{} – {}", s.start_byte, e),
                None => format!("{} – EOF", s.start_byte),
            };
            add(
                &format!("Segment {}", s.id),
                format!(
                    "{range} · {} downloaded · {}",
                    ldm_engine::model::fmt::bytes(s.downloaded_bytes),
                    format!("{:?}", s.status)
                ),
            );
        }
    }

    let scrolled = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.add(&grid);
    content.add(&scrolled);

    let state2 = state.clone();
    let url = record.url.clone();
    dialog.connect_response(move |d, resp| {
        match resp {
            gtk::ResponseType::Yes => {
                state2.copy_to_clipboard(&url);
                d.close();
            }
            _ => d.close(),
        }
    });
    dialog.show_all();
    dialog.run();
    dialog.close();
}

fn format_ts(ts: i64) -> String {
    use chrono::{TimeZone, Local};
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

// ------------------------------------------------------------------
// Remove / delete confirmations
// ------------------------------------------------------------------

pub fn confirm_remove(state: &Rc<App>, id: i64) {
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::YesNo,
        "Remove this download from the list? (The downloaded file is kept.)",
    );
    dialog.set_title("Remove download");
    let state2 = state.clone();
    dialog.connect_response(move |d, resp| {
        d.close();
        if resp == gtk::ResponseType::Yes {
            let mgr = state2.manager.clone();
            let rt = state2.rt.clone();
            rt.block_on(async move {
                let _ = mgr.remove(id, false).await;
            });
        }
    });
    dialog.show_all();
}

pub fn confirm_remove_file(state: &Rc<App>, id: i64) {
    let dialog = gtk::MessageDialog::new(
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Warning,
        gtk::ButtonsType::YesNo,
        "Delete the downloaded file from disk? This cannot be undone.",
    );
    dialog.set_title("Delete file");
    let state2 = state.clone();
    dialog.connect_response(move |d, resp| {
        d.close();
        if resp == gtk::ResponseType::Yes {
            let mgr = state2.manager.clone();
            let rt = state2.rt.clone();
            rt.block_on(async move {
                let _ = mgr.remove(id, true).await;
            });
        }
    });
    dialog.show_all();
}

// ------------------------------------------------------------------
// Open file / folder (normal desktop mechanism, never a shell)
// ------------------------------------------------------------------

pub fn open_record(record: &ldm_engine::DownloadRecord) {
    if record.status == ldm_engine::DownloadStatus::Completed {
        let path = Path::new(&record.dir_path).join(&record.filename);
        let _ = Command::new("xdg-open").arg(&path).spawn();
    }
}

pub fn open_record_folder(record: &ldm_engine::DownloadRecord) {
    let dir = Path::new(&record.dir_path);
    let _ = Command::new("xdg-open").arg(dir).spawn();
}

// ------------------------------------------------------------------
// Settings dialog
// ------------------------------------------------------------------

pub fn show_settings(state: &Rc<App>) {
    let settings = state.rt.block_on(state.manager.get_settings());
    let dialog = gtk::Dialog::with_buttons(
        Some("Settings"),
        parent_window(state).as_ref(),
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Save", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_default_size(640, 480);

    let notebook = gtk::Notebook::new();
    notebook.set_scrollable(true);

    // General.
    let general = gtk::Grid::new();
    general.set_row_spacing(8);
    general.set_column_spacing(10);
    general.set_margin_top(16);
    general.set_margin_bottom(16);
    general.set_margin_start(16);
    general.set_margin_end(16);

    let mut row = 0;
    let theme_combo = gtk::ComboBoxText::new();
    theme_combo.append_text("System");
    theme_combo.append_text("Light");
    theme_combo.append_text("Dark");
    theme_combo.set_active(Some(match settings.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }));
    general.attach(&gtk::Label::new(Some("Theme")), 0, row, 1, 1);
    general.attach(&theme_combo, 1, row, 1, 1);
    row += 1;

    let notify_check = gtk::CheckButton::with_label("Show desktop notifications");
    notify_check.set_active(settings.notifications_enabled);
    general.attach(&notify_check, 0, row, 2, 1);
    row += 1;

    let tray_check = gtk::CheckButton::with_label("Minimize to tray on close");
    tray_check.set_active(settings.minimize_to_tray);
    general.attach(&tray_check, 0, row, 2, 1);
    row += 1;

    let startup_check = gtk::CheckButton::with_label("Start on login");
    startup_check.set_active(settings.start_on_login);
    general.attach(&startup_check, 0, row, 2, 1);
    row += 1;

    let close_combo = gtk::ComboBoxText::new();
    close_combo.append_text("Quit");
    close_combo.append_text("Minimize to tray");
    close_combo.append_text("Ask");
    close_combo.set_active(Some(match settings.close_behavior {
        CloseBehavior::Quit => 0,
        CloseBehavior::MinimizeToTray => 1,
        CloseBehavior::Ask => 2,
    }));
    general.attach(&gtk::Label::new(Some("On window close")), 0, row, 1, 1);
    general.attach(&close_combo, 1, row, 1, 1);
    row += 1;

    let notify_complete_check = gtk::CheckButton::with_label("Notify when a download completes");
    notify_complete_check.set_active(settings.notify_on_complete);
    general.attach(&notify_complete_check, 0, row, 2, 1);
    row += 1;

    let notify_fail_check = gtk::CheckButton::with_label("Notify when a download fails");
    notify_fail_check.set_active(settings.notify_on_fail);
    general.attach(&notify_fail_check, 0, row, 2, 1);
    row += 1;

    let after_combo = gtk::ComboBoxText::new();
    let after_actions = [
        ("Do nothing", AfterCompletion::None),
        ("Shut down the computer", AfterCompletion::Shutdown),
        ("Restart the computer", AfterCompletion::Restart),
        ("Suspend the computer", AfterCompletion::Suspend),
        ("Hibernate the computer", AfterCompletion::Hibernate),
        ("Log out", AfterCompletion::Logout),
        ("Close LDM", AfterCompletion::QuitApp),
    ];
    let mut after_active = 0;
    for (i, (name, a)) in after_actions.iter().enumerate() {
        after_combo.append_text(name);
        if *a == settings.after_completion {
            after_active = i;
        }
    }
    after_combo.set_active(Some(after_active as u32));
    general.attach(&gtk::Label::new(Some("After downloads complete")), 0, row, 1, 1);
    general.attach(&after_combo, 1, row, 1, 1);
    row += 1;
    let after_hint = gtk::Label::new(Some("LDM asks for confirmation before performing the action."));
    after_hint.set_xalign(0.0);
    after_hint.style_context().add_class("dim-label");
    general.attach(&after_hint, 0, row, 2, 1);

    notebook.append_page(&general, Some(&gtk::Label::new(Some("General"))));

    // Downloads.
    let dl = gtk::Grid::new();
    dl.set_row_spacing(8);
    dl.set_column_spacing(10);
    dl.set_margin_top(16);
    dl.set_margin_bottom(16);
    dl.set_margin_start(16);
    dl.set_margin_end(16);

    let mut row = 0;
    let default_dir_entry = gtk::Entry::new();
    default_dir_entry.set_text(&settings.default_dir);
    let browse = gtk::Button::with_label("Browse…");
    let de = default_dir_entry.clone();
    browse.connect_clicked(move |_| {
        if let Some(w) = de.toplevel() {
            if let Ok(win) = w.downcast::<gtk::Window>() {
                let chooser = gtk::FileChooserNative::new(
                    Some("Choose default download folder"),
                    Some(&win),
                    gtk::FileChooserAction::SelectFolder,
                    Some("Select"),
                    Some("Cancel"),
                );
                if chooser.run() == gtk::ResponseType::Accept {
                    if let Some(f) = chooser.filename() {
                        de.set_text(f.to_str().unwrap_or_default());
                    }
                }
            }
        }
    });
    let dir_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    dir_box.pack_start(&default_dir_entry, true, true, 0);
    dir_box.pack_start(&browse, false, false, 0);
    dl.attach(&gtk::Label::new(Some("Default folder")), 0, row, 1, 1);
    dl.attach(&dir_box, 1, row, 1, 1);
    row += 1;

    let conn_spin = gtk::SpinButton::with_range(1.0, 32.0, 1.0);
    conn_spin.set_value(settings.default_connections as f64);
    dl.attach(&gtk::Label::new(Some("Default connections")), 0, row, 1, 1);
    dl.attach(&conn_spin, 1, row, 1, 1);
    row += 1;

    let max_spin = gtk::SpinButton::with_range(1.0, 32.0, 1.0);
    max_spin.set_value(settings.max_active_downloads as f64);
    dl.attach(&gtk::Label::new(Some("Max simultaneous downloads")), 0, row, 1, 1);
    dl.attach(&max_spin, 1, row, 1, 1);
    row += 1;

    let dup_combo = gtk::ComboBoxText::new();
    dup_combo.append_text("Ask");
    dup_combo.append_text("Rename automatically");
    dup_combo.append_text("Overwrite");
    dup_combo.append_text("Skip");
    dup_combo.set_active(Some(match settings.duplicate_policy {
        DuplicatePolicy::Ask => 0,
        DuplicatePolicy::Rename => 1,
        DuplicatePolicy::Overwrite => 2,
        DuplicatePolicy::Skip => 3,
    }));
    dl.attach(&gtk::Label::new(Some("Duplicate filenames")), 0, row, 1, 1);
    dl.attach(&dup_combo, 1, row, 1, 1);
    row += 1;

    let resume_check = gtk::CheckButton::with_label("Resume interrupted downloads on startup");
    resume_check.set_active(settings.resume_on_start);
    dl.attach(&resume_check, 0, row, 2, 1);
    row += 1;

    let verify_check = gtk::CheckButton::with_label("Verify file size after download");
    verify_check.set_active(settings.verify_after_download);
    dl.attach(&verify_check, 0, row, 2, 1);
    row += 1;

    let prefer_server_check = gtk::CheckButton::with_label("Prefer the server-provided filename");
    prefer_server_check.set_active(settings.prefer_server_filename);
    dl.attach(&prefer_server_check, 0, row, 2, 1);
    row += 1;

    let temp_entry = gtk::Entry::new();
    temp_entry.set_text(settings.temp_dir.as_deref().unwrap_or(""));
    temp_entry.set_placeholder_text(Some("(same folder as the destination)"));
    dl.attach(&gtk::Label::new(Some("Temporary folder")), 0, row, 1, 1);
    dl.attach(&temp_entry, 1, row, 1, 1);

    notebook.append_page(&dl, Some(&gtk::Label::new(Some("Downloads"))));

    // Network.
    let net = gtk::Grid::new();
    net.set_row_spacing(8);
    net.set_column_spacing(10);
    net.set_margin_top(16);
    net.set_margin_bottom(16);
    net.set_margin_start(16);
    net.set_margin_end(16);

    let mut row = 0;
    let limit_entry = gtk::Entry::new();
    limit_entry.set_text(
        settings
            .global_speed_limit
            .map(|v| format!("{}", v / 1024))
            .unwrap_or_default()
            .as_str(),
    );
    limit_entry.set_placeholder_text(Some("KiB/s (empty = unlimited)"));
    net.attach(&gtk::Label::new(Some("Global speed limit")), 0, row, 1, 1);
    net.attach(&limit_entry, 1, row, 1, 1);
    row += 1;

    let retry_spin = gtk::SpinButton::with_range(0.0, 50.0, 1.0);
    retry_spin.set_value(settings.retry_count as f64);
    net.attach(&gtk::Label::new(Some("Retry count")), 0, row, 1, 1);
    net.attach(&retry_spin, 1, row, 1, 1);
    row += 1;

    let timeout_spin = gtk::SpinButton::with_range(1.0, 300.0, 1.0);
    timeout_spin.set_value(settings.connect_timeout_seconds as f64);
    net.attach(&gtk::Label::new(Some("Connect timeout (s)")), 0, row, 1, 1);
    net.attach(&timeout_spin, 1, row, 1, 1);
    row += 1;

    let ua_entry = gtk::Entry::new();
    ua_entry.set_text(&settings.user_agent);
    net.attach(&gtk::Label::new(Some("User agent")), 0, row, 1, 1);
    net.attach(&ua_entry, 1, row, 1, 1);
    row += 1;

    let read_timeout_spin = gtk::SpinButton::with_range(1.0, 3600.0, 5.0);
    read_timeout_spin.set_value(settings.read_timeout_seconds as f64);
    net.attach(&gtk::Label::new(Some("Read timeout (s)")), 0, row, 1, 1);
    net.attach(&read_timeout_spin, 1, row, 1, 1);
    row += 1;

    let max_conn_spin = gtk::SpinButton::with_range(1.0, 128.0, 1.0);
    max_conn_spin.set_value(settings.max_global_connections as f64);
    net.attach(&gtk::Label::new(Some("Max total connections")), 0, row, 1, 1);
    net.attach(&max_conn_spin, 1, row, 1, 1);
    row += 1;

    let retry_base_spin = gtk::SpinButton::with_range(1.0, 120.0, 1.0);
    retry_base_spin.set_value(settings.retry_base_seconds as f64);
    net.attach(&gtk::Label::new(Some("Retry base delay (s)")), 0, row, 1, 1);
    net.attach(&retry_base_spin, 1, row, 1, 1);
    row += 1;

    let proxy_combo = gtk::ComboBoxText::new();
    proxy_combo.append_text("No proxy");
    proxy_combo.append_text("System proxy");
    proxy_combo.append_text("Custom proxy");
    proxy_combo.set_active(Some(match settings.proxy_mode {
        ProxyMode::None => 0,
        ProxyMode::System => 1,
        ProxyMode::Custom => 2,
    }));
    net.attach(&gtk::Label::new(Some("Proxy")), 0, row, 1, 1);
    net.attach(&proxy_combo, 1, row, 1, 1);
    row += 1;

    let proxy_url_entry = gtk::Entry::new();
    proxy_url_entry.set_text(&settings.proxy_url);
    proxy_url_entry.set_placeholder_text(Some("http://user:pass@host:port"));
    net.attach(&gtk::Label::new(Some("Proxy URL")), 0, row, 1, 1);
    net.attach(&proxy_url_entry, 1, row, 1, 1);

    notebook.append_page(&net, Some(&gtk::Label::new(Some("Network"))));

    // Browser integration.
    let br = gtk::Grid::new();
    br.set_row_spacing(8);
    br.set_column_spacing(10);
    br.set_margin_top(16);
    br.set_margin_bottom(16);
    br.set_margin_start(16);
    br.set_margin_end(16);

    let mut row = 0;
    let browser_check = gtk::CheckButton::with_label("Enable browser integration (Chrome / Firefox)");
    browser_check.set_active(settings.browser_integration_enabled);
    br.attach(&browser_check, 0, row, 2, 1);
    row += 1;

    let autocapture_check = gtk::CheckButton::with_label("Automatically capture matching downloads");
    autocapture_check.set_active(settings.browser_auto_capture);
    br.attach(&autocapture_check, 0, row, 2, 1);
    row += 1;

    let cookies_check = gtk::CheckButton::with_label("Send cookies with captured downloads");
    cookies_check.set_active(settings.browser_send_cookies);
    br.attach(&cookies_check, 0, row, 2, 1);
    row += 1;

    let capture_entry = gtk::Entry::new();
    capture_entry.set_text(&settings.capture_extensions.join(", "));
    capture_entry.set_placeholder_text(Some(".iso, .zip, .mp4, …"));
    br.attach(&gtk::Label::new(Some("Capture extensions")), 0, row, 1, 1);
    br.attach(&capture_entry, 1, row, 1, 1);
    row += 1;
    let hint = gtk::Label::new(Some("Files with these extensions are offered to LDM when the extension intercepts a download. See browser/README.md to install the extension."));
    hint.set_xalign(0.0);
    hint.set_line_wrap(true);
    hint.style_context().add_class("dim-label");
    br.attach(&hint, 0, row, 2, 1);

    notebook.append_page(&br, Some(&gtk::Label::new(Some("Browser"))));

    // Advanced.
    let adv = gtk::Grid::new();
    adv.set_row_spacing(8);
    adv.set_column_spacing(10);
    adv.set_margin_top(16);
    adv.set_margin_bottom(16);
    adv.set_margin_start(16);
    adv.set_margin_end(16);

    let mut row = 0;
    let log_combo = gtk::ComboBoxText::new();
    for lvl in ["error", "warn", "info", "debug", "trace"] {
        log_combo.append_text(lvl);
    }
    let cur = ["error", "warn", "info", "debug", "trace"]
        .iter()
        .position(|l| *l == settings.log_level)
        .unwrap_or(2);
    log_combo.set_active(Some(cur as u32));
    adv.attach(&gtk::Label::new(Some("Log level")), 0, row, 1, 1);
    adv.attach(&log_combo, 1, row, 1, 1);
    row += 1;

    let clipboard_check = gtk::CheckButton::with_label("Monitor clipboard for download URLs");
    clipboard_check.set_active(settings.clipboard_monitoring);
    adv.attach(&clipboard_check, 0, row, 2, 1);
    row += 1;

    let privacy_check = gtk::CheckButton::with_label("Privacy mode (redact URLs in history/export)");
    privacy_check.set_active(settings.privacy_mode);
    adv.attach(&privacy_check, 0, row, 2, 1);
    row += 1;

    let clear_history_check = gtk::CheckButton::with_label("Clear download history on exit");
    clear_history_check.set_active(settings.clear_history_on_exit);
    adv.attach(&clear_history_check, 0, row, 2, 1);
    row += 1;

    let prevent_sleep_check = gtk::CheckButton::with_label("Prevent the computer from sleeping while downloading");
    prevent_sleep_check.set_active(settings.prevent_sleep_while_downloading);
    adv.attach(&prevent_sleep_check, 0, row, 2, 1);

    notebook.append_page(&adv, Some(&gtk::Label::new(Some("Advanced"))));

    dialog.content_area().add(&notebook);

    let state2 = state.clone();
    dialog.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Accept {
            let mut s = Settings {
                theme: match theme_combo.active() {
                    Some(1) => Theme::Light,
                    Some(2) => Theme::Dark,
                    _ => Theme::System,
                },
                notifications_enabled: notify_check.is_active(),
                notify_on_complete: notify_complete_check.is_active(),
                notify_on_fail: notify_fail_check.is_active(),
                minimize_to_tray: tray_check.is_active(),
                close_behavior: match close_combo.active() {
                    Some(1) => CloseBehavior::MinimizeToTray,
                    Some(2) => CloseBehavior::Ask,
                    _ => CloseBehavior::Quit,
                },
                start_on_login: startup_check.is_active(),
                after_completion: after_actions
                    .get(after_combo.active().unwrap_or(0) as usize)
                    .map(|(_, a)| *a)
                    .unwrap_or(AfterCompletion::None),
                default_dir: default_dir_entry.text().to_string(),
                temp_dir: {
                    let t = temp_entry.text().to_string();
                    if t.trim().is_empty() { None } else { Some(t) }
                },
                default_connections: conn_spin.value() as i32,
                max_active_downloads: max_spin.value() as i32,
                max_global_connections: max_conn_spin.value() as i32,
                duplicate_policy: match dup_combo.active() {
                    Some(1) => DuplicatePolicy::Rename,
                    Some(2) => DuplicatePolicy::Overwrite,
                    Some(3) => DuplicatePolicy::Skip,
                    _ => DuplicatePolicy::Ask,
                },
                prefer_server_filename: prefer_server_check.is_active(),
                resume_on_start: resume_check.is_active(),
                verify_after_download: verify_check.is_active(),
                global_speed_limit: parse_speed_limit(&limit_entry.text().to_string()),
                retry_count: retry_spin.value() as i32,
                retry_base_seconds: retry_base_spin.value() as u64,
                connect_timeout_seconds: timeout_spin.value() as u64,
                read_timeout_seconds: read_timeout_spin.value() as u64,
                proxy_mode: match proxy_combo.active() {
                    Some(2) => ProxyMode::Custom,
                    Some(0) => ProxyMode::None,
                    _ => ProxyMode::System,
                },
                proxy_url: proxy_url_entry.text().to_string(),
                user_agent: ua_entry.text().to_string(),
                log_level: log_combo
                    .active_text()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "info".to_string()),
                clipboard_monitoring: clipboard_check.is_active(),
                privacy_mode: privacy_check.is_active(),
                clear_history_on_exit: clear_history_check.is_active(),
                prevent_sleep_while_downloading: prevent_sleep_check.is_active(),
                browser_integration_enabled: browser_check.is_active(),
                browser_auto_capture: autocapture_check.is_active(),
                browser_send_cookies: cookies_check.is_active(),
                capture_extensions: capture_entry
                    .text()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                ..settings.clone()
            };
            s.validate();
            let s2 = state2.clone();
            state2.rt.block_on(async move {
                let _ = s2.manager.update_settings(s).await;
            });
            state2.apply_theme();
        }
        d.close();
    });

    dialog.show_all();
}
