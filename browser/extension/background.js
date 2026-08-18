// LDM browser extension — background service worker.
//
// Flow: a download starts in the browser -> if it matches the user's capture
// rules and LDM integration is enabled, we cancel the browser download and
// hand URL / referrer / cookies to the desktop app via the native host.
// When auto-capture is off, we show a notification with a "Send to LDM" action
// so the user stays in control (spec §21, §22).

const HOST_NAME = "ldm";
const NATIVE_TIMEOUT_MS = 3000;

const DEFAULT_SETTINGS = {
  enabled: true,
  autoCapture: true,
  sendCookies: false,
  captureExtensions: [
    ".iso", ".zip", ".rar", ".7z", ".exe", ".dmg", ".tar", ".tar.gz", ".gz",
    ".xz", ".bz2", ".deb", ".rpm", ".apk", ".appimage", ".mp4", ".mkv",
    ".mov", ".avi", ".mp3", ".flac", ".pdf",
  ],
  excludeHosts: [
    "accounts.google.com", "login.microsoftonline.com", "signin.aws.amazon.com",
    "paypal.com", "github.com/login",
  ],
};

// Never intercept obvious login/account pages even if the extension is on.
const ALWAYS_EXCLUDE = ["login", "signin", "auth", "account", "bank", "wallet"];

let port = null;

function getSettings() {
  return chrome.storage.local.get(DEFAULT_SETTINGS);
}

function extOf(name) {
  const lower = name.toLowerCase();
  for (const multi of [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"]) {
    if (lower.endsWith(multi)) return multi;
  }
  const i = lower.lastIndexOf(".");
  return i > 0 ? lower.slice(i) : "";
}

function matchesCapture(url, filename, settings) {
  if (!settings.enabled) return false;
  const host = new URL(url).hostname;
  if (settings.excludeHosts.some((h) => host === h || host.endsWith("." + h))) {
    return false;
  }
  const lower = host + url.split("?")[0].toLowerCase();
  if (ALWAYS_EXCLUDE.some((k) => lower.includes(k))) return false;
  const ext = extOf(filename || url);
  if (!ext) return false;
  return settings.captureExtensions.includes(ext);
}

function openPort() {
  if (port) return port;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
    port.onDisconnect.addListener(() => {
      port = null;
    });
  } catch (e) {
    port = null;
  }
  return port;
}

function sendToLDM(download) {
  const p = openPort();
  if (!p) {
    notify("LDM is not running", "Open the LDM desktop app and try again.");
    return;
  }
  const msg = {
    action: "add_download",
    url: download.url,
    filename: download.filename || undefined,
    referrer: download.referrer || undefined,
  };
  if (msg.cookies === undefined) delete msg.cookies;
  const send = async () => {
    if (download.sendCookies) {
      try {
        const cookies = await chrome.cookies.getAll({ url: download.url });
        msg.cookies = cookies.map((c) => [c.name, c.value]);
      } catch (e) {
        msg.cookies = [];
      }
    }
    p.postMessage(msg);
    notify(
      "Sent to LDM",
      (download.filename || download.url.split("/").pop()) + " is downloading in LDM."
    );
  };
  send();
}

function notify(title, message) {
  try {
    chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: title,
      message: message,
    });
  } catch (e) {
    // Notifications are best-effort.
  }
}

chrome.downloads.onCreated.addListener(async (item) => {
  if (item.state !== "in_progress") return;
  const settings = await getSettings();
  if (!matchesCapture(item.url, item.filename, settings)) return;

  const record = {
    url: item.url,
    filename: item.filename,
    referrer: item.referrer,
    sendCookies: settings.sendCookies,
  };

  if (settings.autoCapture) {
    // Cancel the browser's own download and take it over.
    try {
      await chrome.downloads.cancel(item.id);
    } catch (e) {
      // The download may already have finished; take over anyway.
    }
    sendToLDM(record);
  } else {
    const nid = "ldm-capture-" + item.id;
    chrome.notifications.create(nid, {
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: "Download detected",
      message: (item.filename || "file") + " — download with LDM?",
      buttons: [{ title: "Download with LDM" }, { title: "Ignore" }],
    });
    const handler = (id, btn) => {
      if (id !== nid) return;
      chrome.notifications.onButtonClicked.removeListener(handler);
      if (btn === 0) {
        chrome.downloads.cancel(item.id).catch(() => {});
        sendToLDM(record);
      }
    };
    chrome.notifications.onButtonClicked.addListener(handler);
  }
});

// The extension toolbar popup opens options.html; keep it light.
chrome.runtime.onInstalled.addListener(async () => {
  const cur = await chrome.storage.local.get(DEFAULT_SETTINGS);
  chrome.storage.local.set(cur);
});
