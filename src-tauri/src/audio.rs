//! Audio engine: a dedicated thread owns the output streams (they are not
//! `Send` on every platform) and receives commands over a channel.
//!
//! Every sound is played on the main output and, optionally, mirrored to a
//! "monitor" output so you can hear what you're sending to a virtual mic.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::PlayMode;

pub enum AudioCmd {
    Play {
        id: String,
        path: PathBuf,
        volume: f32,
        mode: PlayMode,
        looped: bool,
    },
    StopAll,
    Configure {
        output: Option<String>,
        monitor: Option<String>,
        master: f32,
        monitor_volume: f32,
    },
}

#[derive(Serialize, Clone)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// Lists output devices of the default host (PulseAudio/PipeWire on Linux
/// when available, otherwise ALSA; WASAPI on Windows).
pub fn list_devices() -> Vec<AudioDevice> {
    let host = rodio::cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return vec![];
    };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d
                .description()
                .map(|desc| desc.name().to_string())
                .unwrap_or_else(|_| d.to_string());
            Some(AudioDevice { id, name })
        })
        .collect()
}

fn open_output(device_id: Option<&str>) -> anyhow::Result<MixerDeviceSink> {
    let mut sink = match device_id {
        None => DeviceSinkBuilder::open_default_sink()?,
        Some(id) => {
            let host = rodio::cpal::default_host();
            let parsed = id
                .parse()
                .map_err(|e| anyhow!("invalid device id {id}: {e}"))?;
            let device = host
                .device_by_id(&parsed)
                .with_context(|| format!("audio device {id} not found"))?;
            DeviceSinkBuilder::from_device(device)?.open_sink_or_fallback()?
        }
    };
    sink.log_on_drop(false);
    Ok(sink)
}

struct Output {
    sink: MixerDeviceSink,
    gain: f32,
}

struct Instance {
    id: String,
    base_volume: f32,
    /// One player per output, index-aligned with `Engine::outputs`.
    players: Vec<Player>,
}

impl Instance {
    fn finished(&self) -> bool {
        self.players.iter().all(Player::empty)
    }
    fn stop(&self) {
        self.players.iter().for_each(Player::stop);
    }
}

struct Engine {
    app: AppHandle,
    outputs: Vec<Output>,
    master: f32,
    playing: Vec<Instance>,
    last_reported: BTreeSet<String>,
}

impl Engine {
    fn error(&self, msg: impl Into<String>) {
        let msg = msg.into();
        log::error!("{msg}");
        let _ = self.app.emit("app-error", msg);
    }

    fn configure(
        &mut self,
        output: Option<String>,
        monitor: Option<String>,
        master: f32,
        monitor_volume: f32,
    ) {
        self.master = master;
        // Stop everything: players are bound to the old mixers.
        self.playing.iter().for_each(Instance::stop);
        self.playing.clear();
        self.outputs.clear();

        match open_output(output.as_deref()) {
            Ok(sink) => self.outputs.push(Output { sink, gain: 1.0 }),
            Err(e) => {
                self.error(format!(
                    "Could not open output device: {e:#}. Falling back to default."
                ));
                match open_output(None) {
                    Ok(sink) => self.outputs.push(Output { sink, gain: 1.0 }),
                    Err(e) => self.error(format!("Could not open default audio device: {e:#}")),
                }
            }
        }
        if let Some(mon) = monitor.filter(|m| Some(m) != output.as_ref()) {
            match open_output(Some(&mon)) {
                Ok(sink) => self.outputs.push(Output {
                    sink,
                    gain: monitor_volume,
                }),
                Err(e) => self.error(format!("Could not open monitor device: {e:#}")),
            }
        }
    }

    fn play(&mut self, id: String, path: PathBuf, volume: f32, mode: PlayMode, looped: bool) {
        let running = self.playing.iter().any(|i| i.id == id && !i.finished());
        match mode {
            PlayMode::Overlap => {}
            PlayMode::Restart => self.stop_id(&id),
            PlayMode::Toggle if running => {
                self.stop_id(&id);
                return;
            }
            PlayMode::Toggle => {}
        }

        let mut players = Vec::with_capacity(self.outputs.len());
        for out in &self.outputs {
            // Each output needs its own decoder.
            let decoder = File::open(&path)
                .with_context(|| format!("opening {}", path.display()))
                .and_then(|f| {
                    Decoder::try_from(f).with_context(|| format!("decoding {}", path.display()))
                });
            let decoder = match decoder {
                Ok(d) => d,
                Err(e) => {
                    self.error(format!("{e:#}"));
                    return;
                }
            };
            let player = Player::connect_new(out.sink.mixer());
            player.set_volume(volume * self.master * out.gain);
            if looped {
                player.append(decoder.repeat_infinite());
            } else {
                player.append(decoder);
            }
            players.push(player);
        }
        self.playing.push(Instance {
            id,
            base_volume: volume,
            players,
        });
    }

    fn stop_id(&mut self, id: &str) {
        self.playing.retain(|i| {
            if i.id == id {
                i.stop();
                false
            } else {
                true
            }
        });
    }

    fn report(&mut self) {
        self.playing.retain(|i| !i.finished());
        let now: BTreeSet<String> = self.playing.iter().map(|i| i.id.clone()).collect();
        if now != self.last_reported {
            let _ = self
                .app
                .emit("audio-playing", now.iter().collect::<Vec<_>>());
            self.last_reported = now;
        }
    }

    fn set_master(&mut self, master: f32) {
        self.master = master;
        for inst in &self.playing {
            for (player, out) in inst.players.iter().zip(&self.outputs) {
                player.set_volume(inst.base_volume * master * out.gain);
            }
        }
    }
}

pub fn spawn(app: AppHandle) -> Sender<AudioCmd> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || run(app, rx))
        .expect("spawning audio thread");
    tx
}

fn run(app: AppHandle, rx: Receiver<AudioCmd>) {
    let mut engine = Engine {
        app,
        outputs: Vec::new(),
        master: 1.0,
        playing: Vec::new(),
        last_reported: BTreeSet::new(),
    };
    // Remember the last device config so pure volume changes don't reopen devices.
    let mut current_devices: Option<(Option<String>, Option<String>, u32)> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(AudioCmd::Play {
                id,
                path,
                volume,
                mode,
                looped,
            }) => engine.play(id, path, volume, mode, looped),
            Ok(AudioCmd::StopAll) => {
                engine.playing.iter().for_each(Instance::stop);
                engine.playing.clear();
            }
            Ok(AudioCmd::Configure {
                output,
                monitor,
                master,
                monitor_volume,
            }) => {
                let key = (output.clone(), monitor.clone(), monitor_volume.to_bits());
                if current_devices.as_ref() == Some(&key) {
                    engine.set_master(master);
                } else {
                    engine.configure(output, monitor, master, monitor_volume);
                    current_devices = Some(key);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        engine.report();
    }
}
