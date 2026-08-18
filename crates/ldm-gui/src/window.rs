//! Main window: header bar, sidebar (views + categories + queues), download
//! list with progress, status bar, context menu, keyboard accelerators.

use std::rc::Rc;

use gtk::prelude::*;
use ldm_engine::AddDownloadOptions;

use crate::app::{
    App, View, COL_CATEGORY, COL_ETA, COL_ID, COL_NAME, COL_PROGRESS, COL_SIZE, COL_SPEED,
    COL_STATUS, COL_STATUS_COLOR,
};

pub fn build_main_window(state: &Rc<App>) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::new(&state.gtk_app);
    window.set_title("LDM — Linux Download Manager");
    window.set_default_size(1100, 720);
    window.set_size_request(760, 480);
    window.set_position(gtk::WindowPosition::Center);
    set_window_icon(&window);

    // Header bar.
    let header = gtk::HeaderBar::new();
    header.set_show_close_button(true);
    header.set_title(Some("LDM"));
    header.set_subtitle(Some("Linux Download Manager"));

    let add_btn = gtk::Button::with_label("Add Download");
    add_btn.set_tooltip_text(Some("Add a new download (Ctrl+N)"));
    add_btn.connect_clicked(glib::clone!(@strong state => move |_| {
        crate::dialogs::show_add_dialog(&state);
    }));
    header.pack_start(&add_btn);

    // Search in the header bar.
    state.search.set_placeholder_text(Some("Search downloads…"));
    state.search.set_width_chars(28);
    state.search.connect_search_changed(glib::clone!(@strong state => move |e| {
        let text = e.text().to_string();
        state.set_search(&text);
    }));
    header.pack_end(&state.search);

    // Pause all / resume all.
    let pause_all = gtk::Button::with_label("Pause All");
    pause_all.set_tooltip_text(Some("Pause all active downloads"));
    pause_all.connect_clicked(glib::clone!(@strong state => move |_| {
        state.rt.block_on(async {
            let records = state.manager
                .list_downloads(ldm_engine::DownloadFilter::Active, "", (ldm_engine::SortField::DateAdded, ldm_engine::SortOrder::Asc), None)
                .await;
            if let Ok(rs) = records {
                for r in rs {
                    let _ = state.manager.pause(r.id).await;
                }
            }
        });
    }));
    header.pack_end(&pause_all);

    let resume_all = gtk::Button::with_label("Resume All");
    resume_all.set_tooltip_text(Some("Resume all paused downloads"));
    resume_all.connect_clicked(glib::clone!(@strong state => move |_| {
        state.rt.block_on(async {
            let records = state.manager
                .list_downloads(ldm_engine::DownloadFilter::Paused, "", (ldm_engine::SortField::DateAdded, ldm_engine::SortOrder::Asc), None)
                .await;
            if let Ok(rs) = records {
                for r in rs {
                    let _ = state.manager.resume(r.id).await;
                }
            }
        });
    }));
    header.pack_end(&resume_all);

    // --- Sidebar ---
    build_sidebar(state);

    // --- Download list ---
    build_list(state);

    let scrolled = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.add(&state.tree);

    // --- Status bar ---
    let statusbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    statusbar.set_margin_start(12);
    statusbar.set_margin_end(12);
    statusbar.set_margin_top(4);
    statusbar.set_margin_bottom(4);
    state.status_label.set_xalign(0.0);
    statusbar.pack_start(&state.status_label, true, true, 0);

    // Layout: sidebar | main.
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    let sidebar_scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    sidebar_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sidebar_scroll.add(&state.sidebar);
    sidebar_scroll.set_size_request(210, -1);
    paned.pack1(&sidebar_scroll, false, false);
    paned.pack2(&scrolled, true, true);

    // Real-time speed graph at the bottom of the sidebar.
    let speed_graph = build_speed_graph(state);
    *state.speed_graph.borrow_mut() = Some(speed_graph.clone());
    let graph_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let graph_title = gtk::Label::new(Some("Transfer rate"));
    graph_title.set_xalign(0.0);
    graph_title.set_margin_start(12);
    graph_title.set_margin_top(6);
    graph_title.style_context().add_class("sidebar-section");
    graph_holder.pack_start(&graph_title, false, false, 0);
    graph_holder.pack_start(&speed_graph, true, true, 0);
    let graph_scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    graph_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    graph_scroll.add(&graph_holder);
    graph_scroll.set_size_request(-1, 110);
    let sidebar_vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_vbox.pack_start(&sidebar_scroll, true, true, 0);
    sidebar_vbox.pack_end(&graph_scroll, false, false, 0);
    paned.pack1(&sidebar_vbox, false, false);

    // Connection-bars panel (per-segment progress of the selected download).
    let conn_area = build_conn_bars(state);
    *state.conn_bars.borrow_mut() = Some(conn_area.clone());
    let conn_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let conn_title = gtk::Label::new(Some("Connections"));
    conn_title.set_xalign(0.0);
    conn_title.set_margin_start(12);
    conn_title.set_margin_top(4);
    conn_title.style_context().add_class("sidebar-section");
    conn_holder.pack_start(&conn_title, false, false, 0);
    conn_holder.pack_start(&conn_area, true, true, 0);
    *state.conn_panel.borrow_mut() = Some(conn_holder.clone());
    conn_holder.hide(); // only visible while a download row is selected

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.pack_start(&paned, true, true, 0);
    vbox.pack_end(&conn_holder, false, false, 0);
    vbox.pack_end(&statusbar, false, false, 0);

    window.set_titlebar(Some(&header));
    window.add(&vbox);

    // Context menu on right-click.
    state.tree.connect_button_press_event(glib::clone!(@strong state => move |tree, ev| {
        if ev.button() == 3 {
            let (x, y) = ev.position();
            if let Some((path, _, _, _)) = tree.path_at_pos(x as i32, y as i32) {
                if let Some(path) = path {
                    if let Some(iter) = tree.model().unwrap().iter(&path) {
                        tree.selection().select_iter(&iter);
                    }
                    show_context_menu(&state, &path);
                    return glib::Propagation::Stop;
                }
            }
        }
        glib::Propagation::Proceed
    }));

    // Double-click: open properties.
    state.tree.connect_row_activated(glib::clone!(@strong state => move |_tree, path, _col| {
        if let Some(iter) = state.store.iter(path) {
            if let Ok(id) = state.store.value(&iter, COL_ID).get::<i64>() {
                crate::dialogs::show_properties(&state, id);
            }
        }
    }));

    // Accelerators.
    let accel = gtk::AccelGroup::new();
    window.add_accel_group(&accel);
    add_btn.add_accelerator("activate", &accel, *gtk::gdk::keys::Key::from_name("n"), gtk::gdk::ModifierType::CONTROL_MASK, gtk::AccelFlags::VISIBLE);
    state.search.add_accelerator("grab-focus", &accel, *gtk::gdk::keys::Key::from_name("f"), gtk::gdk::ModifierType::CONTROL_MASK, gtk::AccelFlags::VISIBLE);

    // Delete key removes the selected row.
    state.tree.connect_key_press_event(glib::clone!(@strong state => move |_tree, ev| {
        if ev.keyval() == gtk::gdk::keys::Key::from_name("Delete") {
            if let Some(id) = selected_id(&state) {
                crate::dialogs::confirm_remove(&state, id);
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }));

    // Close behavior: hide to tray instead of quitting when configured.
    window.connect_delete_event(glib::clone!(@strong state => move |win, _| {
        let settings = state.rt.block_on(state.manager.get_settings());
        match settings.close_behavior {
            ldm_engine::settings::CloseBehavior::Quit => glib::Propagation::Proceed,
            _ => {
                win.hide();
                glib::Propagation::Stop
            }
        }
    }));

    window
}

fn add_sidebar_row(
    state: &Rc<App>,
    glyph: &str,
    glyph_color: Option<&str>,
    label: &str,
    view: View,
    indent: bool,
) {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    hbox.set_margin_start(if indent { 30 } else { 14 });
    hbox.set_margin_end(14);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);
    // Fixed-width icon cell so every row label starts at the same x position
    // regardless of the glyph's natural width (glyphs are right-aligned).
    let icon = gtk::Label::new(Some(glyph));
    icon.set_width_chars(2);
    icon.set_xalign(1.0);
    if let Some(color) = glyph_color {
        icon.set_markup(&format!("<span color='{color}'>{glyph}</span>"));
    }
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hbox.pack_start(&icon, false, false, 0);
    hbox.pack_start(&l, true, true, 0);
    row.add(&hbox);
    state.sidebar.add(&row);
    state.sidebar_views.borrow_mut().push(view);
}

