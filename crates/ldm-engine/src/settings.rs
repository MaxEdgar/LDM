//! Application settings. Stored as a versioned JSON document in the
//! `settings` table (one row, key `app`), loaded at startup and cached.

use serde::{Deserialize, Serialize};

pub const SETTINGS_VERSION: i32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // General
    pub theme: Theme,
    pub language: String,
    pub start_on_login: bool,
    pub minimize_to_tray: bool,
    pub close_behavior: CloseBehavior,
    pub notifications_enabled: bool,
    pub notify_on_complete: bool,
    pub notify_on_fail: bool,

    // Downloads
    pub default_dir: String,
    pub temp_dir: Option<String>,
    pub default_connections: i32,
    pub max_active_downloads: i32,
    pub max_global_connections: i32,
    pub duplicate_policy: DuplicatePolicy,
    pub prefer_server_filename: bool,
    pub resume_on_start: bool,
    pub verify_after_download: bool,
    pub ui_density: Density,

    // Network
    pub global_speed_limit: Option<i64>,
    pub connect_timeout_seconds: u64,
    pub read_timeout_seconds: u64,
    pub retry_count: i32,
    pub retry_base_seconds: u64,
    pub proxy_mode: ProxyMode,
    pub proxy_url: String,
    pub user_agent: String,

    // Browser integration
    pub browser_integration_enabled: bool,
    pub browser_auto_capture: bool,
    pub browser_send_cookies: bool,
    pub capture_extensions: Vec<String>,
    pub exclude_extensions: Vec<String>,
    pub exclude_hosts: Vec<String>,

    // Clipboard
    pub clipboard_monitoring: bool,

    // Privacy
    pub privacy_mode: bool,
    pub clear_history_on_exit: bool,
    pub redact_urls_in_history: bool,

    // Scheduler / power
    pub prevent_sleep_while_downloading: bool,
    /// What to do once every download in the queue has finished (IDM-style).
    pub after_completion: AfterCompletion,

    // Advanced
    pub log_level: String,
    pub ui_update_hz: u32,

    /// Internal; bumped when the settings schema changes.
    pub version: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    Quit,
    MinimizeToTray,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    Rename,
    Overwrite,
    Ask,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    None,
    System,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Comfortable,
    Compact,
}

/// Action to take once the download queue is fully finished (spec §14).
/// Never triggered without explicit configuration and a confirmation prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterCompletion {
    /// Do nothing (default).
    #[default]
    None,
    Shutdown,
    Restart,
    Suspend,
    Hibernate,
    Logout,
    /// Close the LDM application itself.
    QuitApp,
}

fn default_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
        .to_string_lossy()
        .to_string()
}

fn default_ua() -> String {
    format!(
        "LDM/{} (+https://github.com/ldm/ldm)",
        crate::ENGINE_VERSION
    )
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            language: "en".to_string(),
            start_on_login: false,
            minimize_to_tray: true,
            close_behavior: CloseBehavior::MinimizeToTray,
            notifications_enabled: true,
            notify_on_complete: true,
            notify_on_fail: true,
            default_dir: default_dir(),
            temp_dir: None,
            default_connections: 8,
            max_active_downloads: 3,
            max_global_connections: 128,
            duplicate_policy: DuplicatePolicy::Ask,
            prefer_server_filename: true,
            resume_on_start: true,
            verify_after_download: true,
            ui_density: Density::Comfortable,
            global_speed_limit: None,
            connect_timeout_seconds: 15,
            read_timeout_seconds: 60,
            retry_count: 5,
            retry_base_seconds: 2,
            proxy_mode: ProxyMode::System,
            proxy_url: String::new(),
            user_agent: default_ua(),
            browser_integration_enabled: false,
            browser_auto_capture: false,
            browser_send_cookies: false,
            capture_extensions: vec![
                ".iso".into(),
                ".zip".into(),
                ".rar".into(),
                ".7z".into(),
                ".exe".into(),
                ".dmg".into(),
                ".tar".into(),
                ".tar.gz".into(),
                ".gz".into(),
                ".xz".into(),
                ".bz2".into(),
                ".deb".into(),
                ".rpm".into(),
                ".apk".into(),
                ".mp4".into(),
                ".mkv".into(),
                ".mov".into(),
                ".avi".into(),
                ".mp3".into(),
                ".flac".into(),
            ],
            exclude_extensions: vec![".html".into(), ".htm".into(), ".php".into(), ".json".into()],
            exclude_hosts: vec![
                "accounts.google.com".into(),
                "login.microsoftonline.com".into(),
                "signin.aws.amazon.com".into(),
                "github.com/login".into(),
                "paypal.com".into(),
                "*.bank*".into(),
            ],
            clipboard_monitoring: false,
            privacy_mode: false,
            clear_history_on_exit: false,
            redact_urls_in_history: true,
            prevent_sleep_while_downloading: false,
            after_completion: AfterCompletion::None,
            log_level: "info".to_string(),
            ui_update_hz: 5,
            version: SETTINGS_VERSION,
        }
    }
}

impl Settings {
    pub fn validate(&mut self) {
        self.default_connections = self.default_connections.clamp(1, 32);
        self.max_active_downloads = self.max_active_downloads.clamp(1, 32);
        self.max_global_connections = self.max_global_connections.clamp(1, 128);
        self.retry_count = self.retry_count.clamp(0, 50);
        self.connect_timeout_seconds = self.connect_timeout_seconds.clamp(1, 300);
        self.read_timeout_seconds = self.read_timeout_seconds.clamp(1, 3600);
        if self.temp_dir.as_deref() == Some("") {
            self.temp_dir = None;
        }
    }

    /// Parse from the stored JSON, falling back to defaults when corrupt.
    pub fn from_json(json: &str) -> Self {
        match serde_json::from_str::<Settings>(json) {
            Ok(mut s) => {
                s.validate();
                s
            }
            Err(_) => {
                tracing::warn!("settings document is corrupt; using defaults");
                Settings::default()
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}
