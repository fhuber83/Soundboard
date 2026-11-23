//! System-wide hotkeys.
//!
//! * Windows / macOS / Linux X11: `tauri-plugin-global-shortcut` (key grabs).
//! * Linux Wayland: the XDG `GlobalShortcuts` portal (KDE Plasma, GNOME 48+,
//!   Hyprland via xdg-desktop-portal-hyprland, ...). Compositors don't allow
//!   key grabs on Wayland, so the portal is the only sanctioned way. The
//!   desktop shows a confirmation dialog the first time and lets the user
//!   reassign the triggers in its own settings.
//!
//! Accelerators are stored as `Mod+Mod+Code`, where `Code` is a DOM
//! `KeyboardEvent.code` value (`KeyA`, `Digit1`, `F5`, `Numpad3`, ...).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Button(String),
    StopAll,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    /// "global", "portal" or "none".
    pub backend: String,
    /// True when system-wide hotkeys are working; the UI then doesn't
    /// handle key presses itself (that would trigger sounds twice).
    pub active: bool,
    pub messages: Vec<String>,
}

#[derive(Default)]
pub struct Hotkeys {
    status: Mutex<HotkeyStatus>,
    /// Plugin backend: shortcut id -> actions.
    plugin_map: Mutex<HashMap<u32, Vec<Action>>>,
    #[cfg(target_os = "linux")]
    portal: Mutex<Option<portal::Running>>,
}

impl Hotkeys {
    pub fn status(&self) -> HotkeyStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, app: &AppHandle, status: HotkeyStatus) {
        for m in &status.messages {
            log::warn!("hotkeys: {m}");
        }
        *self.status.lock().unwrap() = status.clone();
        let _ = app.emit("hotkey-status", status);
    }
}

pub fn use_portal() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// The plugin must be registered during app setup even when unused.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_global_shortcut::{Builder, ShortcutState};
    Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let actions = app
                .state::<crate::AppState>()
                .hotkeys
                .plugin_map
                .lock()
                .unwrap()
                .get(&shortcut.id())
                .cloned()
                .unwrap_or_default();
            for action in actions {
                crate::dispatch(app, &action);
            }
        })
        .build()
}

/// (Re-)register all hotkeys. Called on startup and whenever the config changes.
pub fn apply(app: &AppHandle, bindings: Vec<(String, Action, String)>) {
    if use_portal() {
        #[cfg(target_os = "linux")]
        portal::apply(app, bindings);
    } else {
        apply_plugin(app, bindings);
    }
}

/// Asks the desktop to show its shortcut configuration UI (portal v2+).
pub fn configure(app: &AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if use_portal() {
        let state = app.state::<crate::AppState>();
        let guard = state.hotkeys.portal.lock().unwrap();
        let running = guard
            .as_ref()
            .ok_or("The shortcuts portal is not running")?;
        return running.configure.try_send(()).map_err(|e| e.to_string());
    }
    let _ = app;
    Err("Only available on Wayland".into())
}

fn apply_plugin(app: &AppHandle, bindings: Vec<(String, Action, String)>) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
    let state = app.state::<crate::AppState>();
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let mut map: HashMap<u32, Vec<Action>> = HashMap::new();
    let mut shortcuts: HashMap<u32, Shortcut> = HashMap::new();
    let mut messages = Vec::new();
    for (accel, action, _) in bindings {
        match accel.parse::<Shortcut>() {
            Ok(sc) => {
                map.entry(sc.id()).or_default().push(action);
                shortcuts.insert(sc.id(), sc);
            }
            Err(e) => messages.push(format!("Invalid hotkey \"{accel}\": {e}")),
        }
    }
    for sc in shortcuts.values() {
        if let Err(e) = gs.register(*sc) {
            messages.push(format!(
                "Could not register {}: {e} (in use by another app?)",
                sc.into_string()
            ));
        }
    }
    *state.hotkeys.plugin_map.lock().unwrap() = map;
    state.hotkeys.set_status(
        app,
        HotkeyStatus {
            backend: "global".into(),
            active: true,
            messages,
        },
    );
}