/// Category glyphs (distinct colors, readable in both themes).
fn category_glyph(name: &str) -> (&'static str, &'static str) {
    match name {
        "Documents" => ("●", "#3b82f6"),
        "Compressed" => ("●", "#f59e0b"),
        "Programs" => ("●", "#22d3ee"),
        "Videos" => ("●", "#ef4444"),
        "Music" => ("●", "#a855f7"),
        "Images" => ("●", "#22c55e"),
        _ => ("●", "#8a94a3"),
    }
}

/// Populate the sidebar (sections, library views, categories, queues).
fn populate_sidebar(state: &Rc<App>, queue_names: &[String]) {
    let add_section = |title: &str| {
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.set_margin_start(12);
        label.set_margin_top(10);
        label.set_margin_bottom(2);
        label.style_context().add_class("sidebar-section");
        let row = gtk::ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(false);
        row.add(&label);
        state.sidebar.add(&row);
    };

    add_section("LIBRARY");
    add_sidebar_row(state, "⤓", None, "All Downloads", View::All, false);
    add_sidebar_row(state, "▶", None, "Active", View::Active, false);
    add_sidebar_row(state, "❚❚", None, "Paused", View::Paused, false);
    add_sidebar_row(state, "≡", None, "Queued", View::Queued, false);
    add_sidebar_row(state, "✓", None, "Completed", View::Completed, false);
    add_sidebar_row(state, "✕", None, "Failed", View::Failed, false);
    add_sidebar_row(state, "⏱", None, "Scheduled", View::Scheduled, false);

    add_section("CATEGORIES");
    for name in ldm_engine::categories::BUILTIN_CATEGORIES.iter().map(|c| c.0) {
        let (glyph, color) = category_glyph(name);
        add_sidebar_row(
            state,
            glyph,
            Some(color),
            name,
            View::Category(name.to_string()),
            true,
        );
    }

    if !queue_names.is_empty() {
        add_section("QUEUES");
        for name in queue_names {
            add_sidebar_row(state, "▤", None, name, View::Queue(name.clone()), true);
        }
    }
    state.sidebar.show_all();
}

