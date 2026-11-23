// Soundboard UI. Uses the global Tauri API (app.withGlobalTauri = true), so
// there is no bundler or npm runtime dependency.
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWebview } = window.__TAURI__.webview;

const VIDEO_EXT = ["mp4", "webm", "mkv", "mov", "m4v", "ogv", "avi"];
const MEDIA_EXT = ["mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac", ...VIDEO_EXT];
const COLORS = ["#3b82f6", "#8b5cf6", "#ec4899", "#ef4444", "#f97316", "#f59e0b", "#84cc16", "#10b981", "#14b8a6", "#06b6d4", "#64748b", "#334155"];
const MODIFIER_CODES = /^(Control|Shift|Alt|Meta|OS|Super|Hyper)(Left|Right)?$/;

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  config: { buttons: [], settings: {} },
  deck: null,        // DeckInfo from backend
  deckError: null,
  hotkeys: { backend: "none", active: false, messages: [] },
  wayland: false,
  platform: "",
  playing: new Set(),
  capturing: false,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const ext = (p) => (p.split(/[\\/]/).pop().split(".").pop() || "").toLowerCase();
const basename = (p) => p.split(/[\\/]/).pop().replace(/\.[^.]+$/, "");
const kindFor = (p) => (VIDEO_EXT.includes(ext(p)) ? "video" : "audio");
const uid = () => (crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).slice(2));

function prettyHotkey(accel) {
  if (!accel) return "";
  return accel
    .split("+")
    .map((t) => t.replace(/^Key(.)$/, "$1").replace(/^Digit(.)$/, "$1").replace(/^Numpad/, "Num ").replace(/^Arrow/, ""))
    .join(" + ");
}

/** Builds `Ctrl+Alt+Shift+Super+Code` from a KeyboardEvent, or null for bare modifiers. */
function accelFromEvent(e) {
  if (MODIFIER_CODES.test(e.code) || !e.code) return null;
  const parts = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Super");
  parts.push(e.code);
  return parts.join("+");
}

const uiLog = (level, message) => invoke("ui_log", { level, message }).catch(() => {});
window.addEventListener("error", (e) => uiLog("error", `${e.message} (${e.filename}:${e.lineno})`));
window.addEventListener("unhandledrejection", (e) => uiLog("error", String(e.reason)));

function toast(msg, kind = "error", ms = 6000) {
  if (kind === "error") uiLog("warn", msg);
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = msg;
  $("#toasts").append(el);
  setTimeout(() => el.remove(), ms);
}

let saveTimer = null;
async function save({ debounce = 0 } = {}) {
  clearTimeout(saveTimer);
  const run = async () => {
    try {
      await invoke("save_config", { config: state.config });
    } catch (e) {
      toast(`Saving failed: ${e}`);
    }
  };
  if (debounce) saveTimer = setTimeout(run, debounce);
  else await run();
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const EDIT_SVG = '<svg viewBox="0 0 24 24"><path d="M3 17.25V21h3.75L17.8 9.94l-3.75-3.75L3 17.25zM20.7 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z"/></svg>';

function renderGrid() {
  const grid = $("#grid");
  grid.replaceChildren();
  const { buttons } = state.config;
  $("#empty").hidden = buttons.length > 0;

  for (const b of buttons) {
    const tile = document.createElement("div");
    tile.className = "tile";
    tile.dataset.id = b.id;
    tile.tabIndex = 0;
    tile.draggable = true;
    tile.style.setProperty("--c", b.color || "#3b82f6");
    if (b.icon) {
      tile.classList.add("has-icon");
      tile.style.backgroundImage = `url("${convertFileSrc(b.icon)}")`;
    }
    if (!b.path) tile.classList.add("missing");
    if (state.playing.has(b.id)) tile.classList.add("playing");

    const badges = document.createElement("div");
    badges.className = "badges";
    const addBadge = (text, title) => {
      const s = document.createElement("span");
      s.className = "badge";
      s.textContent = text;
      if (title) s.title = title;
      badges.append(s);
    };
    if (b.kind === "video") addBadge("▶ video");
    if (b.hotkey) addBadge(prettyHotkey(b.hotkey), "Hotkey");
    if (b.deckKey != null) addBadge(`SD ${b.deckKey + 1}`, "Stream Deck key");
    if (b.looped) addBadge("⟳", "Loops");

    const label = document.createElement("div");
    label.className = "label";
    label.textContent = b.label || basename(b.path) || "Untitled";

    const edit = document.createElement("button");
    edit.className = "edit";
    edit.title = "Edit";
    edit.innerHTML = EDIT_SVG;
    edit.addEventListener("click", (e) => {
      e.stopPropagation();
      openEditor(b.id);
    });

    tile.append(badges, label, edit);
    tile.addEventListener("click", () => invoke("trigger", { id: b.id }));
    tile.addEventListener("keydown", (e) => {
      if ((e.key === "Enter" || e.key === " ") && e.target === tile) {
        e.preventDefault();
        invoke("trigger", { id: b.id });
      }
    });
    tile.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      openEditor(b.id);
    });
    attachReorder(tile);
    grid.append(tile);
  }
}

