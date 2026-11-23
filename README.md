# Soundboard

A cross-platform (Linux + Windows) soundboard: play sounds and videos with
**global hotkeys**, mouse clicks or an **Elgato Stream Deck**. It uses only
open-source libraries and needs neither the Elgato software nor an account.

Built with [Tauri 2](https://tauri.app) (Rust backend, plain HTML/JS UI).

## Features

- Grid of buttons for audio (mp3, wav, ogg/vorbis, flac, m4a/aac) and video
  (mp4, webm, mkv, mov, …). Drag files onto the window to add them, and drag
  tiles to reorder them.
- Per button: label, color, icon image, volume, loop, and what a press does
  while the sound is still playing (restart, overlap, or stop/toggle).
- **Global hotkeys** that work while other apps are focused:
  - Windows and Linux/X11: native key grabs.
  - Linux/Wayland: the XDG *GlobalShortcuts* portal (KDE Plasma 6, GNOME 48+,
    Hyprland, …). The first time, your desktop asks you to confirm the
    shortcuts. You can rebind them later under Settings → *Configure in
    desktop…* or in your desktop's shortcut settings.
- **Stream Deck** via [`elgato-streamdeck`](https://github.com/OpenActionAPI/rust-elgato-streamdeck)
  (hidapi). Hot-plug support. Keys show the button's color, icon and label,
  and get a white frame while playing. One key can act as "Stop all".
  Supported: every model with LCD keys that `elgato-streamdeck` knows:
  Original/V2/MK.2, Mini/Mini MK.2, XL/XL V2, Plus, Plus XL, Neo, and the
  module variants. The Pedal has no displays and isn't supported yet.
- **Output device selection** plus an optional **monitor output**: send
  sounds to a virtual mic for Discord/OBS and hear them on your headphones at
  the same time.
- Videos open in a separate black window (fullscreen and always-on-top are
  optional) that you can capture in OBS or put on a second screen.
- **Command line**: `soundboard --trigger "Airhorn"` / `soundboard --stop-all`
  sends the command to the running instance. Use this to bind keys in
  compositors without the portal (Sway, …), in scripts, or in macro tools.

## Building

Prerequisites: Rust (stable) and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

**Linux** also needs the WebKitGTK/GStreamer development packages:

| Distro | Packages |
|---|---|
| Arch / CachyOS | `webkit2gtk-4.1 gst-plugins-good gst-plugins-bad gst-libav` |
| Debian / Ubuntu | `libwebkit2gtk-4.1-dev libudev-dev libasound2-dev librsvg2-dev gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav` |
| Fedora | `webkit2gtk4.1-devel systemd-devel alsa-lib-devel gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-libav` |

The GStreamer plugins are what WebKitGTK uses to play videos. Without
`gst-libav`/openh264, H.264 videos won't play.

**Windows** needs the MSVC build tools and WebView2. WebView2 is preinstalled
on Windows 10/11.

```sh
cargo tauri dev      # run with auto-rebuild on changes
cargo tauri build    # release build + installers in src-tauri/target/release/bundle/

# Without the Tauri CLI (no file watching, no installers):
cd src-tauri && cargo run
```

### Cross-compiling for Windows from Linux

Needs LLVM (`clang`, `lld`, `llvm-rc`) and `cargo install cargo-xwin --locked`.
`cargo-xwin` downloads the Microsoft CRT and Windows SDK, which means you
accept Microsoft's license for them.

```sh
# Portable single .exe (needs only the WebView2 runtime, built into Windows 10/11):
cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --no-bundle
# -> src-tauri/target/x86_64-pc-windows-msvc/release/soundboard.exe

# NSIS installer (additionally needs `makensis`, e.g. the `nsis` package):
cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
```

MSI installers can only be built on Windows.

The GitHub Actions workflow in `.github/workflows/build.yml` builds the
Linux (deb, rpm, AppImage) and Windows (NSIS, MSI) packages.

## Stream Deck setup

- **Linux**: give your user access to the device. Install the udev rule once,
  then unplug and replug the deck:
  ```sh
  sudo cp linux/70-streamdeck.rules /etc/udev/rules.d/
  sudo udevadm control --reload-rules && sudo udevadm trigger
  ```
  The `.deb`/`.rpm` packages install this rule automatically.
- **Windows**: quit the official Elgato Stream Deck app first. It holds the
  device exclusively.

Assign keys in a button's editor (*Stream Deck key*). The "Stop all" key is
set in Settings.

## Routing sounds into voice chat (virtual mic)

Linux (PipeWire/PulseAudio):

```sh
# A virtual sink that apps like Discord can use as a microphone:
pactl load-module module-null-sink sink_name=soundboard sink_properties=device.description=Soundboard
pactl load-module module-remap-source master=soundboard.monitor source_name=soundboard_mic source_properties=device.description=Soundboard-Mic
```

Then choose **Soundboard** as *Output device* and your headphones as
*Monitor device* in Settings. In Discord, select **Soundboard-Mic** as your
input (or mix it with your real mic using `module-loopback`, qpwgraph, or
Helvum).

Windows: install a virtual audio cable (e.g. VB-CABLE), choose *CABLE Input*
as the output device and your headphones as the monitor.

Note: video audio is played by the WebView on the system default device.

## Configuration

Config is stored in JSON at:

- Linux: `~/.config/dev.soundboard.Soundboard/config.json`
- Windows: `%APPDATA%\dev.soundboard.Soundboard\config.json`

Set `SOUNDBOARD_CONFIG=/path/to/profile.json` to use a different file
(portable setups, multiple profiles).

Hotkeys are stored as `Modifiers+Code` using DOM `KeyboardEvent.code` names,
e.g. `Ctrl+Alt+Digit1`, `Shift+F13`, `Numpad7`.

## Architecture

```
ui/                    plain HTML/CSS/JS (no bundler), uses window.__TAURI__
  index.html, app.js   main window: grid, editor, settings, Stream Deck key rendering
  video.html, video.js video player window
src-tauri/src/
  main.rs              app state, commands, triggering, CLI, video window
  config.rs            config model + atomic JSON persistence
  audio.rs             rodio/cpal engine thread (main + monitor output)
  hotkeys.rs           global-shortcut plugin / XDG portal backends
  deck.rs              Stream Deck thread (hot-plug, input, key images)
  media_server.rs      loopback HTTP server with range support for <video>
```

Why the media server? WebKitGTK can't stream `<video>` from Tauri's custom
`asset://` protocol. Videos are served from `127.0.0.1` instead, behind
random per-file tokens. Only files assigned to buttons can be fetched.

Key images for the Stream Deck are drawn in the UI on a `<canvas>`, so they
use the same fonts as the app. The backend scales them to the device's
native format.

## Known limitations

- On Linux the audio stream appears in pavucontrol as `cpal-pulseaudio-<pid>`.
  That name is hard-coded in cpal, so pick the device in the app's settings
  instead of routing the stream per app.
- Wayland desktops without the GlobalShortcuts portal (e.g. Sway) fall back
  to in-window hotkeys. Use `soundboard --trigger …` bindings there.
