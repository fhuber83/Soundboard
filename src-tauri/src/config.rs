//! Persistent configuration (buttons + settings), stored as JSON in the
//! platform config directory (e.g. `~/.config/dev.soundboard.Soundboard/config.json`
//! or `%APPDATA%\dev.soundboard.Soundboard\config.json`).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Audio,
    Video,
}

/// What happens when a button is triggered while its sound is still playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlayMode {
    /// Start another instance on top of the running one.
    Overlap,
    /// Stop the running instance and start from the beginning.
    #[default]
    Restart,
    /// Stop if playing, otherwise start.
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Button {
    pub id: String,
    pub label: String,
    pub path: String,
    pub kind: MediaKind,
    /// CSS color used for the tile and the Stream Deck key background.
    pub color: String,
    /// Optional image shown on the tile / Stream Deck key.
    pub icon: Option<String>,
    /// 0.0 ..= 1.0
    pub volume: f32,
    pub mode: PlayMode,
    pub looped: bool,
    /// Accelerator, e.g. `Ctrl+Shift+KeyA` (modifiers + DOM `KeyboardEvent.code`).
    pub hotkey: Option<String>,
    /// Zero-based Stream Deck key index.
    pub deck_key: Option<u8>,
}

impl Default for Button {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            path: String::new(),
            kind: MediaKind::Audio,
            color: "#3b82f6".into(),
            icon: None,
            volume: 1.0,
            mode: PlayMode::Restart,
            looped: false,
            hotkey: None,
            deck_key: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub master_volume: f32,
    /// cpal device id (`host:device`), `None` = system default.
    pub output_device: Option<String>,
    /// Optional second output so you can hear what you send to e.g. a virtual mic.
    pub monitor_device: Option<String>,
    pub monitor_volume: f32,
    pub stop_all_hotkey: Option<String>,
    pub deck_stop_all_key: Option<u8>,
    pub deck_brightness: u8,
    pub video_fullscreen: bool,
    pub video_always_on_top: bool,
    pub video_close_on_end: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            master_volume: 0.8,
            output_device: None,
            monitor_device: None,
            monitor_volume: 0.8,
            stop_all_hotkey: Some("Ctrl+Shift+Period".into()),
            deck_stop_all_key: None,
            deck_brightness: 70,
            video_fullscreen: false,
            video_always_on_top: true,
            video_close_on_end: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub buttons: Vec<Button>,
    pub settings: Settings,
}

impl Config {
    pub fn button(&self, id: &str) -> Option<&Button> {
        self.buttons.iter().find(|b| b.id == id)
    }

    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::error!(
                    "config at {} is invalid ({e}); starting with defaults",
                    path.display()
                );
                // Keep the broken file around instead of silently overwriting it.
                let _ = std::fs::copy(path, path.with_extension("json.broken"));
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write atomically so a crash never leaves a half-written config.
        let tmp: PathBuf = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path).context("replacing config file")?;
        Ok(())
    }
}