function renderStatus() {
  const deckChip = $("#chip-deck");
  if (state.deck) {
    deckChip.textContent = `Stream Deck: ${state.deck.model} (${state.deck.keyCount} keys)`;
    deckChip.className = "chip ok";
    deckChip.title = `Serial ${state.deck.serial}`;
  } else if (state.deckError) {
    deckChip.textContent = "Stream Deck: error";
    deckChip.className = "chip warn";
    deckChip.title = state.deckError;
  } else {
    deckChip.textContent = "Stream Deck: not connected";
    deckChip.className = "chip";
    deckChip.title = "Plug in a Stream Deck; it is detected automatically.";
  }

  const hk = state.hotkeys;
  const hkChip = $("#chip-hotkeys");
  const label = hk.backend === "portal" ? "Hotkeys: global (portal)" : hk.backend === "global" ? "Hotkeys: global" : "Hotkeys: –";
  hkChip.textContent = hk.active ? label : "Hotkeys: window only";
  hkChip.className = `chip ${hk.active && !hk.messages.length ? "ok" : "warn"}`;
  hkChip.title = hk.messages.join("\n") || (hk.active ? "Hotkeys work system-wide." : "");

  const banner = $("#banner");
  const msgs = [];
  if (state.deckError) msgs.push(state.deckError);
  for (const m of hk.messages) msgs.push(m);
  banner.hidden = msgs.length === 0;
  banner.replaceChildren(...msgs.map((m) => Object.assign(document.createElement("div"), { textContent: "⚠ " + m })));
}

function renderMaster() {
  const v = Math.round((state.config.settings.masterVolume ?? 0.8) * 100);
  $("#master").value = v;
  $("#master-val").textContent = v;
}

function renderAll() {
  renderGrid();
  renderStatus();
  renderMaster();
  renderDeck();
}

// ---------------------------------------------------------------------------
// Stream Deck key images (rendered here, sent to the backend as PNG)
// ---------------------------------------------------------------------------

const iconCache = new Map();
function loadIcon(path) {
  if (!iconCache.has(path)) {
    iconCache.set(
      path,
      new Promise((resolve) => {
        const img = new Image();
        img.crossOrigin = "anonymous";
        img.onload = () => resolve(img);
        img.onerror = () => resolve(null);
        img.src = convertFileSrc(path);
      }),
    );
  }
  return iconCache.get(path);
}

function wrapText(ctx, text, maxWidth, maxLines) {
  const words = text.split(/\s+/).filter(Boolean);
  const lines = [];
  let line = "";
  for (const w of words) {
    const test = line ? `${line} ${w}` : w;
    if (ctx.measureText(test).width <= maxWidth || !line) line = test;
    else {
      lines.push(line);
      line = w;
    }
  }
  if (line) lines.push(line);
  if (lines.length > maxLines) {
    lines.length = maxLines;
    lines[maxLines - 1] = lines[maxLines - 1].replace(/.{0,2}$/, "…");
  }
  return lines;
}