/// Converts `Ctrl+Shift+KeyA` into the XDG "shortcuts" spec format used by
/// the portal: `CTRL+SHIFT+a`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn to_xdg_trigger(accel: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut key = None;
    for token in accel.split('+') {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => parts.push("CTRL".to_string()),
            "alt" => parts.push("ALT".to_string()),
            "shift" => parts.push("SHIFT".to_string()),
            "super" | "meta" | "cmd" => parts.push("LOGO".to_string()),
            _ => key = Some(token),
        }
    }
    let code = key?;
    let sym = if let Some(c) = code.strip_prefix("Key") {
        c.to_ascii_lowercase()
    } else if let Some(d) = code.strip_prefix("Digit") {
        d.to_string()
    } else if let Some(n) = code.strip_prefix("Numpad") {
        match n {
            "Add" => "KP_Add".into(),
            "Subtract" => "KP_Subtract".into(),
            "Multiply" => "KP_Multiply".into(),
            "Divide" => "KP_Divide".into(),
            "Decimal" => "KP_Decimal".into(),
            "Enter" => "KP_Enter".into(),
            "Equal" => "KP_Equal".into(),
            d => format!("KP_{d}"),
        }
    } else {
        match code {
            "Space" => "space",
            "Enter" => "Return",
            "Backspace" => "BackSpace",
            "PageUp" => "Prior",
            "PageDown" => "Next",
            "ArrowUp" => "Up",
            "ArrowDown" => "Down",
            "ArrowLeft" => "Left",
            "ArrowRight" => "Right",
            "Minus" => "minus",
            "Equal" => "equal",
            "BracketLeft" => "bracketleft",
            "BracketRight" => "bracketright",
            "Semicolon" => "semicolon",
            "Quote" => "apostrophe",
            "Backquote" => "grave",
            "Backslash" => "backslash",
            "Comma" => "comma",
            "Period" => "period",
            "Slash" => "slash",
            "ScrollLock" => "Scroll_Lock",
            "PrintScreen" => "Print",
            other => other, // F1..F24, Escape, Tab, Home, End, Insert, Delete, Pause
        }
        .to_string()
    };
    parts.push(sym);
    Some(parts.join("+"))
}

#[cfg(target_os = "linux")]
mod portal {
    use super::*;
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;
    use tokio::sync::oneshot;

    pub struct Running {
        stop: oneshot::Sender<()>,
        pub configure: tokio::sync::mpsc::Sender<()>,
    }