fn build_sidebar(state: &Rc<App>) {
    let queues = state.rt.block_on(state.manager.list_queues()).unwrap_or_default();
    let names: Vec<String> = queues.iter().map(|q| q.name.clone()).collect();
    *state.queue_names.borrow_mut() = names.clone();
    populate_sidebar(state, &names);

    state.sidebar.connect_row_selected(glib::clone!(@strong state => move |_, row| {
        if let Some(row) = row {
            let idx = row.index();
            if idx >= 0 {
                if let Some(v) = state.sidebar_views.borrow().get(idx as usize) {
                    state.set_view(v.clone());
                }
            }
        }
    }));
}

/// Rebuild the queue rows of the sidebar (called on QueueChanged and startup).
pub fn rebuild_queues(state: &Rc<App>) {
    let queues = state.rt.block_on(state.manager.list_queues()).unwrap_or_default();
    let names: Vec<String> = queues.iter().map(|q| q.name.clone()).collect();
    *state.queue_names.borrow_mut() = names.clone();

    // Rebuild the entire sidebar to keep row indices aligned.
    while let Some(w) = state.sidebar.row_at_index(0) {
        state.sidebar.remove(&w);
    }
    state.sidebar_views.borrow_mut().clear();
    populate_sidebar(state, &names);
}

