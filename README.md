# Quota Bar

A menu bar app for macOS and a tray app for Linux, written in Rust with
[NSH](https://nsh.dev), showing the remaining quota of the local AI CLIs. One
tray item, one popup panel, no web view and no JavaScript runtime.

```
ag 16%  cc 78%  cx 75%  gk 83%  oc 93%
```

| Source | How it reads the quota |
| --- | --- |
| Antigravity (`ag`) | `agy -p "/quota" --output-format json`, plus `agy -p "/credits"` for the credit balance |
| Claude Code (`cc`) | `claude -p "/usage" --output-format json`, parsed from the CLI's own summary; remaining = 100 − used |
| Codex (`cx`) | `GET https://chatgpt.com/backend-api/wham/usage` with the ChatGPT login in `~/.codex/auth.json`; a stale login falls back to `codex app-server`, which refreshes it without a model turn |
| Grok Build (`gk`) | `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits` with the OIDC login in `~/.grok/auth.json`; an expired token is refreshed and written back |
| OpenCode Go (`oc`) | `GET https://opencode.ai/zen/go/v1/usage` with the stored `oc_sk_…` key; `percent` is used, shown as remaining |

## Behaviour

- On macOS, clicking the menu bar item toggles the panel: a second click, a
  click in another app, or Escape closes it. Under GNOME, clicking the tray
  label opens its menu; its **面板** item opens the panel.
- Opening the panel never fetches: it shows the numbers already in memory.
  Data is refreshed at launch, every five minutes, and on the refresh button.

## Providers

Each provider can be shown or hidden from the tray's **显示** submenu. The
choice is saved to `config.json` in the app's data directory:

```
~/Library/Application Support/bar.jyeu.quota/config.json   # macOS
~/.local/share/bar.jyeu.quota/config.json                  # Linux
```

On Linux that is `$XDG_DATA_HOME/bar.jyeu.quota/config.json` when
`XDG_DATA_HOME` is set. `QUOTA_BAR_CONFIG` overrides the path on either
platform. Every provider is on unless its entry says otherwise, so a missing
file (or a provider added by a later version) starts enabled:

```json
{
  "providers": {
    "antigravity": true,
    "claude": true,
    "codex": true,
    "grok": true,
    "opencode": false
  }
}
```

An entry can also be an object, which leaves room for provider settings later:

```json
"claude": { "enabled": false }
```

Edits to the file take effect on the next refresh (the refresh button or menu
item, otherwise within five minutes). Hidden providers are neither fetched nor
shown in the menu bar and panel.

Codex never starts a model turn: its quota comes from the usage endpoint with
the stored ChatGPT login, and only a stale login falls back to the local app
server to refresh it.

`grok usage --json` currently refuses outside the TUI ("Run /usage inside grok
instead"), so Grok Build reads the same billing endpoint its `/usage` view uses.
It reuses the OIDC login in `~/.grok/auth.json` (or `$GROK_HOME/auth.json`),
refreshes an expired access token with the stored refresh token, and writes the
new one back so the CLI stays signed in. No model turn is made.

To add a provider, add its collector in `src/quota.rs` and one entry to
`PROVIDERS` there; the tray menu, config file and footer pick it up.

## Resource budget

The point of writing this in Rust is that an idle app should be close to free.

- **One timer, no busy loop.** Data is fetched at launch, every five minutes,
  and when the refresh control asks. An idle app sleeps at 0% CPU.
- Batches run concurrently, each one CLI process at a time, with no shell.
- One window, created only while the panel is open; the main window starts
  hidden (`nsh.json` `window.defaultVisible: false`) and never shows.
- The popup closes on blur, so the window's memory is released after use.

## Build and run

```sh
cargo run                          # development build, logs in the terminal
cargo test                         # providers' parsing logic
cargo build --release
nsh package --target macos --run   # a signed .app bundle in .nsh/out
```

Windows is not supported.

### Linux

The Linux backend uses GTK 4, GStreamer and Pango. On Debian or Ubuntu:

```sh
sudo apt install build-essential pkg-config libgtk-4-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  libpango1.0-dev libcairo2-dev libgdk-pixbuf-2.0-dev libglib2.0-dev \
  libxkbcommon-dev libwayland-dev libx11-dev libxi-dev libxcursor-dev
cargo run
```

The panel's labels are Chinese; install `fonts-noto-cjk` (or another CJK font)
if they show as boxes.

A tray host for StatusNotifierItem is required. KDE, Xfce and most other
desktops have one; GNOME needs the AppIndicator extension
(`gnome-shell-extension-appindicator`). The label appears a moment after
launch: GNOME's extension can miss the first one, so the app sends it again.

GTK 4 has no API to place a window next to the tray icon, so the window manager
chooses where the panel opens. The tray label and the panel itself show the
same numbers as on macOS.

Debug switches: `QUOTA_BAR_OPEN_PANEL=1` opens the panel at launch (add
`QUOTA_BAR_PIN_PANEL=1` to keep it open for screenshots, `QUOTA_BAR_PANEL_AT=x,y`
to place it, `QUOTA_BAR_REFRESH_SECS=15` for a faster refresh while testing).
The panel position is macOS-only.

The packaged macOS app is a menu bar app (`macos.activationPolicy: accessory`),
so it has no Dock icon. To start it at login, add the packaged app under System
Settings → General → Login Items. On Linux, start `quota-bar` from your
desktop's autostart settings.

The OpenCode key comes from `opencode auth export` at runtime; `OPENCODE_GO_KEY`
overrides it.

## Look

The panel is a rounded, translucent sheet with layered cards and a hairline top
edge. The window itself stays transparent and the sheet's own fill draws the
material, with its content clipped to the rounded shape. NSH 0.5 puts macOS's
popover material behind the whole rectangular window, which would show as a
square layer outside the corners, so it is not used. The Linux GTK backend draws
the same sheet; its own translucent fill carries the look where GTK has no
popover material.

## Layout

```
src/lib.rs     tray item, popup panel, refresh policy
src/config.rs  which providers are on, read from and saved to config.json
src/quota.rs   the providers, one collector each, parsing and formatting
src/main.rs    cargo run entry (AppKit / GTK 4 backend)
nsh.json       app metadata, accessory policy, hidden main window
```
