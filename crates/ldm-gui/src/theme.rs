//! Theme: a small GTK CSS layer plus light/dark application.

use gtk::prelude::*;
use ldm_engine::settings::Theme;

/// Base stylesheet — colors come from theme CSS variables so both light and
/// dark variants stay readable.
const BASE_CSS: &str = r#"
.sidebar-section {
    font-size: 11px;
    font-weight: bold;
    opacity: 0.6;
}
.prop-label {
    font-weight: bold;
}
treeview.view {
    font-size: 13px;
}
treeview.view row {
    padding: 6px 4px;
}
progressbar trough {
    min-height: 10px;
    border-radius: 5px;
}
progressbar progress {
    border-radius: 5px;
    min-height: 10px;
}
/* Sidebar: slightly distinct surface, rounded selection with the accent. */
list {
    background: transparent;
}
list row {
    border-radius: 6px;
    margin: 1px 6px;
    padding: 0;
}
list row:hover {
    background: alpha(currentColor, 0.06);
}
list row:selected {
    background: alpha(#3b82f6, 0.22);
    color: inherit;
}
headerbar {
    min-height: 42px;
}
"#;

const LIGHT_CSS: &str = r#"
progressbar progress { background: #2563eb; }
treeview.view:selected { background: #dbeafe; color: #1c2230; }
list row:selected { background: alpha(#2563eb, 0.16); }
"#;

const DARK_CSS: &str = r#"
progressbar progress { background: #3b82f6; }
treeview.view:selected { background: #1e3a5f; color: #e6e9ef; }
list row:selected { background: alpha(#3b82f6, 0.24); }
"#;

pub fn install_default_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(BASE_CSS.as_bytes()).unwrap();
    if let Some(screen) = gtk::gdk::Screen::default() {
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

pub fn apply_theme(theme: Theme) {
    if let Some(settings) = gtk::Settings::default() {
        let dark = match theme {
            Theme::System => settings
                .property::<bool>("gtk-application-prefer-dark-theme"),
            Theme::Light => false,
            Theme::Dark => true,
        };
        let _ = settings.set_property("gtk-application-prefer-dark-theme", dark);
    }

    let provider = gtk::CssProvider::new();
    let css = match theme {
        Theme::Dark => DARK_CSS,
        _ => LIGHT_CSS,
    };
    provider.load_from_data(css.as_bytes()).unwrap();
    if let Some(screen) = gtk::gdk::Screen::default() {
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