fn build_list(state: &Rc<App>) {
    state.tree.set_headers_visible(true);
    state.tree.set_rubber_banding(true);
    state.tree.selection().set_mode(gtk::SelectionMode::Single);

    let add_col = |tree: &gtk::TreeView, title: &str, col: i32, expand: bool| {
        let renderer = gtk::CellRendererText::new();
        let column = gtk::TreeViewColumn::new();
        column.set_title(title);
        gtk::prelude::CellLayoutExt::pack_start(&column, &renderer, true);
        gtk::prelude::CellLayoutExt::add_attribute(&column, &renderer, "text", col);
        column.set_resizable(true);
        column.set_expand(expand);
        tree.append_column(&column);
    };

    add_col(&state.tree, "Name", COL_NAME, true);
    add_col(&state.tree, "Size", COL_SIZE, false);
    add_col(&state.tree, "Category", COL_CATEGORY, false);

    // Progress column uses a progress bar renderer.
    let pbar = gtk::CellRendererProgress::new();
    let pcol = gtk::TreeViewColumn::new();
    pcol.set_title("Progress");
    gtk::prelude::CellLayoutExt::pack_start(&pcol, &pbar, true);
    gtk::prelude::CellLayoutExt::add_attribute(&pcol, &pbar, "value", COL_PROGRESS);
    pcol.set_min_width(140);
    state.tree.append_column(&pcol);

    add_col(&state.tree, "Speed", COL_SPEED, false);
    add_col(&state.tree, "ETA", COL_ETA, false);

    // Status column with per-status color.
    let status_renderer = gtk::CellRendererText::new();
    let status_col = gtk::TreeViewColumn::new();
    status_col.set_title("Status");
    gtk::prelude::CellLayoutExt::pack_start(&status_col, &status_renderer, true);
    gtk::prelude::CellLayoutExt::add_attribute(&status_col, &status_renderer, "text", COL_STATUS);
    gtk::prelude::CellLayoutExt::add_attribute(&status_col, &status_renderer, "foreground", COL_STATUS_COLOR);
    status_col.set_resizable(true);
    state.tree.append_column(&status_col);

    // Show the connection panel when a download is selected, hide it otherwise.
    state.tree.selection().connect_changed(glib::clone!(@strong state => move |_| {
        if let Some(panel) = state.conn_panel.borrow().as_ref() {
            if state.selected_id().is_some() {
                panel.show();
            } else {
                panel.hide();
            }
        }
        state.redraw_conn_bars();
    }));
}

pub fn selected_id(state: &Rc<App>) -> Option<i64> {
    let sel = state.tree.selection();
    match sel.selected() {
        Some((_, iter)) => state.store.value(&iter, COL_ID).get::<i64>().ok(),
        None => None,
    }
}

