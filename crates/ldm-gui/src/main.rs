//! LDM native GTK3 desktop UI.
//!
//! Unlike a webview shell, this is a real native application: GTK3 widgets
//! rendered by the system toolkit, the download engine linked in-process.
//! The engine runs inside a Tokio runtime on a worker thread; events flow to
//! the GTK main loop over a `glib::MainContext` channel, and UI actions call
//! back into the engine with `Handle::block_on` / spawned tasks.

mod app;
mod dialogs;
mod notify;
mod theme;
mod tray;
mod ui;
mod window;

use std::sync::Arc;

use glib::MainContext;
use gtk::prelude::*;
use ldm_engine::{DownloadManager, ManagerConfig};

use app::App;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ldm_engine=info")),
        )
        .init();

    // Build the engine inside a Tokio runtime on this thread, then hand the
    // runtime to a worker thread that keeps it alive for the process lifetime.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("failed to start runtime: {e}");
            std::process::exit(1);
        }
    };
    let handle = rt.handle().clone();
    let manager = match rt.block_on(DownloadManager::new(ManagerConfig::default())) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("failed to initialize engine: {e}");
            std::process::exit(1);
        }
    };
    std::thread::Builder::new()
        .name("ldm-engine".into())
        .spawn(move || {
            // Park the runtime: spawned engine tasks keep running until the
            // process exits.
            let _ = rt.block_on(std::future::pending::<()>());
        })
        .expect("spawn engine thread");

    run_gui(manager, handle);
}

fn run_gui(manager: Arc<DownloadManager>, rt: tokio::runtime::Handle) {
    let app = gtk::Application::new(Some("org.ldm.app"), Default::default());

    app.connect_startup(move |_app| {
        theme::install_default_css();
    });

    let manager_shutdown = manager.clone();
    let rt_shutdown = rt.clone();
    app.connect_shutdown(move |_| {
        let mgr = manager_shutdown.clone();
        let rt = rt_shutdown.clone();
        rt.block_on(async move {
            mgr.shutdown().await;
        });
    });

    app.connect_activate(move |app| {
        // Event bridge: engine broadcast -> glib main-context channel.
        let (ui_tx, ui_rx) = MainContext::channel::<app::UiMsg>(glib::Priority::DEFAULT);
        let mut events = manager.subscribe();
        let tx = ui_tx.clone();
        rt.spawn(async move {
            while let Ok(ev) = events.recv().await {
                if tx.send(app::UiMsg::Engine(ev)).is_err() {
                    break;
                }
            }
        });

        let state = App::new(manager.clone(), rt.clone(), ui_tx, app);
        let window = window::build_main_window(&state);
        *state.window.borrow_mut() = Some(window.clone());
        window.show_all();

        // Route engine/UI messages onto the GTK main loop.
        let state_ui = state.clone();
        ui_rx.attach(None, move |msg| {
            state_ui.on_msg(msg);
            glib::ControlFlow::Continue
        });

        state.setup_tray();
        state.apply_theme();
        state.refresh().ok();
    });

    app.run();
}