async function renderKey(w, h, { color, label, icon, playing, stop }) {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d");
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, w, h);

  if (icon) {
    const img = await loadIcon(icon);
    if (img) {
      const s = Math.max(w / img.width, h / img.height);
      ctx.drawImage(img, (w - img.width * s) / 2, (h - img.height * s) / 2, img.width * s, img.height * s);
      const g = ctx.createLinearGradient(0, h, 0, h * 0.35);
      g.addColorStop(0, "rgba(0,0,0,0.8)");
      g.addColorStop(1, "rgba(0,0,0,0)");
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, w, h);
    }
  }

  if (stop) {
    ctx.fillStyle = "#fff";
    const s = w * 0.3;
    ctx.fillRect((w - s) / 2, h * 0.22, s, s);
  }

  const fontPx = Math.round(w * 0.17);
  ctx.font = `600 ${fontPx}px system-ui, sans-serif`;
  ctx.fillStyle = "#fff";
  ctx.textAlign = "center";
  ctx.textBaseline = "alphabetic";
  ctx.shadowColor = "rgba(0,0,0,0.7)";
  ctx.shadowBlur = 3;
  const lines = wrapText(ctx, label, w * 0.9, 3);
  const lh = fontPx * 1.1;
  const startY = icon || stop ? h - 6 - (lines.length - 1) * lh : (h - lines.length * lh) / 2 + fontPx * 0.85;
  lines.forEach((l, i) => ctx.fillText(l, w / 2, startY + i * lh));
  ctx.shadowBlur = 0;

  if (playing) {
    ctx.strokeStyle = "#fff";
    ctx.lineWidth = Math.max(3, w * 0.05);
    ctx.strokeRect(0, 0, w, h);
  }

  // The Deck renders dark colors poorly; the background is drawn fully saturated already.
  return c.toDataURL("image/png");
}

let deckRenderSeq = 0;
async function renderDeck() {
  const deck = state.deck;
  if (!deck) return;
  const seq = ++deckRenderSeq;
  const [w, h] = deck.imageSize;
  const stopKey = state.config.settings.deckStopAllKey;
  const images = [];
  for (let key = 0; key < deck.keyCount; key++) {
    let png = null;
    if (stopKey === key) {
      png = await renderKey(w, h, { color: "#b91c1c", label: "Stop all", stop: true });
    } else {
      const b = state.config.buttons.find((x) => x.deckKey === key);
      if (b) {
        try {
          png = await renderKey(w, h, {
            color: b.color || "#3b82f6",
            label: b.label || basename(b.path),
            icon: b.icon,
            playing: state.playing.has(b.id),
          });
        } catch (e) {
          // e.g. a tainted canvas if an icon couldn't be loaded with CORS
          png = await renderKey(w, h, { color: b.color || "#3b82f6", label: b.label || basename(b.path) });
        }
      }
    }
    if (seq !== deckRenderSeq) return; // a newer render started
    images.push({ key, png });
  }
  invoke("deck_set_images", { images }).catch((e) => toast(`Stream Deck: ${e}`));
}

// ---------------------------------------------------------------------------
// Reordering tiles by drag & drop
// ---------------------------------------------------------------------------

let dragId = null;
function attachReorder(tile) {
  tile.addEventListener("dragstart", (e) => {
    dragId = tile.dataset.id;
    tile.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", dragId);
  });
  tile.addEventListener("dragend", () => {
    dragId = null;
    tile.classList.remove("dragging");
    document.querySelectorAll(".drag-over").forEach((t) => t.classList.remove("drag-over"));
  });
  tile.addEventListener("dragover", (e) => {
    if (!dragId || dragId === tile.dataset.id) return;
    e.preventDefault();
    tile.classList.add("drag-over");
  });
  tile.addEventListener("dragleave", () => tile.classList.remove("drag-over"));
  tile.addEventListener("drop", (e) => {
    e.preventDefault();
    if (!dragId || dragId === tile.dataset.id) return;
    const list = state.config.buttons;
    const from = list.findIndex((b) => b.id === dragId);
    const [moved] = list.splice(from, 1);
    const to = list.findIndex((b) => b.id === tile.dataset.id);
    list.splice(from <= to ? to + 1 : to, 0, moved);
    renderGrid();
    save();
  });
}

// ---------------------------------------------------------------------------
// Adding files
// ---------------------------------------------------------------------------

