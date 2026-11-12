# Development

## Build and test

```sh
cargo run                          # development build, logs in the terminal
cargo test                         # parsing and config tests
cargo build --release
nsh package --target macos --run   # a signed .app bundle in .nsh/out
```

The app is a Rust application on NSH. `src/main.rs` starts the native backend
directly (AppKit on macOS, GTK 4 on Linux); `nsh.json` supplies the app id,
accessory activation policy and the hidden main window.

## Layout

```
src/lib.rs        tray item, popup panel, refresh policy
src/config.rs     which providers are on, saved to config.json
src/quota.rs      the providers, one collector each, parsing and formatting
src/main.rs       cargo run entry (AppKit / GTK 4 backend)
nsh.json          app metadata, accessory policy, hidden main window
docs/             screenshots and guides
```

## Debug switches

| Variable | Effect |
| --- | --- |
| `QUOTA_BAR_OPEN_PANEL=1` | Open the panel at launch |
| `QUOTA_BAR_PIN_PANEL=1` | Keep the panel open when it loses focus |
| `QUOTA_BAR_OPEN_DELAY_MS=30000` | Wait before opening the panel, so the first fetch can land |
| `QUOTA_BAR_PANEL_AT=x,y` | Place the panel at a point (macOS) |
| `QUOTA_BAR_REFRESH_SECS=15` | Faster background refresh (minimum 5) |
| `QUOTA_BAR_CONFIG=path` | Use another config file |

Provider overrides are listed in [providers.md](providers.md):
`OPENCODE_GO_KEY`, `CODEX_HOME`, `CODEX_BIN`, `GROK_HOME` and
`CURSOR_STATE_VSCDB`.

## Screenshots

The screenshots in this repository were taken from the running app with real
data. To reproduce a clean panel shot:

```sh
QUOTA_BAR_CONFIG=/tmp/quota.json \
QUOTA_BAR_OPEN_PANEL=1 \
QUOTA_BAR_PIN_PANEL=1 \
QUOTA_BAR_OPEN_DELAY_MS=30000 \
QUOTA_BAR_PANEL_AT=120,120 \
cargo run
```

Everything is fetched at launch; opening late means the cards are already
filled when the panel appears. With the panel pinned, capture the window rather
than the whole screen so the transparent rounded sheet is preserved:

```sh
screencapture -x -o -l <window-id> panel.png
```

The window's title is `Quota Bar`; on a 2× display the capture is already
Retina-resolution. A screenshot cropped from the screen instead shows the
desktop through the translucent sheet.

## Linux

The Linux backend uses GTK 4, GStreamer and Pango. On Debian or Ubuntu:

```sh
sudo apt install build-essential pkg-config libgtk-4-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  libpango1.0-dev libcairo2-dev libgdk-pixbuf-2.0-dev libglib2.0-dev \
  libxkbcommon-dev libwayland-dev libx11-dev libxi-dev libxcursor-dev sqlite3
cargo run
```

A StatusNotifierItem host is required. KDE, Xfce and most desktops have one;
GNOME needs the AppIndicator extension. GTK 4 cannot place a window next to the
tray icon, so the window manager chooses where the panel opens. Install
`fonts-noto-cjk` if the Chinese labels show as boxes.

## Resource budget

The point of writing this in Rust is that an idle app should be close to free.

- One timer, no busy loop. Data is fetched at launch, every five minutes, and
  on demand. An idle app sleeps at 0% CPU.
- Batches run concurrently, each CLI one process at a time, with no shell.
- One window, created only while the panel is open. The main window stays
  hidden (`nsh.json` `window.defaultVisible: false`) and only gives the tray an
  owner.
- The popup closes on blur, so its memory is released after use.
