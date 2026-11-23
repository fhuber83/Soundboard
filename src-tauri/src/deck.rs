//! Elgato Stream Deck support via the open-source `elgato-streamdeck` crate
//! (hidapi). A background thread hot-plugs the first device it finds,
//! forwards key presses and draws key images.
//!
//! Key images are rendered by the web UI (canvas → PNG) so labels use the
//! same fonts/colors as the app; this module only scales them to the
//! device's native format.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use elgato_streamdeck::{
    StreamDeck, StreamDeckInput, info::Kind, list_devices, new_hidapi, refresh_device_list,
};
use image::DynamicImage;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

pub enum DeckCmd {
    SetImages(Vec<(u8, Option<DynamicImage>)>),
    SetBrightness(u8),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeckInfo {
    pub model: String,
    pub serial: String,
    pub key_count: u8,
    pub rows: u8,
    pub columns: u8,
    pub image_size: (usize, usize),
}

#[derive(Default)]
pub struct DeckState {
    pub info: Mutex<Option<DeckInfo>>,
    pub last_error: Mutex<Option<String>>,
}

/// Starts the device thread. `AppState` must already be managed.
pub fn start(app: AppHandle, rx: Receiver<DeckCmd>, brightness: u8) {
    std::thread::Builder::new()
        .name("streamdeck".into())
        .spawn(move || run(app, rx, brightness))
        .expect("spawning stream deck thread");
}

fn set_error(app: &AppHandle, err: Option<String>) {
    let state = app.state::<crate::AppState>();
    let mut last = state.deck.last_error.lock().unwrap();
    if *last != err {
        if let Some(e) = &err {
            log::warn!("stream deck: {e}");
        }
        *last = err.clone();
        let _ = app.emit("deck-error", err);
    }
}

fn set_info(app: &AppHandle, info: Option<DeckInfo>) {
    *app.state::<crate::AppState>().deck.info.lock().unwrap() = info.clone();
    let _ = app.emit("deck-changed", info);
}

fn run(app: AppHandle, rx: Receiver<DeckCmd>, mut brightness: u8) {
    let mut hid = loop {
        match new_hidapi() {
            Ok(h) => break h,
            Err(e) => {
                set_error(&app, Some(format!("HID API unavailable: {e}")));
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };

    loop {
        // --- Wait for a device -------------------------------------------
        let _ = refresh_device_list(&mut hid);
        let Some((kind, serial)) = list_devices(&hid).into_iter().find(|(k, _)| k.is_visual())
        else {
            // Keep the channel drained while idle, remembering brightness.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match rx.recv_deadline(deadline) {
                    Ok(DeckCmd::SetBrightness(b)) => brightness = b,
                    Ok(DeckCmd::SetImages(_)) => {}
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            continue;
        };

        let deck = match StreamDeck::connect(&hid, kind, &serial) {
            Ok(d) => d,
            Err(e) => {
                let hint = if cfg!(target_os = "linux") {
                    " — install the udev rule (see README: linux/70-streamdeck.rules) and replug the device"
                } else {
                    " — close the Elgato Stream Deck software, it locks the device"
                };
                set_error(
                    &app,
                    Some(format!("Found {kind:?} but could not open it: {e}{hint}")),
                );
                std::thread::sleep(Duration::from_secs(3));
                continue;
            }
        };
        set_error(&app, None);
        let _ = deck.reset();
        let _ = deck.set_brightness(brightness);
        log::info!("stream deck connected: {kind:?} ({serial})");
        set_info(&app, Some(info_for(kind, serial)));

        // --- Device loop -------------------------------------------------
        let mut pressed = vec![false; kind.key_count() as usize];
        let result: Result<(), String> = 'device: loop {
            loop {
                match rx.try_recv() {
                    Ok(DeckCmd::SetBrightness(b)) => {
                        brightness = b;
                        if let Err(e) = deck.set_brightness(b) {
                            break 'device Err(e.to_string());
                        }
                    }
                    Ok(DeckCmd::SetImages(images)) => {
                        for (key, img) in images {
                            if key >= kind.key_count() {
                                continue;
                            }
                            let r = match img {
                                Some(img) => deck.set_button_image(key, img),
                                None => deck.clear_button_image(key),
                            };
                            if let Err(e) = r {
                                log::warn!("stream deck image for key {key}: {e}");
                            }
                        }
                        if let Err(e) = deck.flush() {
                            break 'device Err(e.to_string());
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        let _ = deck.reset();
                        return;
                    }
                }
            }

            match deck.read_input(Some(Duration::from_millis(40))) {
                Ok(StreamDeckInput::ButtonStateChange(states)) => {
                    for (key, (&now, was)) in states.iter().zip(pressed.iter_mut()).enumerate() {
                        if now && !*was {
                            crate::deck_key_pressed(&app, key as u8);
                        }
                        *was = now;
                    }
                }
                Ok(_) => {}
                Err(e) => break 'device Err(e.to_string()),
            }
        };

        if let Err(e) = result {
            log::info!("stream deck disconnected: {e}");
        }
        set_info(&app, None);
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn info_for(kind: Kind, serial: String) -> DeckInfo {
    DeckInfo {
        model: format!("{kind:?}"),
        serial,
        key_count: kind.key_count(),
        rows: kind.row_count(),
        columns: kind.column_count(),
        image_size: kind.key_image_format().size,
    }
}
