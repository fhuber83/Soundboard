// Video player window. Receives `video-play` / `video-stop` events from the
// backend; the first request is fetched with `take_pending_video` because the
// window is created on demand.
const { invoke } = window.__TAURI__.core;
const { getCurrentWebviewWindow } = window.__TAURI__.webviewWindow;
const { listen } = window.__TAURI__.event;

const win = getCurrentWebviewWindow();
const log = (level, message) => invoke("ui_log", { level, message: `video: ${message}` }).catch(() => {});
window.addEventListener("error", (e) => log("error", e.message));
window.addEventListener("unhandledrejection", (e) => log("error", String(e.reason)));
const video = document.getElementById("v");
let current = null;

function finish() {
  video.pause();
  video.removeAttribute("src");
  video.load();
  const closeOnEnd = current?.closeOnEnd;
  current = null;
  if (closeOnEnd) invoke("video_finished");
}

async function play(req) {
  if (req.toggle && current?.id === req.id && !video.paused) {
    finish();
    return;
  }
  current = req;
  log("info", `playing ${req.path}`);
  video.src = req.url;
  video.volume = Math.min(1, Math.max(0, req.volume));
  video.loop = !!req.looped;
  try {
    await video.play();
  } catch (e) {
    log("error", `play() failed for ${req.path}: ${e.message || e}`);
    document.getElementById("hint").textContent = `Cannot play: ${e.message || e}`;
    document.getElementById("hint").style.opacity = 1;
  }
}

video.addEventListener("ended", finish);
video.addEventListener("error", () => {
  const err = video.error;
  log("error", `media error ${err?.code}: ${err?.message || ""} (${current?.path})`);
  const hint = document.getElementById("hint");
  hint.textContent = `Cannot play this video (code ${err?.code ?? "?"}). On Linux, install the GStreamer plugins (gst-plugins-good/bad/ugly, gst-libav).`;
  hint.style.opacity = 1;
});

window.addEventListener("dblclick", async () => {
  await win.setFullscreen(!(await win.isFullscreen()));
});
window.addEventListener("keydown", async (e) => {
  if (e.key === "Escape") {
    if (await win.isFullscreen()) await win.setFullscreen(false);
    else {
      current = { ...current, closeOnEnd: true };
      finish();
    }
  }
});

await listen("video-play", (e) => play(e.payload));
await listen("video-stop", () => finish());

const pending = await invoke("take_pending_video");
if (pending) play(pending);