    pub fn apply(app: &AppHandle, bindings: Vec<(String, Action, String)>) {
        let state = app.state::<crate::AppState>();
        // Close the previous session; the new one carries the full set.
        if let Some(old) = state.hotkeys.portal.lock().unwrap().take() {
            let _ = old.stop.send(());
        }
        let (stop_tx, stop_rx) = oneshot::channel();
        let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(1);
        *state.hotkeys.portal.lock().unwrap() = Some(Running {
            stop: stop_tx,
            configure: cfg_tx,
        });

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = run(&app, bindings, stop_rx, cfg_rx).await {
                app.state::<crate::AppState>().hotkeys.set_status(
                    &app,
                    HotkeyStatus {
                        backend: "portal".into(),
                        active: false,
                        messages: vec![format!(
                            "Global shortcuts portal unavailable ({e}). Hotkeys only work while the window is focused."
                        )],
                    },
                );
            }
        });
    }

    /// Writes `~/.local/share/applications/<app-id>.desktop` (hidden from
    /// menus; installed packages provide the visible launcher) so the portal
    /// can resolve our app id, plus an icon for the desktop's shortcut UI.
    fn ensure_desktop_entry() -> std::io::Result<()> {
        let Some(data) = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
        else {
            return Ok(());
        };
        // AppImages are mounted at a random path; point at the image itself.
        let exe = match std::env::var_os("APPIMAGE") {
            Some(p) => std::path::PathBuf::from(p),
            None => std::env::current_exe()?,
        };
        let icon_dir = data.join("icons/hicolor/128x128/apps");
        let icon_path = icon_dir.join(format!("{}.png", crate::APP_ID));
        if !icon_path.exists() {
            std::fs::create_dir_all(&icon_dir)?;
            std::fs::write(&icon_path, include_bytes!("../icons/128x128.png"))?;
        }
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Soundboard\nComment=Soundboard with global hotkeys and Stream Deck support\nExec=\"{}\" %F\nIcon={}\nCategories=AudioVideo;Audio;\nNoDisplay=true\n",
            exe.display(),
            crate::APP_ID
        );
        let dir = data.join("applications");
        let path = dir.join(format!("{}.desktop", crate::APP_ID));
        if std::fs::read_to_string(&path).ok().as_deref() != Some(entry.as_str()) {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, entry)?;
            log::info!("wrote {}", path.display());
        }
        Ok(())
    }

    async fn run(
        app: &AppHandle,
        bindings: Vec<(String, Action, String)>,
        mut stop: oneshot::Receiver<()>,
        mut configure: tokio::sync::mpsc::Receiver<()>,
    ) -> ashpd::Result<()> {
        // The portal identifies non-sandboxed apps through the host Registry,
        // which requires a matching `<app-id>.desktop` file.
        if let Err(e) = ensure_desktop_entry() {
            log::warn!("could not write desktop entry: {e}");
        }
        if let Ok(id) = crate::APP_ID.try_into()
            && let Err(e) = ashpd::register_host_app(id).await
        {
            log::debug!("portal host registration failed: {e}");
        }

        // Portal shortcut ids embed the accelerator, so changing a hotkey in
        // the app creates a new shortcut with the new preferred trigger
        // instead of the desktop silently keeping the old one.
        let mut actions: HashMap<String, Action> = HashMap::new();
        let mut shortcuts = Vec::new();
        let mut messages = Vec::new();
        for (accel, action, description) in bindings {
            let Some(trigger) = to_xdg_trigger(&accel) else {
                messages.push(format!("Invalid hotkey \"{accel}\""));
                continue;
            };
            let id = match &action {
                Action::Button(b) => format!("btn-{b}-{accel}"),
                Action::StopAll => format!("stop-all-{accel}"),
            };
            shortcuts.push(
                NewShortcut::new(id.clone(), description).preferred_trigger(trigger.as_str()),
            );
            actions.insert(id, action);
        }

        let gs = GlobalShortcuts::new().await?;
        let session = gs.create_session(Default::default()).await?;
        let mut activated = gs.receive_activated().await?;

        if !shortcuts.is_empty() {
            let response = gs
                .bind_shortcuts(&session, &shortcuts, None, Default::default())
                .await?
                .response();
            match response {
                Ok(bound) => {
                    let unassigned: Vec<_> = bound
                        .shortcuts()
                        .iter()
                        .filter(|s| s.trigger_description().is_empty())
                        .map(|s| s.description().to_string())
                        .collect();
                    if !unassigned.is_empty() {
                        messages.push(format!(
                            "No key is assigned yet for: {}. Use Settings → \"Configure in desktop…\" or your desktop's shortcut settings.",
                            unassigned.join(", ")
                        ));
                    }
                }
                Err(e) => messages.push(format!("Shortcut binding was not confirmed: {e}")),
            }
        }
        app.state::<crate::AppState>().hotkeys.set_status(
            app,
            HotkeyStatus {
                backend: "portal".into(),
                active: true,
                messages,
            },
        );

        loop {
            tokio::select! {
                _ = &mut stop => break,
                Some(()) = configure.recv() => {
                    if let Err(e) = gs.configure_shortcuts(&session, None, Default::default()).await {
                        let _ = app.emit("app-error", format!("Could not open the desktop's shortcut settings: {e}"));
                    }
                }
                ev = activated.next() => {
                    let Some(ev) = ev else { break };
                    // Signals only reach the session owner (us), and previous
                    // sessions are closed before a new one is created.
                    if let Some(action) = actions.get(ev.shortcut_id()) {
                        crate::dispatch(app, action);
                    }
                }
            }
        }
        let _ = session.close().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::to_xdg_trigger;

    #[test]
    fn xdg_triggers() {
        assert_eq!(
            to_xdg_trigger("Ctrl+Shift+KeyA").as_deref(),
            Some("CTRL+SHIFT+a")
        );
        assert_eq!(to_xdg_trigger("Alt+Numpad7").as_deref(), Some("ALT+KP_7"));
        assert_eq!(to_xdg_trigger("F13").as_deref(), Some("F13"));
        assert_eq!(to_xdg_trigger("Super+Digit1").as_deref(), Some("LOGO+1"));
        assert_eq!(
            to_xdg_trigger("Ctrl+Alt+Backspace").as_deref(),
            Some("CTRL+ALT+BackSpace")
        );
        assert_eq!(to_xdg_trigger("Ctrl+Shift"), None);
    }
}
