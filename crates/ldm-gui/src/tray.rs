//! System tray (libappindicator3) via a small raw FFI binding.
//!
//! The `appindicator` crate needs `appindicator3-0.1.pc`, which Ubuntu only
//! ships as `ayatana-appindicator3-0.1.pc`; linking directly against the
//! `libappindicator3.so` shim is simpler and dependency-free.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::rc::Rc;

use gtk::prelude::*;
use ldm_engine::{DownloadFilter, SortField, SortOrder};

use crate::app::App;

#[repr(C)]
struct AppIndicator {
    _private: [u8; 0],
}

const APP_INDICATOR_CATEGORY_APPLICATION_STATUS: c_int = 0;
const APP_INDICATOR_STATUS_ACTIVE: c_int = 1;

#[link(name = "appindicator3")]
extern "C" {
    fn app_indicator_new(
        id: *const c_char,
        icon_name: *const c_char,
        category: c_int,
    ) -> *mut AppIndicator;
    fn app_indicator_set_status(indicator: *mut AppIndicator, status: c_int);
    fn app_indicator_set_icon_theme_path(indicator: *mut AppIndicator, path: *const c_char);
    fn app_indicator_set_icon_full(
        indicator: *mut AppIndicator,
        name: *const c_char,
        desc: *const c_char,
    );
    fn app_indicator_set_title(indicator: *mut AppIndicator, title: *const c_char);
    fn app_indicator_set_menu(indicator: *mut AppIndicator, menu: *mut gtk::ffi::GtkMenu);
}

pub struct Tray {
    indicator: *mut AppIndicator,
    _menu: gtk::Menu,
}

// The pointer is only used from the main thread; safe in practice.
unsafe impl Send for Tray {}
unsafe impl Sync for Tray {}

impl Drop for Tray {
    fn drop(&mut self) {
        // The indicator is owned by the process for its whole lifetime; the
        // GTK main loop tears down when the process exits, so we deliberately
        // leak the pointer here rather than risk double-unref.
        let _ = self.indicator;
    }
}

impl Tray {
    pub fn new(state: &Rc<App>) -> Option<Self> {
        let id = cstr("ldm-tray");
        let icon = cstr("ldm");
        let indicator = unsafe { app_indicator_new(id.as_ptr(), icon.as_ptr(), APP_INDICATOR_CATEGORY_APPLICATION_STATUS) };
        if indicator.is_null() {
            return None;
        }
        unsafe {
            app_indicator_set_status(indicator, APP_INDICATOR_STATUS_ACTIVE);
            app_indicator_set_title(indicator, cstr("LDM").as_ptr());
        }

        let menu = build_menu(state);

        unsafe {
            use glib::translate::ToGlibPtr;
            app_indicator_set_menu(indicator, menu.to_glib_none().0);
        }

        Some(Tray {
            indicator,
            _menu: menu,
        })
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

fn build_menu(state: &Rc<App>) -> gtk::Menu {
    let menu = gtk::Menu::new();

    let open_item = gtk::MenuItem::with_label("Open LDM");
    open_item.connect_activate(glib::clone!(@strong state => move |_| {
        if let Some(w) = state.window.borrow().as_ref() {
            w.present();
        }
    }));
    menu.append(&open_item);

    let pause_item = gtk::MenuItem::with_label("Pause All");
    pause_item.connect_activate(glib::clone!(@strong state => move |_| {
        let s = state.clone();
        state.rt.block_on(async move {
            let records = s.manager
                .list_downloads(DownloadFilter::Active, "", (SortField::DateAdded, SortOrder::Asc), None)
                .await;
            if let Ok(rs) = records {
                for r in rs {
                    let _ = s.manager.pause(r.id).await;
                }
            }
        });
    }));
    menu.append(&pause_item);

    let resume_item = gtk::MenuItem::with_label("Resume All");
    resume_item.connect_activate(glib::clone!(@strong state => move |_| {
        let s = state.clone();
        state.rt.block_on(async move {
            let records = s.manager
                .list_downloads(DownloadFilter::Paused, "", (SortField::DateAdded, SortOrder::Asc), None)
                .await;
            if let Ok(rs) = records {
                for r in rs {
                    let _ = s.manager.resume(r.id).await;
                }
            }
        });
    }));
    menu.append(&resume_item);

    let settings_item = gtk::MenuItem::with_label("Settings…");
    settings_item.connect_activate(glib::clone!(@strong state => move |_| {
        crate::dialogs::show_settings(&state);
    }));
    menu.append(&settings_item);

    menu.append(&gtk::SeparatorMenuItem::new());

    let quit_item = gtk::MenuItem::with_label("Quit");
    quit_item.connect_activate(glib::clone!(@strong state => move |_| {
        let s = state.clone();
        state.rt.block_on(async move {
            s.manager.shutdown().await;
        });
        state.gtk_app.quit();
    }));
    menu.append(&quit_item);

    menu.show_all();
    menu
}