function nextFreeDeckKey() {
  if (!state.deck) return null;
  const used = new Set(state.config.buttons.map((b) => b.deckKey).filter((k) => k != null));
  if (state.config.settings.deckStopAllKey != null) used.add(state.config.settings.deckStopAllKey);
  for (let k = 0; k < state.deck.keyCount; k++) if (!used.has(k)) return k;
  return null;
}

function addFiles(paths) {
  const media = paths.filter((p) => MEDIA_EXT.includes(ext(p)));
  if (media.length < paths.length) toast(`Skipped ${paths.length - media.length} unsupported file(s).`, "info");
  for (const path of media) {
    const n = state.config.buttons.length;
    state.config.buttons.push({
      id: uid(),
      label: basename(path),
      path,
      kind: kindFor(path),
      color: COLORS[n % COLORS.length],
      icon: null,
      volume: 1,
      mode: "restart",
      looped: false,
      hotkey: null,
      deckKey: nextFreeDeckKey(),
    });
  }
  if (media.length) {
    renderGrid();
    renderDeck();
    save();
  }
}

// ---------------------------------------------------------------------------
// Hotkey capture fields
// ---------------------------------------------------------------------------

function setupHotkeyInput(input) {
  input.addEventListener("focus", () => {
    state.capturing = true;
    input.classList.add("capturing");
    input.dataset.prev = input.value;
    input.placeholder = "Press keys… (Esc to cancel)";
  });
  input.addEventListener("blur", () => {
    state.capturing = false;
    input.classList.remove("capturing");
    input.placeholder = "Click, then press a key combination";
  });
  input.addEventListener("keydown", (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey) {
      input.blur();
      return;
    }
    const accel = accelFromEvent(e);
    if (!accel) return;
    input.dataset.accel = accel;
    input.value = prettyHotkey(accel);
    input.blur();
  });
}

function setHotkeyInput(input, accel) {
  input.dataset.accel = accel || "";
  input.value = prettyHotkey(accel);
}

// ---------------------------------------------------------------------------
// Stream Deck key picker
// ---------------------------------------------------------------------------

/**
 * Renders a clickable mini Stream Deck. `owner(key)` returns {label,color} of
 * whatever occupies a key (or null). Calls `onPick(keyOrNull)`.
 */
function renderDeckPicker(container, selected, owner, onPick) {
  container.replaceChildren();
  const cols = state.deck?.columns ?? 5;
  const rows = state.deck?.rows ?? 3;
  container.style.gridTemplateColumns = `repeat(${cols}, auto)`;
  for (let key = 0; key < cols * rows; key++) {
    const btn = document.createElement("button");
    btn.type = "button";
    const o = owner(key);
    btn.textContent = o ? o.label : key + 1;
    btn.title = o ? `Key ${key + 1}: ${o.label}` : `Key ${key + 1}`;
    if (o) {
      btn.classList.add("taken");
      btn.style.background = o.color;
    }
    if (key === selected) btn.classList.add("sel");
    btn.addEventListener("click", () => onPick(key === selected ? null : key));
    container.append(btn);
  }
  const none = document.createElement("button");
  none.type = "button";
  none.className = "none" + (selected == null ? " sel" : "");
  none.textContent = "None";
  none.style.gridColumn = `1 / span ${cols}`;
  none.addEventListener("click", () => onPick(null));
  container.append(none);
}

// ---------------------------------------------------------------------------
// Button editor
// ---------------------------------------------------------------------------

const dlgEdit = $("#dlg-edit");
const formEdit = $("#form-edit");
let editing = null; // working copy
let editingIsNew = false;

function renderEditorDeckPicker() {
  const sel = editing.deckKey ?? null;
  const stopKey = state.config.settings.deckStopAllKey;
  renderDeckPicker(
    $("#deck-picker"),
    sel,
    (key) => {
      if (key === sel) return { label: formEdit.label.value || "This", color: formEdit.color.value };
      if (key === stopKey) return { label: "Stop", color: "#b91c1c" };
      const b = state.config.buttons.find((x) => x.deckKey === key && x.id !== editing.id);
      return b ? { label: b.label, color: b.color } : null;
    },
    (key) => {
      editing.deckKey = key;
      renderEditorDeckPicker();
    },
  );
  $("#deck-key-hint").textContent = state.deck
    ? `(${state.deck.model}; picking a used key moves it here)`
    : "(no device connected — showing a 15-key layout)";
}

