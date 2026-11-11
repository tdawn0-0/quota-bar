# Quota Bar

A macOS menu bar app, written in Rust with [NSH](https://nsh.dev), showing the
remaining quota of the local AI CLIs. One menu bar item, one popup panel, no
web view and no JavaScript runtime.

```
ag 16%  cc 78%  oc 93%
```

| Source | How it reads the quota |
| --- | --- |
| Antigravity (`ag`) | `agy -p "/quota" --output-format json`, plus `agy -p "/credits"` for the credit balance |
| Claude Code (`cc`) | `claude -p "/usage" --output-format json`, parsed from the CLI's own summary; remaining = 100 − used |
| OpenCode Go (`oc`) | `GET https://opencode.ai/zen/go/v1/usage` with the stored `oc_sk_…` key; `percent` is used, shown as remaining |

## Behaviour

- Clicking the menu bar item toggles the panel: a second click, a click in
  another app, or Escape closes it.
- Opening the panel never fetches: it shows the numbers already in memory.
  Data is refreshed at launch, every five minutes, and on the refresh button.

## Providers

Each provider can be shown or hidden from the tray's **显示** submenu. The
choice is saved to `config.json` in the app's data directory, which on macOS
is:

```
~/Library/Application Support/bar.jyeu.quota/config.json
```

`QUOTA_BAR_CONFIG` overrides the path. Every provider is on unless its entry
says otherwise, so a missing file (or a provider added by a later version)
starts enabled:

```json
{
  "providers": {
    "antigravity": true,
    "claude": true,
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
cargo test                         # collectors' parsing logic
```

Debug switches: `QUOTA_BAR_OPEN_PANEL=1` opens the panel at launch (add
`QUOTA_BAR_PIN_PANEL=1` to keep it open for screenshots, `QUOTA_BAR_PANEL_AT=x,y`
to place it, `QUOTA_BAR_REFRESH_SECS=15` for a faster refresh while testing).


cargo build --release
nsh package --target macos --run   # a signed .app bundle in .nsh/out
```

The packaged app is a menu bar app (`macos.activationPolicy: accessory`), so it
has no Dock icon. To start it at login, add the packaged app under System
Settings → General → Login Items.

The OpenCode key comes from `opencode auth export` at runtime; `OPENCODE_GO_KEY`
overrides it.

## Look

The panel is a rounded, translucent sheet with layered cards and a hairline top
edge. NSH 0.5's macOS backend paints an opaque window background, so the
translucency is drawn in-app rather than as an `NSVisualEffectView` blur; a real
vibrancy material would need the platform layer.

## Layout

```
src/lib.rs     tray item, popup panel, refresh policy
src/config.rs  which providers are on, read from and saved to config.json
src/quota.rs   the providers, one collector each, parsing and formatting
src/main.rs    cargo run entry (AppKit backend)
nsh.json       app metadata, accessory policy, hidden main window
```