fn show_context_menu(state: &Rc<App>, path: &gtk::TreePath) {
    let Some(id) = state
        .store
        .value(state.store.iter(path).as_ref().unwrap(), COL_ID)
        .get::<i64>()
        .ok()
    else {
        return;
    };
    let Some(record) = state.records.borrow().get(&id).cloned() else {
        return;
    };
    let menu = gtk::Menu::new();

    let active = record.status.is_active();
    let completed = record.status == ldm_engine::DownloadStatus::Completed;

    // Each menu item owns clones of the engine handle and runtime handle, so
    // GTK can invoke them any number of times from the main loop.
    let state_ref = state.clone();

    let add_item = |menu: &gtk::Menu, label: &str, action: Box<dyn Fn() + 'static>| {
        let item = gtk::MenuItem::with_label(label);
        item.connect_activate(move |_| action());
        menu.append(&item);
    };

    let mgr = state_ref.manager.clone();
    let rt = state_ref.rt.clone();

    // Build a menu-item action that awaits an async manager call for this id.
    let act = |f: fn(
        std::sync::Arc<ldm_engine::DownloadManager>,
        i64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = ldm_engine::error::Result<()>> + Send>,
    >| {
        let mgr = mgr.clone();
        let rt = rt.clone();
        Box::new(move || {
            let mgr = mgr.clone();
            rt.block_on(async move {
                let _ = f(mgr, id).await;
            });
        }) as Box<dyn Fn() + 'static>
    };

    fn resume_f(
        m: std::sync::Arc<ldm_engine::DownloadManager>,
        id: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ldm_engine::error::Result<()>> + Send>> {
        Box::pin(async move { m.resume(id).await })
    }
    fn pause_f(
        m: std::sync::Arc<ldm_engine::DownloadManager>,
        id: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ldm_engine::error::Result<()>> + Send>> {
        Box::pin(async move { m.pause(id).await })
    }
    fn retry_f(
        m: std::sync::Arc<ldm_engine::DownloadManager>,
        id: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ldm_engine::error::Result<()>> + Send>> {
        Box::pin(async move { m.retry(id).await })
    }
    fn cancel_f(
        m: std::sync::Arc<ldm_engine::DownloadManager>,
        id: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ldm_engine::error::Result<()>> + Send>> {
        Box::pin(async move { m.cancel(id, false).await })
    }

    add_item(&menu, "Start", act(resume_f));
    add_item(&menu, "Pause", act(pause_f));
    if !active {
        add_item(&menu, "Resume", act(resume_f));
    }
    add_item(&menu, "Retry from scratch", act(retry_f));
    add_item(&menu, "Cancel", act(cancel_f));
    menu.append(&gtk::SeparatorMenuItem::new());
    add_item(&menu, "Copy URL", {
        let state = state_ref.clone();
        Box::new(move || {
            let mgr = state.manager.clone();
            let rt = state.rt.clone();
            if let Ok(Some(r)) = rt.block_on(mgr.get_download(id)) {
                state.copy_to_clipboard(&r.url);
            }
        }) as Box<dyn Fn() + 'static>
    });
    add_item(&menu, "Copy Filename", {
        let state = state_ref.clone();
        Box::new(move || {
            let mgr = state.manager.clone();
            let rt = state.rt.clone();
            if let Ok(Some(r)) = rt.block_on(mgr.get_download(id)) {
                state.copy_to_clipboard(&r.filename);
            }
        }) as Box<dyn Fn() + 'static>
    });
    if completed {
        add_item(&menu, "Open File", {
            let state = state_ref.clone();
            Box::new(move || {
                let mgr = state.manager.clone();
                let rt = state.rt.clone();
                if let Ok(Some(r)) = rt.block_on(mgr.get_download(id)) {
                    crate::dialogs::open_record(&r);
                }
            }) as Box<dyn Fn() + 'static>
        });
        add_item(&menu, "Open Folder", {
            let state = state_ref.clone();
            Box::new(move || {
                let mgr = state.manager.clone();
                let rt = state.rt.clone();
                if let Ok(Some(r)) = rt.block_on(mgr.get_download(id)) {
                    crate::dialogs::open_record_folder(&r);
                }
            }) as Box<dyn Fn() + 'static>
        });
    }
    menu.append(&gtk::SeparatorMenuItem::new());
    add_item(&menu, "Properties…", {
        let state = state_ref.clone();
        Box::new(move || crate::dialogs::show_properties(&state, id))
    });
    add_item(&menu, "Remove from list", {
        let state = state_ref.clone();
        Box::new(move || crate::dialogs::confirm_remove(&state, id))
    });
    add_item(&menu, "Delete file…", {
        let state = state_ref.clone();
        Box::new(move || crate::dialogs::confirm_remove_file(&state, id))
    });

    menu.show_all();
    menu.popup_easy(3, gtk::current_event_time());
}

impl App {
    pub fn copy_to_clipboard(&self, text: &str) {
        if let Some(clip) = gtk::Clipboard::default(&gtk::gdk::Display::default().unwrap()) {
            clip.set_text(text);
        }
    }
}

/// Best-effort: set the window/taskbar icon from the installed or bundled
/// icon file so the app never shows a generic placeholder.
fn set_window_icon(window: &gtk::ApplicationWindow) {
    let candidates = [
        std::path::PathBuf::from("/usr/share/icons/hicolor/128x128/apps/ldm.png"),
        std::path::PathBuf::from("/usr/share/pixmaps/ldm.png"),
    ];
    let mut path = candidates.iter().find(|p| p.exists()).cloned();
    if path.is_none() {
        // Bundled icon next to the binary (dev builds): assets/icon.png.
        if let Ok(exe) = std::env::current_exe() {
            let p = exe.parent().map(|d| d.join("../../assets/icon.png"));
            if let Some(p) = p {
                if p.exists() {
                    path = Some(p);
                }
            }
        }
    }
    if let Some(p) = path {
        if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file(p) {
            window.set_icon(Some(&pixbuf));
        }
    }
}

/// Helper to launch an add-download flow from outside dialogs.
pub fn queue_add(state: &Rc<App>, opts: AddDownloadOptions) {
    let mgr = state.manager.clone();
    let rt = state.rt.clone();
    let tx = state.ui_tx.clone();
    rt.spawn(async move {
        let res = mgr.add_download(opts, None).await.map_err(|e| e.to_string());
        let _ = tx.send(crate::app::UiMsg::AddResult(res));
    });
}

// ------------------------------------------------------------------
// Speed graph + connection bars (custom-drawn, cheap: only redrawn when
// progress events arrive, ~5 Hz).
// ------------------------------------------------------------------

/// Real-time transfer-rate graph (last ~60 samples).
fn build_speed_graph(state: &Rc<App>) -> gtk::DrawingArea {
    let da = gtk::DrawingArea::new();
    da.set_size_request(-1, 78);
    da.set_margin_start(8);
    da.set_margin_end(8);
    da.set_margin_top(2);
    da.set_margin_bottom(6);
    let state2 = state.clone();
    da.connect_draw(move |da, cr| {
        let alloc = da.allocation();
        let w = alloc.width().max(10) as f64;
        let h = alloc.height().max(10) as f64;
        cr.set_source_rgb(0.08, 0.09, 0.12);
        crate::ui::rounded_rect(cr, 0.0, 0.0, w, h, 6.0);
        let _ = cr.fill();

        let samples: Vec<f64> = state2
            .speed_history
            .borrow()
            .iter()
            .map(|s| *s as f64)
            .collect();
        if samples.len() < 2 {
            cr.set_source_rgb(0.5, 0.52, 0.56);
            let _ = cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
            cr.set_font_size(10.0);
            cr.move_to(10.0, h / 2.0 + 3.0);
            let _ = cr.show_text("no active transfers");
            return glib::Propagation::Proceed;
        }

        let max = samples.iter().cloned().fold(1.0f64, f64::max).max(1.0);
        let n = samples.len() as f64;
        let step = w / n;
        let plot_h = h - 16.0;
        let baseline = h - 14.0;

        // Area fill (accent blue).
        cr.set_source_rgba(0.23, 0.51, 0.96, 0.55);
        cr.move_to(0.0, baseline);
        for (i, s) in samples.iter().enumerate() {
            let x = i as f64 * step + step * 0.5;
            let y = baseline - (s / max) * (plot_h - 4.0);
            cr.line_to(x, y);
        }
        cr.line_to(w, baseline);
        cr.close_path();
        let _ = cr.fill();

        // Line on top.
        cr.set_source_rgb(0.35, 0.6, 1.0);
        cr.set_line_width(1.5);
        for (i, s) in samples.iter().enumerate() {
            let x = i as f64 * step + step * 0.5;
            let y = baseline - (s / max) * (plot_h - 4.0);
            if i == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
        }
        let _ = cr.stroke();

        // Current rate label.
        if let Some(&last) = samples.last() {
            let txt = format!("{}/s", crate::app::format_bytes(last as i64));
            cr.set_source_rgb(0.85, 0.87, 0.9);
            let _ = cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
            cr.set_font_size(10.0);
            cr.move_to(8.0, h - 4.0);
            let _ = cr.show_text(&txt);
        }
        glib::Propagation::Proceed
    });
    da
}

/// Per-connection progress bars for the selected download (IDM-style).
///
/// The panel height is recomputed by `App::update_progress_row` from the live
/// segment count, so bars keep a comfortable size whether the download uses 1
/// or 32 connections.
fn build_conn_bars(state: &Rc<App>) -> gtk::DrawingArea {
    let da = gtk::DrawingArea::new();
    da.set_size_request(-1, 58);
    da.set_margin_start(12);
    da.set_margin_end(12);
    da.set_margin_bottom(4);
    let state2 = state.clone();
    da.connect_draw(move |da, cr| {
        let alloc = da.allocation();
        let w = alloc.width().max(20) as f64;
        let h = alloc.height().max(16) as f64;
        cr.set_source_rgb(0.07, 0.08, 0.1);
        crate::ui::rounded_rect(cr, 0.0, 0.0, w, h, 6.0);
        let _ = cr.fill();

        let id = state2.selected_id();
        let segs = match id {
            Some(id) => state2
                .seg_progress
                .borrow()
                .get(&id)
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        };

        // Overall progress of the selected download (used for the fallback bar
        // and for segments whose server range end is unknown).
        let overall = id.and_then(|i| {
            state2.records.borrow().get(&i).map(|r| {
                let total = r.total_bytes.unwrap_or(r.downloaded_bytes).max(1);
                (r.downloaded_bytes as f64 / total as f64).clamp(0.0, 1.0)
            })
        });

        // Fallback: single overall bar (paused / completed / single-stream).
        if segs.is_empty() {
            if let Some(frac) = overall {
                let bar_h = 20.0;
                let bar_x = 44.0;
                let bar_w = (w - bar_x - 56.0).max(20.0);
                let y = (h - bar_h) / 2.0;
                cr.set_source_rgb(0.16, 0.18, 0.23);
                crate::ui::rounded_rect(cr, bar_x, y, bar_w, bar_h, 5.0);
                let _ = cr.fill();
                if frac > 0.0 {
                    cr.set_source_rgb(0.23, 0.51, 0.96);
                    crate::ui::rounded_rect(cr, bar_x, y, (bar_w * frac).max(2.0), bar_h, 5.0);
                    let _ = cr.fill();
                }
                cr.set_source_rgb(0.85, 0.87, 0.9);
                let _ = cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
                cr.set_font_size(11.0);
                cr.move_to(12.0, y + bar_h / 2.0 + 3.5);
                let _ = cr.show_text("Overall");
                cr.set_source_rgb(0.55, 0.58, 0.63);
                let _ = cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
                cr.set_font_size(10.0);
                cr.move_to(bar_x + bar_w + 6.0, y + bar_h / 2.0 + 3.0);
                let _ = cr.show_text(&format!("{:.0}%", frac * 100.0));
                return glib::Propagation::Proceed;
            }
            cr.set_source_rgb(0.5, 0.52, 0.56);
            let _ = cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
            cr.set_font_size(10.0);
            cr.move_to(12.0, h / 2.0 + 3.0);
            let _ = cr.show_text("Select a download to see connection progress");
            return glib::Propagation::Proceed;
        }

        let n = segs.len();
        let cap = n.min(32) as f64;
        let label_w = 20.0;
        let pct_w = 34.0;
        let bar_x = label_w + 2.0;
        let bar_w = (w - bar_x - pct_w - 10.0).max(20.0);
        let gap = if n > 12 { 2.0 } else { 4.0 };
        let bar_h = ((h - 10.0 - gap * (cap - 1.0)) / cap).max(6.0);

        for (i, s) in segs.iter().take(n.min(32)).enumerate() {
            let y = 5.0 + i as f64 * (bar_h + gap);
            let frac = if let Some(end) = s.end {
                let total_len = (end - s.start + 1).max(1) as f64;
                ((s.downloaded as f64) / total_len).clamp(0.0, 1.0)
            } else {
                // Server reported no range end (size unknown): use the overall
                // progress rather than inventing a 100% fill.
                overall.unwrap_or(0.0)
            };

            // Track.
            cr.set_source_rgb(0.16, 0.18, 0.23);
            crate::ui::rounded_rect(cr, bar_x, y, bar_w, bar_h, 3.0);
            let _ = cr.fill();
            // Fill (blue while active, green when done).
            if frac >= 1.0 {
                cr.set_source_rgb(0.16, 0.62, 0.36);
            } else {
                cr.set_source_rgb(0.23, 0.51, 0.96);
            }
            let fill_w = (bar_w * frac).max(if frac > 0.0 { 2.0 } else { 0.0 });
            crate::ui::rounded_rect(cr, bar_x, y, fill_w, bar_h, 3.0);
            let _ = cr.fill();

            // Segment number + percent labels.
            cr.set_source_rgb(0.7, 0.73, 0.78);
            let _ = cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
            cr.set_font_size(9.0);
            cr.move_to(4.0, y + bar_h / 2.0 + 3.0);
            let _ = cr.show_text(&format!("{}", i + 1));
            cr.set_source_rgb(0.55, 0.58, 0.63);
            cr.set_font_size(9.0);
            cr.move_to(bar_x + bar_w + 6.0, y + bar_h / 2.0 + 3.0);
            let _ = cr.show_text(&format!("{:.0}%", frac * 100.0));
        }
        glib::Propagation::Proceed
    });
    da
}
