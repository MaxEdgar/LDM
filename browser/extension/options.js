// LDM extension options page.

const DEFAULTS = {
  enabled: true,
  autoCapture: true,
  sendCookies: false,
  captureExtensions: [".iso", ".zip", ".rar", ".7z", ".exe", ".dmg", ".tar", ".tar.gz", ".gz", ".xz", ".bz2", ".deb", ".rpm", ".apk", ".appimage", ".mp4", ".mkv", ".mov", ".avi", ".mp3", ".flac", ".pdf"],
  excludeHosts: ["accounts.google.com", "login.microsoftonline.com", "signin.aws.amazon.com", "paypal.com", "github.com/login"],
};

const $ = (id) => document.getElementById(id);

chrome.storage.local.get(DEFAULTS).then((s) => {
  $("enabled").checked = s.enabled;
  $("autoCapture").checked = s.autoCapture;
  $("sendCookies").checked = s.sendCookies;
  $("captureExtensions").value = s.captureExtensions.join(", ");
  $("excludeHosts").value = s.excludeHosts.join(", ");
});

$("save").addEventListener("click", async () => {
  const split = (v) =>
    v.split(",").map((s) => s.trim().toLowerCase()).filter(Boolean);
  await chrome.storage.local.set({
    enabled: $("enabled").checked,
    autoCapture: $("autoCapture").checked,
    sendCookies: $("sendCookies").checked,
    captureExtensions: split($("captureExtensions").value),
    excludeHosts: split($("excludeHosts").value),
  });
  const st = $("status");
  st.textContent = "Saved ✓";
  setTimeout(() => (st.textContent = ""), 2000);
});