function renderSwatches(current) {
  const wrap = $("#swatches");
  wrap.querySelectorAll("button").forEach((b) => b.remove());
  for (const c of COLORS) {
    const b = document.createElement("button");
    b.type = "button";
    b.style.background = c;
    b.title = c;
    if (c.toLowerCase() === current.toLowerCase()) b.classList.add("sel");
    b.addEventListener("click", () => {
      formEdit.color.value = c;
      renderSwatches(c);
    });
    wrap.insertBefore(b, formEdit.color);
  }
}

function openEditor(id) {
  const existing = id && state.config.buttons.find((b) => b.id === id);
  editingIsNew = !existing;
  editing = existing
    ? structuredClone(existing)
    : { id: uid(), label: "", path: "", kind: "audio", color: COLORS[state.config.buttons.length % COLORS.length], icon: null, volume: 1, mode: "restart", looped: false, hotkey: null, deckKey: nextFreeDeckKey() };

  $("#edit-title").textContent = editingIsNew ? "Add button" : "Edit button";
  $('[data-action="delete"]', formEdit).hidden = editingIsNew;
  formEdit.label.value = editing.label;
  formEdit.path.value = editing.path;
  formEdit.kind.value = editing.kind;
  formEdit.mode.value = editing.mode;
  formEdit.volume.value = Math.round(editing.volume * 100);
  $("#vol-val").textContent = `${formEdit.volume.value}%`;
  formEdit.looped.checked = editing.looped;
  formEdit.color.value = editing.color;
  formEdit.icon.value = editing.icon || "";
  setHotkeyInput(formEdit.hotkey, editing.hotkey);
  renderSwatches(editing.color);
  renderEditorDeckPicker();
  dlgEdit.showModal();
  if (editingIsNew) formEdit.label.focus();
}

function readEditor() {
  const path = formEdit.path.value.trim();
  return {
    ...editing,
    label: formEdit.label.value.trim() || basename(path),
    path,
    kind: formEdit.kind.value,
    mode: formEdit.mode.value,
    volume: Number(formEdit.volume.value) / 100,
    looped: formEdit.looped.checked,
    color: formEdit.color.value,
    icon: formEdit.icon.value.trim() || null,
    hotkey: formEdit.hotkey.dataset.accel || null,
  };
}

formEdit.volume.addEventListener("input", () => ($("#vol-val").textContent = `${formEdit.volume.value}%`));
formEdit.color.addEventListener("input", () => renderSwatches(formEdit.color.value));
formEdit.path.addEventListener("change", () => {
  const p = formEdit.path.value.trim();
  if (p) formEdit.kind.value = kindFor(p);
});
setupHotkeyInput(formEdit.hotkey);

formEdit.addEventListener("click", async (e) => {
  const action = e.target.closest("[data-action]")?.dataset.action;
  if (!action) return;
  if (action === "browse-file") {
    const [p] = await invoke("pick_files", { multiple: false, images: false });
    if (p) {
      formEdit.path.value = p;
      formEdit.kind.value = kindFor(p);
      if (!formEdit.label.value) formEdit.label.value = basename(p);
    }
  } else if (action === "browse-icon") {
    const [p] = await invoke("pick_files", { multiple: false, images: true });
    if (p) formEdit.icon.value = p;
  } else if (action === "clear-hotkey") {
    setHotkeyInput(formEdit.hotkey, null);
  } else if (action === "test") {
    const draft = readEditor();
    if (!draft.path) return toast("Choose a file first.");
    invoke("preview", { button: draft }).catch((err) => toast(String(err)));
  } else if (action === "delete") {
    state.config.buttons = state.config.buttons.filter((b) => b.id !== editing.id);
    dlgEdit.close("deleted");
    renderGrid();
    renderDeck();
    save();
  }
});

