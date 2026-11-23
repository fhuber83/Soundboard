// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod deck;
mod hotkeys;
mod media_server;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use base64::Engine as _;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use audio::AudioCmd;
use config::{Button, Config, MediaKind, PlayMode};
use deck::{DeckCmd, DeckInfo, DeckState};
use hotkeys::{Action, HotkeyStatus, Hotkeys};

pub const APP_ID: &str = "dev.soundboard.Soundboard";
const VIDEO_WINDOW: &str = "video";

type Binding = (String, Action, String);

pub struct AppState {
    config: RwLock<Config>,
    config_path: PathBuf,
    audio: Sender<AudioCmd>,
    deck_tx: Sender<DeckCmd>,
    pub deck: DeckState,
    pub hotkeys: Hotkeys,
    bindings: Mutex<Option<Vec<Binding>>>,
    pending_video: Mutex<Option<VideoRequest>>,
    media: Option<Arc<media_server::MediaServer>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoRequest {
    id: String,
    path: String,
    url: String,
    volume: f32,
    looped: bool,
    toggle: bool,
    close_on_end: bool,
}

// ---------------------------------------------------------------------------
// Triggering
// ---------------------------------------------------------------------------

pub fn dispatch(app: &AppHandle, action: &Action) {
    match action {
        Action::Button(id) => trigger_button(app, id),
        Action::StopAll => stop_all(app),
    }
}

pub fn deck_key_pressed(app: &AppHandle, key: u8) {
    let state = app.state::<AppState>();
    let action = {
        let cfg = state.config.read().unwrap();
        if cfg.settings.deck_stop_all_key == Some(key) {
            Some(Action::StopAll)
        } else {
            cfg.buttons
                .iter()
                .find(|b| b.deck_key == Some(key))
                .map(|b| Action::Button(b.id.clone()))
        }
    };
    if let Some(action) = action {
        dispatch(app, &action);
    }
}

fn trigger_button(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    let (button, settings) = {
        let cfg = state.config.read().unwrap();
        let Some(b) = cfg.button(id) else { return };
        (b.clone(), cfg.settings.clone())
    };
    play_button(app, &button, &settings);
}

fn play_button(app: &AppHandle, button: &Button, settings: &config::Settings) {
    let state = app.state::<AppState>();
    if button.path.is_empty() {
        let _ = app.emit(
            "app-error",
            format!("\"{}\" has no file assigned", button.label),
        );
        return;
    }
    let _ = app.emit("triggered", &button.id);
    match button.kind {
        MediaKind::Audio => {
            let _ = state.audio.send(AudioCmd::Play {
                id: button.id.clone(),
                path: PathBuf::from(&button.path),
                volume: button.volume,
                mode: button.mode,
                looped: button.looped,
            });
        }
        MediaKind::Video => play_video(app, button, settings),
    }
}

fn stop_all(app: &AppHandle) {
    let state = app.state::<AppState>();
    let _ = state.audio.send(AudioCmd::StopAll);
    let close = state.config.read().unwrap().settings.video_close_on_end;
    if let Some(win) = app.get_webview_window(VIDEO_WINDOW) {
        if close {
            let _ = win.close();
        } else {
            let _ = win.emit("video-stop", ());
        }
    }
}

fn play_video(app: &AppHandle, button: &Button, settings: &config::Settings) {
    let Some(media) = app.state::<AppState>().media.clone() else {
        let _ = app.emit(
            "app-error",
            "Video playback is unavailable (media server failed to start)",
        );
        return;
    };
    let req = VideoRequest {
        id: button.id.clone(),
        path: button.path.clone(),
        url: media.url_for(std::path::Path::new(&button.path)),
        volume: (button.volume * settings.master_volume).clamp(0.0, 1.0),
        looped: button.looped,
        toggle: button.mode == PlayMode::Toggle,
        close_on_end: settings.video_close_on_end,
    };
    let fullscreen = settings.video_fullscreen;
    let on_top = settings.video_always_on_top;
    let app2 = app.clone();
    // Window creation must happen on the main thread (deadlocks on Windows otherwise).
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window(VIDEO_WINDOW) {
            let _ = win.show();
            let _ = win.emit("video-play", req);
            return;
        }
        *app2.state::<AppState>().pending_video.lock().unwrap() = Some(req);
        let built =
            WebviewWindowBuilder::new(&app2, VIDEO_WINDOW, WebviewUrl::App("video.html".into()))
                .title("Soundboard – Video")
                .inner_size(960.0, 540.0)
                .fullscreen(fullscreen)
                .always_on_top(on_top)
                .focused(false)
                .build();
        if let Err(e) = built {
            log::error!("could not open video window: {e}");
            let _ = app2.emit("app-error", format!("Could not open video window: {e}"));
        }
    });
}

