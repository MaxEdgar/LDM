//! Desktop notifications via the D-Bus notification service.

use ldm_engine::Settings;

pub fn on_completed(settings: &Settings) {
    if !settings.notifications_enabled || !settings.notify_on_complete {
        return;
    }
    let _ = notify_rust::Notification::new()
        .summary("Download completed")
        .body("A download has finished.")
        .appname("LDM")
        .show();
}

pub fn on_failed(settings: &Settings, error: &str) {
    if !settings.notifications_enabled || !settings.notify_on_fail {
        return;
    }
    let _ = notify_rust::Notification::new()
        .summary("Download failed")
        .body(error)
        .appname("LDM")
        .show();
}