dlgEdit.addEventListener("close", () => {
  if (dlgEdit.returnValue !== "save") return;
  const b = readEditor();
  if (!b.path) {
    toast("No file selected — the button was saved but won't play anything.", "info");
  }
  // A Stream Deck key or hotkey belongs to one button only.
  for (const other of state.config.buttons) {
    if (other.id === b.id) continue;
    if (b.deckKey != null && other.deckKey === b.deckKey) other.deckKey = null;
    if (b.hotkey && other.hotkey === b.hotkey) {
      other.hotkey = null;
      toast(`Hotkey moved from "${other.label}".`, "info");
    }
  }
  if (b.deckKey != null && state.config.settings.deckStopAllKey === b.deckKey) state.config.settings.deckStopAllKey = null;
  const idx = state.config.buttons.findIndex((x) => x.id === b.id);
  if (idx >= 0) state.config.buttons[idx] = b;
  else state.config.buttons.push(b);
  renderGrid();
  renderDeck();
  save();
});

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

const dlgSettings = $("#dlg-settings");
const formSettings = $("#form-settings");
let settingsStopKey = null;

function fillDeviceSelect(select, devices, value, defaultLabel) {
  select.replaceChildren(new Option(defaultLabel, ""));
  for (const d of devices) select.append(new Option(d.name, d.id));
  if (value && !devices.some((d) => d.id === value)) select.append(new Option(`${value} (unavailable)`, value));
  select.value = value || "";
}

function renderSettingsDeckPicker() {
  renderDeckPicker(
    $("#deck-stop-picker"),
    settingsStopKey,
    (key) => {
      if (key === settingsStopKey) return { label: "Stop", color: "#b91c1c" };
      const b = state.config.buttons.find((x) => x.deckKey === key);
      return b ? { label: b.label, color: b.color } : null;
    },
    (key) => {
      settingsStopKey = key;
      renderSettingsDeckPicker();
    },
  );
}

async function openSettings() {
  const s = state.config.settings;
  let devices = [];
  try {
    devices = await invoke("list_audio_devices");
  } catch (e) {
    toast(`Could not list audio devices: ${e}`);
  }
  fillDeviceSelect(formSettings.outputDevice, devices, s.outputDevice, "System default");
  fillDeviceSelect(formSettings.monitorDevice, devices, s.monitorDevice, "Off");
  formSettings.monitorVolume.value = Math.round((s.monitorVolume ?? 0.8) * 100);
  $("#mon-val").textContent = `${formSettings.monitorVolume.value}%`;
  setHotkeyInput(formSettings.stopAllHotkey, s.stopAllHotkey);
  formSettings.deckBrightness.value = s.deckBrightness ?? 70;
  $("#bright-val").textContent = `${formSettings.deckBrightness.value}%`;
  formSettings.videoFullscreen.checked = !!s.videoFullscreen;
  formSettings.videoAlwaysOnTop.checked = !!s.videoAlwaysOnTop;
  formSettings.videoCloseOnEnd.checked = !!s.videoCloseOnEnd;
  settingsStopKey = s.deckStopAllKey ?? null;
  renderSettingsDeckPicker();

  $("#hotkey-help").textContent = state.wayland
    ? "Wayland: hotkeys go through your desktop's Global Shortcuts portal. The first time, your desktop asks you to confirm them. You can also change them later in its shortcut settings."
    : "Hotkeys work system-wide, even when another app has focus.";
  $("#portal-row").hidden = !(state.wayland && state.hotkeys.active);
  $("#deck-help").textContent = state.deck
    ? `${state.deck.model}, serial ${state.deck.serial}.`
    : state.deckError || "No Stream Deck connected.";
  dlgSettings.showModal();
}

formSettings.monitorVolume.addEventListener("input", () => ($("#mon-val").textContent = `${formSettings.monitorVolume.value}%`));
formSettings.deckBrightness.addEventListener("input", () => ($("#bright-val").textContent = `${formSettings.deckBrightness.value}%`));
setupHotkeyInput(formSettings.stopAllHotkey);
formSettings.addEventListener("click", (e) => {
  if (e.target.closest('[data-action="clear-stop-hotkey"]')) setHotkeyInput(formSettings.stopAllHotkey, null);
  if (e.target.closest('[data-action="configure-desktop"]')) invoke("configure_desktop_shortcuts").catch((err) => toast(String(err)));
});