// ---------------------------------------------------------------------------
// Applying configuration
// ---------------------------------------------------------------------------

fn bindings_for(cfg: &Config) -> Vec<Binding> {
    let mut out: Vec<Binding> = cfg
        .buttons
        .iter()
        .filter_map(|b| {
            let hk = b.hotkey.as_ref().filter(|h| !h.is_empty())?;
            let label = if b.label.is_empty() {
                "Sound"
            } else {
                &b.label
            };
            Some((
                hk.clone(),
                Action::Button(b.id.clone()),
                format!("Soundboard: {label}"),
            ))
        })
        .collect();
    if let Some(hk) = cfg
        .settings
        .stop_all_hotkey
        .as_ref()
        .filter(|h| !h.is_empty())
    {
        out.push((hk.clone(), Action::StopAll, "Soundboard: Stop all".into()));
    }
    out
}

fn apply_config(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap().clone();
    let s = &cfg.settings;
    let _ = state.audio.send(AudioCmd::Configure {
        output: s.output_device.clone(),
        monitor: s.monitor_device.clone(),
        master: s.master_volume,
        monitor_volume: s.monitor_volume,
    });
    let _ = state
        .deck_tx
        .send(DeckCmd::SetBrightness(s.deck_brightness));

    // Only re-register hotkeys when they actually changed: on Wayland every
    // re-bind may pop up a desktop confirmation dialog.
    let bindings = bindings_for(&cfg);
    let mut last = state.bindings.lock().unwrap();
    if last.as_ref() != Some(&bindings) {
        *last = Some(bindings.clone());
        drop(last);
        hotkeys::apply(app, bindings);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitialState {
    config: Config,
    deck: Option<DeckInfo>,
    deck_error: Option<String>,
    hotkeys: HotkeyStatus,
    platform: &'static str,
    wayland: bool,
    config_path: String,
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> InitialState {
    InitialState {
        config: state.config.read().unwrap().clone(),
        deck: state.deck.info.lock().unwrap().clone(),
        deck_error: state.deck.last_error.lock().unwrap().clone(),
        hotkeys: state.hotkeys.status(),
        platform: std::env::consts::OS,
        wayland: hotkeys::use_portal(),
        config_path: state.config_path.display().to_string(),
    }
}

#[tauri::command]
fn save_config(
    app: AppHandle,
    state: tauri::State<AppState>,
    config: Config,
) -> Result<(), String> {
    config
        .save(&state.config_path)
        .map_err(|e| format!("{e:#}"))?;
    *state.config.write().unwrap() = config;
    apply_config(&app);
    Ok(())
}

#[tauri::command]
fn list_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_devices()
}

#[tauri::command]
fn trigger(app: AppHandle, id: String) {
    trigger_button(&app, &id);
}

/// Plays an unsaved button (the editor's "Test" button).
#[tauri::command]
fn preview(app: AppHandle, state: tauri::State<AppState>, button: Button) {
    let settings = state.config.read().unwrap().settings.clone();
    play_button(&app, &button, &settings);
}

#[tauri::command]
fn stop_all_cmd(app: AppHandle) {
    stop_all(&app);
}

#[derive(Deserialize)]
struct KeyImage {
    key: u8,
    /// Base64 PNG; `None` clears the key.
    png: Option<String>,
}

#[tauri::command]
fn deck_set_images(state: tauri::State<AppState>, images: Vec<KeyImage>) -> Result<(), String> {
    let mut decoded = Vec::with_capacity(images.len());
    for img in images {
        let image = match img.png {
            Some(b64) => {
                let b64 = b64.split_once(',').map(|(_, d)| d).unwrap_or(&b64);
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| e.to_string())?;
                Some(image::load_from_memory(&bytes).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        decoded.push((img.key, image));
    }
    state
        .deck_tx
        .send(DeckCmd::SetImages(decoded))
        .map_err(|e| e.to_string())
}

/// Lets the web pages write to the backend log (WebView consoles aren't visible in release builds).
#[tauri::command]
fn ui_log(level: String, message: String) {
    match level.as_str() {
        "error" => log::error!(target: "ui", "{message}"),
        "warn" => log::warn!(target: "ui", "{message}"),
        _ => log::info!(target: "ui", "{message}"),
    }
}

/// Opens the desktop's own dialog for the portal shortcuts (Wayland only).
#[tauri::command]
fn configure_desktop_shortcuts(app: AppHandle) -> Result<(), String> {
    hotkeys::configure(&app)
}

#[tauri::command]
fn take_pending_video(state: tauri::State<AppState>) -> Option<VideoRequest> {
    state.pending_video.lock().unwrap().take()
}

#[tauri::command]
fn video_finished(app: AppHandle) {
    if let Some(win) = app.get_webview_window(VIDEO_WINDOW) {
        let _ = win.close();
    }
}

#[tauri::command]
async fn pick_files(app: AppHandle, multiple: bool, images: bool) -> Vec<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    dialog = if images {
        dialog.add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "svg"])
    } else {
        dialog
            .add_filter(
                "Audio & video",
                &[
                    "mp3", "wav", "ogg", "flac", "m4a", "aac", "mp4", "webm", "mkv", "mov", "m4v",
                    "ogv",
                ],
            )
            .add_filter("All files", &["*"])
    };
    let picked = if multiple {
        dialog.blocking_pick_files().unwrap_or_default()
    } else {
        dialog.blocking_pick_file().into_iter().collect()
    };
    picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Command line (also forwarded from later launches to the running instance)
// ---------------------------------------------------------------------------

const USAGE: &str = "Usage: soundboard [--trigger <button id or label>]... [--stop-all] [--show]

Without options, starts the soundboard (or focuses the running one).
--trigger and --stop-all are sent to the running instance, so you can bind
them to keys in your compositor/window manager, scripts or other tools.";

/// Returns true if the args contained an action.
fn handle_cli(app: &AppHandle, args: &[String]) -> bool {
    let mut acted = false;
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--trigger" | "-t" => {
                acted = true;
                let Some(name) = it.next() else { continue };
                let id = {
                    let state = app.state::<AppState>();
                    let cfg = state.config.read().unwrap();
                    cfg.button(name)
                        .or_else(|| {
                            cfg.buttons
                                .iter()
                                .find(|b| b.label.eq_ignore_ascii_case(name))
                        })
                        .map(|b| b.id.clone())
                };
                match id {
                    Some(id) => trigger_button(app, &id),
                    None => log::warn!("--trigger: no button named {name:?}"),
                }
            }
            "--stop-all" => {
                acted = true;
                stop_all(app);
            }
            "--show" => {}
            other => log::warn!("unknown argument {other:?}"),
        }
    }
    acted
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }

    tauri::Builder::default()
        // Must be the first plugin: later launches forward their args here and exit.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !handle_cli(app, &args) || args.iter().any(|a| a == "--show") {
                show_main(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(hotkeys::plugin())
        .setup(|app| {
            let handle = app.handle().clone();
            // SOUNDBOARD_CONFIG allows portable setups / multiple profiles.
            let config_path = match std::env::var_os("SOUNDBOARD_CONFIG") {
                Some(p) => PathBuf::from(p),
                None => app.path().app_config_dir()?.join("config.json"),
            };
            let config = Config::load(&config_path);
            log::info!("config: {}", config_path.display());

            let (deck_tx, deck_rx) = crossbeam_channel::unbounded();
            let brightness = config.settings.deck_brightness;
            app.manage(AppState {
                config: RwLock::new(config),
                config_path,
                audio: audio::spawn(handle.clone()),
                deck_tx,
                deck: DeckState::default(),
                hotkeys: Hotkeys::default(),
                bindings: Mutex::new(None),
                pending_video: Mutex::new(None),
                media: media_server::MediaServer::start()
                    .inspect_err(|e| log::error!("media server failed to start: {e}"))
                    .ok(),
            });
            deck::start(handle.clone(), deck_rx, brightness);
            apply_config(&handle);
            let args: Vec<String> = std::env::args().collect();
            handle_cli(&handle, &args);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window quits, even if the video window is open.
            if window.label() == "main" && matches!(event, WindowEvent::CloseRequested { .. }) {
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_config,
            list_audio_devices,
            trigger,
            preview,
            stop_all_cmd,
            deck_set_images,
            take_pending_video,
            ui_log,
            configure_desktop_shortcuts,
            video_finished,
            pick_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Soundboard");
}