dlgSettings.addEventListener("close", () => {
  if (dlgSettings.returnValue !== "save") return;
  const s = state.config.settings;
  s.outputDevice = formSettings.outputDevice.value || null;
  s.monitorDevice = formSettings.monitorDevice.value || null;
  s.monitorVolume = Number(formSettings.monitorVolume.value) / 100;
  s.stopAllHotkey = formSettings.stopAllHotkey.dataset.accel || null;
  s.deckBrightness = Number(formSettings.deckBrightness.value);
  s.videoFullscreen = formSettings.videoFullscreen.checked;
  s.videoAlwaysOnTop = formSettings.videoAlwaysOnTop.checked;
  s.videoCloseOnEnd = formSettings.videoCloseOnEnd.checked;
  s.deckStopAllKey = settingsStopKey;
  if (settingsStopKey != null) {
    for (const b of state.config.buttons) if (b.deckKey === settingsStopKey) b.deckKey = null;
  }
  renderGrid();
  renderDeck();
  save();
});

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

$("#btn-add").addEventListener("click", async () => {
  const paths = await invoke("pick_files", { multiple: true, images: false });
  if (paths.length) addFiles(paths);
});
$("#btn-settings").addEventListener("click", openSettings);
$("#btn-stop").addEventListener("click", () => invoke("stop_all_cmd"));
$("#master").addEventListener("input", (e) => {
  state.config.settings.masterVolume = Number(e.target.value) / 100;
  $("#master-val").textContent = e.target.value;
  save({ debounce: 150 });
});

// In-window hotkeys, used only when system-wide hotkeys are unavailable
// (otherwise the global grab would fire as well and play the sound twice).
window.addEventListener("keydown", (e) => {
  if (state.hotkeys.active || state.capturing || e.repeat) return;
  if (document.querySelector("dialog[open]")) return;
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement) return;
  const accel = accelFromEvent(e);
  if (!accel) return;
  if (accel === state.config.settings.stopAllHotkey) {
    e.preventDefault();
    invoke("stop_all_cmd");
    return;
  }
  const b = state.config.buttons.find((x) => x.hotkey === accel);
  if (b) {
    e.preventDefault();
    invoke("trigger", { id: b.id });
  }
});

// File drag & drop from the OS (Tauri delivers real paths).
getCurrentWebview().onDragDropEvent((event) => {
  const overlay = $("#drop-overlay");
  const { type } = event.payload;
  if (type === "enter" || type === "over") overlay.hidden = false;
  else overlay.hidden = true;
  if (type === "drop") addFiles(event.payload.paths || []);
});

function flash(id) {
  const tile = document.querySelector(`.tile[data-id="${CSS.escape(id)}"]`);
  if (!tile) return;
  tile.classList.remove("flash");
  void tile.offsetWidth;
  tile.classList.add("flash");
}

async function init() {
  // Subscribe first so no event is lost between the snapshot and the listeners.
  await listen("app-error", (e) => toast(e.payload));
  await listen("triggered", (e) => flash(e.payload));
  await listen("audio-playing", (e) => {
    const before = state.playing;
    state.playing = new Set(e.payload);
    document.querySelectorAll(".tile").forEach((t) => t.classList.toggle("playing", state.playing.has(t.dataset.id)));
    // Only redraw the Deck if a button with a deck key changed state.
    const changed = state.config.buttons.some((b) => b.deckKey != null && before.has(b.id) !== state.playing.has(b.id));
    if (changed) renderDeck();
  });
  await listen("deck-changed", (e) => {
    state.deck = e.payload;
    if (state.deck) state.deckError = null;
    renderStatus();
    renderDeck();
  });
  await listen("deck-error", (e) => {
    state.deckError = e.payload;
    renderStatus();
  });
  await listen("hotkey-status", (e) => {
    state.hotkeys = e.payload;
    renderStatus();
  });

  const s = await invoke("get_state");
  state.config = s.config;
  state.deck = s.deck;
  state.deckError = s.deckError;
  state.hotkeys = s.hotkeys;
  state.wayland = s.wayland;
  state.platform = s.platform;
  $("#config-path").textContent = s.configPath;
  renderAll();
}

init().catch((e) => toast(`Startup failed: ${e}`));
