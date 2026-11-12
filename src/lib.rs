//! Quota Bar: a menu bar app showing the remaining quota of the local AI CLIs.
//!
//! Resource budget: no busy loop. Data is fetched at launch, every five
//! minutes, whenever the refresh control or menu item asks, and when a
//! provider is turned back on; opening the panel only reads what is already in
//! memory. On macOS the popup closes on blur; on Linux it stays mapped so
//! the compositor preserves the position chosen by dragging it.

mod config;
mod i18n;
#[cfg(target_os = "macos")]
mod macos_tray;
mod quota;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use nsh_base::theme;
use nsh_core::color::{rgb, rgba};
use nsh_core::desktop::{
    ResizeEdge, TrayTitleRun, WindowAttributes, WindowKind, WindowLevel, WindowPosition,
};
use nsh_core::element::desktop::{
    MenuItem, WindowRef, current_window, open_window, quit, set_tray, tray, window,
};
use nsh_core::geometry::{Bounds, Points, point, px, relative, size};
use nsh_core::notifications::Notification;
#[cfg(target_os = "macos")]
use nsh_core::platform::Lifecycle;
use nsh_core::prelude::*;
use nsh_core::time::unix_ms;

use i18n::tr;
use quota::Source;

const PANEL_WIDTH: f32 = 344.0;
const PANEL_MAX_HEIGHT: f32 = 656.0;
/// The bounds a resize may take the panel to.
const PANEL_MIN_WIDTH: f32 = 280.0;
const PANEL_MIN_HEIGHT: f32 = 220.0;
const PANEL_MAX_SIZE: f32 = 1200.0;

const BLUE: u32 = 0x0a84ff;
const AMBER: u32 = 0xff9f0a;
const RED: u32 = 0xff3b30;

/// The menu bar item's picture. On macOS its title carries the item, so a
/// 1×1 transparent icon leaves it text-only; Linux needs a visible icon.
#[cfg(target_os = "macos")]
const TRAY_ICON: &str = "data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 width=%271%27 height=%271%27/%3E";
#[cfg(target_os = "linux")]
const TRAY_ICON: &str = "data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 viewBox=%270 0 16 16%27%3E%3Crect x=%272.5%27 y=%279%27 width=%272.5%27 height=%275%27 rx=%271%27/%3E%3Crect x=%276.75%27 y=%275.5%27 width=%272.5%27 height=%278.5%27 rx=%271%27/%3E%3Crect x=%2711%27 y=%272%27 width=%272.5%27 height=%2712%27 rx=%271%27/%3E%3C/svg%3E";

/// The single-instance socket, made before the app starts.
#[cfg(unix)]
static INSTANCE: std::sync::Mutex<Option<std::os::unix::net::UnixListener>> =
    std::sync::Mutex::new(None);

thread_local! {
    /// When the background refresh last ran, to notice a slept machine.
    static LAST_TICK: Cell<f64> = const { Cell::new(0.0) };
    /// The open panel, so a second tray click closes it.
    static PANEL: RefCell<Option<WindowRef>> = const { RefCell::new(None) };
    /// When it opened, so activation that arrives late does not close it.
    static OPENED_AT: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// The first size the panel reports is the one it opened at; only a
    /// later one is a resize worth keeping.
    static PANEL_SIZE_SEEN: Cell<bool> = const { Cell::new(false) };
    static PANEL_SIZE_LAST: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    /// The newest resize wins: an older debounce does not save.
    static PANEL_SIZE_SEQ: Cell<u64> = const { Cell::new(0) };
    /// What the window was asked to open at, and the window manager's border
    /// around the content (a GTK window's state leaves it out).
    static PANEL_REQUEST: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
    static PANEL_BORDER: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
}

/// The signals the app shares between its tray handler and its panel.
#[derive(Clone, Copy)]
struct Store {
    sources: Signal<Vec<Source>>,
    updated: Signal<f64>,
    busy: Signal<bool>,
    /// Set when a refresh was asked for while one was running.
    pending: Signal<bool>,
    config: Signal<config::Config>,
    /// A pinned panel stays open when another app comes forward.
    pinned: Signal<bool>,
    /// Bumped whenever `sources` changes. A window's content is made once,
    /// so the open panel's cards follow this instead of the list itself.
    revision: Signal<u64>,
    /// Whether the panel shows the settings instead of the cards.
    settings: Signal<bool>,
    /// A newer release, when the update check found one.
    update: Signal<Option<String>>,
    /// Whether the login item is installed.
    autostart: Signal<bool>,
    /// Linux tray hosts can miss a label set before they start; the label is
    /// sent once more after startup.
    #[cfg(target_os = "linux")]
    tray_ready: Signal<bool>,
}

#[native_module("quota-bar/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

#[component]
pub fn App() -> impl IntoElement {
    let config = load_config();
    i18n::choose(match config.language() {
        config::Language::Zh => Some(i18n::Lang::Zh),
        config::Language::En => Some(i18n::Lang::En),
        config::Language::Auto => None,
    });
    let store = Store {
        sources: signal(Vec::<Source>::new()),
        updated: signal(0.0_f64),
        busy: signal(false),
        pending: signal(false),
        config: signal(config),
        pinned: signal(false),
        revision: signal(0_u64),
        settings: signal(false),
        update: signal(None::<String>),
        autostart: signal(autostart_enabled()),
        #[cfg(target_os = "linux")]
        tray_ready: signal(false),
    };

    // One fetch at launch, then only on demand.
    refresh(store);

    // A second launch opens the running instance's panel instead of adding a
    // second tray item (where the platform reports it).
    spawn(async move {
        loop {
            let _launch = next_second_instance().await;
            show_panel(store);
        }
    });

    // And where it doesn't (a second copy of the binary exits on the lock),
    // its connection is the request to show the panel.
    #[cfg(unix)]
    if let Some(listener) = instance_listener() {
        let listener = std::sync::Arc::new(listener);
        spawn(async move {
            loop {
                let listener = std::sync::Arc::clone(&listener);
                match blocking(move || listener.accept()).await {
                    Ok(_) => show_panel(store),
                    Err(_) => break,
                }
            }
        });
    }

    // A first run has no config and no window: show the panel once, so
    // double-clicking the app does not look like nothing happened.
    if !config::Config::exists() && std::env::var_os("QUOTA_BAR_OPEN_PANEL").is_none() {
        set_timeout(Duration::from_millis(1200), move || show_panel(store));
    }

    // A newer release matters most when an upstream endpoint changes.
    if std::env::var_os("QUOTA_BAR_NO_UPDATE").is_none() {
        set_timeout(Duration::from_secs(8), move || {
            spawn(async move {
                if let Ok(tag) = quota::latest_release().await {
                    if quota::is_newer(&tag, &app_version()) {
                        store.update.set(Some(tag));
                    }
                }
            });
        });
    }

    effect(move || set_tray_menu(store));

    // GNOME's AppIndicator extension can miss the first label; sending an
    // empty one first and the real one shortly after makes it show.
    #[cfg(target_os = "linux")]
    set_timeout(Duration::from_secs(2), move || store.tray_ready.set(true));

    // Switching to another app closes the panel. The delay lets the
    // activation that follows a tray click land first. On Linux the app
    // reports Background while its hidden main window is unmapped, so there
    // the platform's `closeOnBlur` does this instead.
    #[cfg(target_os = "macos")]
    effect(move || {
        if lifecycle() == Lifecycle::Active || store.pinned.get() {
            return;
        }
        set_timeout(Duration::from_millis(400), move || {
            let age = unix_ms() - OPENED_AT.with(|at| at.get());
            if lifecycle() != Lifecycle::Active && age > 900.0 && !store.pinned.get() {
                close_panel(store);
            }
        });
    });

    // Steady background refresh; opening the panel only reads what is there.
    let interval = std::env::var("QUOTA_BAR_REFRESH_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs >= 5)
        .unwrap_or(300);
    LAST_TICK.with(|tick| tick.set(unix_ms()));
    set_interval(Duration::from_secs(interval), move || {
        // A long gap means the machine slept (or the app was suspended):
        // give Wi-Fi a moment before asking the network again.
        let now = unix_ms();
        let gap = now - LAST_TICK.with(|tick| tick.replace(now));
        if gap > (interval as f64 * 1.5 + 15.0) * 1000.0 {
            set_timeout(Duration::from_secs(4), move || refresh(store));
        } else {
            refresh(store);
        }
    });

    // `QUOTA_BAR_OPEN_PANEL=1` opens the panel at launch, for screenshots;
    // `QUOTA_BAR_OPEN_DELAY_MS` waits for the first fetch before opening it.
    if std::env::var_os("QUOTA_BAR_OPEN_PANEL").is_some() {
        let pinned = std::env::var_os("QUOTA_BAR_PIN_PANEL").is_some();
        let delay = std::env::var("QUOTA_BAR_OPEN_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(700);
        set_timeout(Duration::from_millis(delay), move || {
            toggle_panel(store, None, pinned)
        });
    }

    // The main window stays hidden (`nsh.json` `defaultVisible: false`); it
    // only exists so the tray has an owner. The panel is a popup of its own.
    view().w_full().h_full()
}

/// Ends this process when another copy already owns the tray item: two
/// copies would put two status items in the menu bar. A Unix socket in the
/// data directory is the lock; the first process keeps it for its lifetime.
pub fn ensure_single_instance() {
    #[cfg(unix)]
    {
        use std::os::unix::net::{UnixListener, UnixStream};
        let Some(dir) = config::dir() else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("instance.sock");
        if UnixStream::connect(&path).is_ok() {
            eprintln!("[quota-bar] already running");
            std::process::exit(0);
        }
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                // Kept for the process's life; the app takes it to open its
                // panel when someone launches it again.
                if let Ok(mut slot) = INSTANCE.lock() {
                    *slot = Some(listener);
                }
            }
            Err(error) => eprintln!("[quota-bar] instance lock: {error}"),
        }
    }
}

/// The lock's socket, for the app to accept later launches from.
#[cfg(unix)]
pub fn instance_listener() -> Option<std::os::unix::net::UnixListener> {
    INSTANCE.lock().ok().and_then(|mut slot| slot.take())
}

/// Reads the config, then hides providers this machine does not have unless
/// the file names them explicitly.
fn load_config() -> config::Config {
    let mut config = config::Config::load().unwrap_or_else(|error| {
        eprintln!("[quota-bar] config: {error}; using defaults");
        config::Config::default()
    });
    detect_providers(&mut config);
    if std::env::var_os("QUOTA_BAR_DEBUG").is_some() {
        let on: Vec<&str> = quota::PROVIDERS
            .iter()
            .filter(|provider| config.enabled(provider.id))
            .map(|provider| provider.short)
            .collect();
        eprintln!("[quota-bar] providers on: {}", on.join(" "));
    }
    config
}

fn detect_providers(config: &mut config::Config) {
    for provider in quota::PROVIDERS {
        if !config.declared(provider.id) && !provider.detected() {
            config.hide(provider.id);
        }
    }
}

/// Collects every enabled source and updates the signals. Runs at most once
/// at a time; a request that arrives meanwhile runs once this one is done.
fn refresh(store: Store) {
    if store.busy.get() {
        store.pending.set(true);
        return;
    }
    store.pending.set(false);

    // Pick up a config file edited outside the app.
    let mut config = config::Config::reload(&store.config.get());
    if config != store.config.get() {
        detect_providers(&mut config);
        store.config.set(config);
        prune(store);
    }
    let config = store.config.get();
    let now = unix_ms() as u64;

    // Start every enabled source at once; each lands in the panel as soon as
    // it is ready, so a slow CLI never holds a quick one back.
    let mut blocking_jobs = Vec::new();
    let mut async_jobs = Vec::new();
    for provider in quota::PROVIDERS
        .iter()
        .filter(|provider| config.enabled(provider.id))
    {
        match &provider.kind {
            quota::Kind::Blocking(collect) => {
                let collect = *collect;
                blocking_jobs.push(blocking(move || collect(now)));
            }
            quota::Kind::Async(collect) => {
                let collect = *collect;
                async_jobs.push(collect());
            }
        }
    }

    // Nothing enabled: drop what was shown and stay idle.
    if blocking_jobs.is_empty() && async_jobs.is_empty() {
        set_sources(store, Vec::new());
        store.updated.set(unix_ms());
        store.pending.set(false);
        return;
    }

    store.busy.set(true);
    // Each source lands as soon as it answers; the last one closes the batch.
    let left = Rc::new(Cell::new(async_jobs.len() + blocking_jobs.len()));
    for job in async_jobs {
        let left = Rc::clone(&left);
        spawn(async move {
            upsert(store, job.await);
            finish(store, &left);
        });
    }
    for job in blocking_jobs {
        let left = Rc::clone(&left);
        spawn(async move {
            if let Ok(source) = job.await {
                upsert(store, source);
            }
            finish(store, &left);
        });
    }
}

/// Marks one source of the current batch done; the last one ends the refresh.
fn finish(store: Store, left: &Cell<usize>) {
    left.set(left.get().saturating_sub(1));
    if left.get() > 0 {
        return;
    }
    prune(store);
    store.updated.set(unix_ms());
    store.busy.set(false);
    notify_crossings(store);
    if store.pending.get() {
        refresh(store);
    }
}

/// Replaces one source in the list, keeping the menu bar order stable, and
/// keeps the last numbers of a source that failed.
fn upsert(store: Store, source: Source) {
    // A provider turned off while its fetch was in flight stays off.
    let Some(provider) = quota::Provider::by_short(source.short) else {
        return;
    };
    if !store.config.get().enabled(provider.id) {
        return;
    }
    let mut sources = store.sources.get();
    let source = keep_last(sources.clone(), source);
    match sources.iter_mut().find(|s| s.short == source.short) {
        Some(slot) => *slot = source,
        None => sources.push(source),
    }
    sources.sort_by_key(|s| quota::Provider::position(s.short));
    set_sources(store, sources);
}

/// Drops the sources that are no longer enabled.
fn prune(store: Store) {
    let config = store.config.get();
    let mut sources = store.sources.get();
    let count = sources.len();
    sources.retain(|source| {
        quota::Provider::by_short(source.short).is_some_and(|provider| config.enabled(provider.id))
    });
    if sources.len() != count {
        set_sources(store, sources);
    }
}

/// Replaces the list, and tells an open panel to rebuild its cards.
fn set_sources(store: Store, sources: Vec<Source>) {
    store.sources.set(sources);
    store
        .revision
        .update(|revision| *revision = revision.wrapping_add(1));
}

fn keep_last(previous: Vec<Source>, mut source: Source) -> Source {
    if !source.error.is_empty() {
        if let Some(old) = previous.iter().find(|s| s.short == source.short) {
            if !old.metrics.is_empty() {
                source.metrics = old.metrics.clone();
                source.note = old.note.clone();
            }
        }
    }
    source
}

/// Two compact lines: each provider's 5-hour value above its weekly one.
/// MacOS only; the status item is styled with it after NSH sets the tray.
#[cfg(target_os = "macos")]
fn macos_tray_title(store: Store) -> String {
    let sources = store.sources.get();
    let forced = std::env::var_os("QUOTA_BAR_FORCE_CRITICAL").is_some();
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for provider in shown_providers(store) {
        let source = sources.iter().find(|source| source.short == provider.short);
        let five = source.and_then(|source| window_remaining(source, true));
        let week = source.and_then(|source| window_remaining(source, false));
        let mut segment = format!("{}{}", provider.short, tray_value(five));
        let weekly = tray_value(week);
        let low = forced
            || source
                .and_then(Source::headline)
                .is_some_and(|left| left < 15.0);
        // The dot goes in the second line's empty first cells, under the
        // provider's name. When the weekly number fills the column, both
        // lines get one more cell so the dot does not shift the next one.
        if low && weekly.chars().count() >= segment.chars().count() {
            segment.push(' ');
        }
        let width = segment.chars().count();
        let mut weekly = format!("{weekly:>width$}");
        if low {
            weekly.replace_range(0..1, "●");
        }
        top.push(format!("{segment:<width$}"));
        bottom.push(weekly);
    }
    if top.is_empty() {
        return tr("额度", "Quota").to_owned();
    }
    format!("{}\n{}", top.join(" "), bottom.join(" "))
}

#[cfg(target_os = "macos")]
fn tray_value(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "-".to_owned())
}

/// The tightest 5-hour (or rolling) window, or the tightest weekly one.
fn window_remaining(source: &Source, five_hour: bool) -> Option<f64> {
    source
        .metrics
        .iter()
        .filter(|metric| {
            if five_hour {
                (metric.label.contains('5') && metric.label.contains("小时"))
                    || metric.label == "会话"
                    || metric.label.contains("滚动")
            } else {
                metric.label.contains('周')
            }
        })
        .filter_map(|metric| metric.remaining)
        .reduce(f64::min)
}

#[cfg(target_os = "linux")]
fn tray_runs(store: Store) -> Vec<TrayTitleRun> {
    if !store.tray_ready.get() {
        return Vec::new();
    }
    let sources = store.sources.get();
    let mut runs = Vec::new();
    for provider in shown_providers(store) {
        if !runs.is_empty() {
            runs.push(TrayTitleRun::new("  "));
        }
        let source = sources.iter().find(|source| source.short == provider.short);
        let warning = source
            .and_then(Source::headline)
            .is_some_and(|remaining| remaining < 15.0);
        match source.and_then(Source::headline) {
            Some(remaining) => {
                runs.push(TrayTitleRun::new(format!(
                    "{}{} {:.0}%",
                    provider.short,
                    if warning { "!" } else { "" },
                    remaining
                )));
            }
            None => runs.push(TrayTitleRun::new(provider.short.to_owned())),
        }
    }
    // An empty title would leave no menu bar item to click.
    if runs.is_empty() {
        runs.push(TrayTitleRun::new(tr("额度", "Quota")));
    }
    runs
}

/// `ag · cc`, for the footer and the tooltip.
fn enabled_shorts(config: &config::Config) -> String {
    let shorts: Vec<&str> = quota::PROVIDERS
        .iter()
        .filter(|provider| config.enabled(provider.id))
        .map(|provider| provider.short)
        .collect();
    shorts.join(" · ")
}

const RELEASES_URL: &str = "https://github.com/tdawn0-0/quota-bar/releases/latest";

fn app_version() -> String {
    serde_json::from_str::<serde_json::Value>(include_str!("../nsh.json"))
        .ok()
        .and_then(|value| {
            value
                .get("version")
                .and_then(|version| version.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

fn open_url(url: &'static str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

fn save_config(store: Store, config: config::Config) {
    if let Err(error) = config.save() {
        eprintln!("[quota-bar] config: {error}");
    }
    store.config.set(config);
}

fn set_menu_bar(store: Store, mode: config::MenuBarMode) {
    let mut config = store.config.get();
    config.set_menu_bar(mode);
    save_config(store, config);
}

fn set_notify(store: Store, notify: config::Notify) {
    let mut config = store.config.get();
    config.set_notify(notify);
    save_config(store, config);
}

fn set_language(store: Store, language: config::Language) {
    let mut config = store.config.get();
    config.set_language(language);
    save_config(store, config);
    i18n::choose(match language {
        config::Language::Zh => Some(i18n::Lang::Zh),
        config::Language::En => Some(i18n::Lang::En),
        config::Language::Auto => None,
    });
    // Every string was made in the old language: make them again.
    let pinned = store.pinned.get();
    close_panel(store);
    open_panel(store, None, pinned);
}

/// Brings the panel forward, opening it when it is closed.
fn show_panel(store: Store) {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        if panel.state().map(|state| state.visible).unwrap_or(false) {
            panel.focus();
            return;
        }
    }
    open_panel(store, None, false);
}

/// The login item: a LaunchAgent on macOS, an autostart entry on Linux.
fn autostart_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/LaunchAgents/bar.jyeu.quota.plist"))
    }
    #[cfg(target_os = "linux")]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Some(config.join("autostart/quota-bar.desktop"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        None
    }
}

fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|path| path.is_file())
}

fn set_autostart(on: bool) -> Result<(), String> {
    let path = autostart_path().ok_or("no home directory")?;
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        };
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let exe = exe.to_string_lossy();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    #[cfg(target_os = "macos")]
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>bar.jyeu.quota</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#
    );
    #[cfg(target_os = "linux")]
    let body = format!(
        "[Desktop Entry]\nType=Application\nName=Quota Bar\nExec={exe}\nX-GNOME-Autostart-enabled=true\n"
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let body = String::new();
    std::fs::write(&path, body).map_err(|error| format!("{}: {error}", path.display()))
}

/// Runs one provider again, leaving the others alone.
fn retry_source(store: Store, short: &'static str) {
    let Some(provider) = quota::Provider::by_short(short) else {
        return;
    };
    let now = unix_ms() as u64;
    match &provider.kind {
        quota::Kind::Blocking(collect) => {
            let collect = *collect;
            spawn(async move {
                if let Ok(source) = blocking(move || collect(now)).await {
                    upsert(store, source);
                    store.updated.set(unix_ms());
                    notify_crossings(store);
                }
            });
        }
        quota::Kind::Async(collect) => {
            let collect = *collect;
            spawn(async move {
                upsert(store, collect().await);
                store.updated.set(unix_ms());
                notify_crossings(store);
            });
        }
    }
}

/// Warns when a provider falls under 15% and when a 5-hour window is back to
/// full, once per crossing.
fn notify_crossings(store: Store) {
    let notify = store.config.get().notify();
    if !notify.low && !notify.reset {
        return;
    }
    thread_local! {
        static PREV: RefCell<BTreeMap<&'static str, Option<f64>>> = const { RefCell::new(BTreeMap::new()) };
    }
    let sources = store.sources.get();
    PREV.with(|previous| {
        let mut previous = previous.borrow_mut();
        for source in &sources {
            let five = window_remaining(source, true);
            let before = previous.insert(source.short, five).flatten();
            let (Some(before), Some(now)) = (before, five) else {
                continue;
            };
            if notify.low && before >= 15.0 && now < 15.0 {
                send_notification(
                    format!("quota-low-{}", source.short),
                    format!("{} {}", source.title, tr("额度不足", "is running low")),
                    if i18n::is_english() {
                        format!("{now:.0}% left")
                    } else {
                        format!("仅剩 {now:.0}%，注意安排")
                    },
                );
            }
            if notify.reset && before < 99.5 && now >= 99.5 {
                send_notification(
                    format!("quota-reset-{}", source.short),
                    format!("{} {}", source.title, tr("额度已恢复", "is back")),
                    tr(
                        "会话额度已回满，可以继续干活了。",
                        "Session quota is back to full. Back to work.",
                    )
                    .to_owned(),
                );
            }
        }
    });
}

fn send_notification(id: String, title: String, body: String) {
    thread_local! {
        static ASKED: Cell<bool> = const { Cell::new(false) };
    }
    let ask = !ASKED.with(|asked| asked.replace(true));
    spawn(async move {
        if ask {
            let _ = request_notification_permission().await;
        }
        let _ = show_notification(Notification::new(id, title, body)).await;
    });
}

/// The providers the menu bar shows: all, or the two with the least left.
fn shown_providers(store: Store) -> Vec<&'static quota::Provider> {
    let config = store.config.get();
    let mut shown: Vec<&'static quota::Provider> = quota::PROVIDERS
        .iter()
        .filter(|provider| config.enabled(provider.id))
        .collect();
    if config.menu_bar() == config::MenuBarMode::Compact && shown.len() > 2 {
        let sources = store.sources.get();
        let left = |provider: &&quota::Provider| {
            sources
                .iter()
                .find(|source| source.short == provider.short)
                .and_then(Source::headline)
                .unwrap_or(f64::MAX)
        };
        shown.sort_by(|a, b| left(a).total_cmp(&left(b)));
        shown.truncate(2);
        shown.sort_by_key(|provider| quota::Provider::position(provider.short));
    }
    shown
}

fn provider_menu(store: Store) -> MenuItem {
    let config = store.config.get();
    MenuItem::submenu(
        "显示",
        quota::PROVIDERS
            .iter()
            .map(|provider| {
                let on = config.enabled(provider.id);
                MenuItem::new(provider.title)
                    .checked(on)
                    .on_select(move || set_provider(store, provider.id, !on))
            })
            .collect::<Vec<_>>(),
    )
}

/// Turns one provider on or off, saves the choice, and updates what shows.
fn set_provider(store: Store, id: &'static str, on: bool) {
    let mut config = store.config.get();
    config.set_enabled(id, on);
    if let Err(error) = config.save() {
        eprintln!("[quota-bar] config: {error}");
    }
    store.config.set(config);
    if on {
        refresh(store);
    } else {
        prune(store);
    }
}

fn set_tray_menu(store: Store) {
    let config = store.config.get();
    #[cfg(target_os = "macos")]
    let macos_title = macos_tray_title(store);
    #[cfg(target_os = "macos")]
    if std::env::var_os("QUOTA_BAR_DEBUG").is_some() {
        eprintln!("[quota-bar] tray title {macos_title:?}");
    }
    // NSH sizes the status item to its title; give macOS the two-line title
    // from the start so a long single-line one never widens it past the menu
    // bar. The AppKit pass below then sets the compact monospaced font.
    #[cfg(target_os = "macos")]
    let title = vec![TrayTitleRun::new(macos_title.clone())];
    #[cfg(not(target_os = "macos"))]
    let title = tray_runs(store);
    set_tray(
        tray()
            .icon(TRAY_ICON)
            .title(title)
            .tooltip(format!(
                "{}：{}",
                tr("AI 额度", "AI quota"),
                match enabled_shorts(&config).as_str() {
                    "" => tr("没有来源", "no providers"),
                    shorts => shorts,
                }
            ))
            .on_click(move |event| toggle_panel(store, event.bounds, false))
            .menu([
                MenuItem::new("刷新").on_select(move || refresh(store)),
                provider_menu(store),
                MenuItem::new("面板").on_select(move || toggle_panel(store, None, false)),
                MenuItem::separator(),
                MenuItem::new("退出").on_select(quit),
            ]),
    );
    // NSH sets the two-line title and transparent icon first; AppKit gets
    // the compact monospaced, right-aligned, icon-less item right after.
    #[cfg(target_os = "macos")]
    set_timeout(Duration::from_millis(80), move || {
        // The task is queued; dropping the future only drops its result.
        drop(run_on_main_thread(move || macos_tray::apply(&macos_title)));
    });
}

fn panel_close_on_blur(pinned: bool) -> bool {
    // GTK 4 cannot restore a toplevel's position on Wayland. Keep the
    // Linux panel mapped when focus changes instead of discarding it.
    cfg!(target_os = "macos") && !pinned
}

fn glass_attributes(pinned: bool) -> WindowAttributes {
    let mut attributes = WindowAttributes::default();
    attributes.transparent = Some(true);
    // No popover material: NSH 0.5 puts macOS's NSVisualEffectView behind
    // the whole rectangular window, so it shows as a square layer outside
    // the sheet's rounded corners. The sheet's own fill is the material.
    attributes.close_on_blur = Some(panel_close_on_blur(pinned));
    attributes.level = Some(if pinned {
        WindowLevel::Floating
    } else {
        WindowLevel::Normal
    });
    attributes.activate = Some(true);
    // A popup is not resizable by default; the sheet's edges resize it.
    attributes.resizable = Some(true);
    attributes.min_size = Some(Some(size(px(PANEL_MIN_WIDTH), px(PANEL_MIN_HEIGHT))));
    attributes
}

/// A tray click closes an open panel and opens a closed one; a pinned panel
/// stays and comes forward instead.
fn toggle_panel(store: Store, bounds: Option<Bounds<Points>>, pinned: bool) {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        if panel.state().map(|state| state.visible).unwrap_or(false) {
            if store.pinned.get() {
                panel.focus();
                return;
            }
            panel.close();
            PANEL.with(|slot| *slot.borrow_mut() = None);
            store.pinned.set(false);
            return;
        }
    }
    open_panel(store, bounds, pinned);
}

fn close_panel(store: Store) {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        panel.close();
        PANEL.with(|slot| *slot.borrow_mut() = None);
        store.pinned.set(false);
    }
}

/// Keeps the panel open (and above other windows) until it is unpinned.
fn toggle_pin(store: Store) {
    let pinned = !store.pinned.get();
    store.pinned.set(pinned);
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        let mut attributes = WindowAttributes::default();
        attributes.close_on_blur = Some(panel_close_on_blur(pinned));
        attributes.level = Some(if pinned {
            WindowLevel::Floating
        } else {
            WindowLevel::Normal
        });
        panel.set(attributes);
    }
}

/// Keeps the size the user resizes the panel to, once a drag settles.
fn track_panel_size(store: Store) {
    let window = current_window();
    effect(move || {
        let Some(state) = window.state() else {
            return;
        };
        if !state.visible {
            return;
        }
        let bounds = (state.bounds.size.width.0, state.bounds.size.height.0);
        // `default_size` is the outer size; a GTK window reports its content,
        // without the window manager's border. Add that back, or each open
        // would shrink the panel by it.
        let border = PANEL_BORDER.with(Cell::get).unwrap_or_else(|| {
            let request = PANEL_REQUEST.with(Cell::get).unwrap_or(bounds);
            let border = (
                (request.0 - bounds.0).max(0.0).round(),
                (request.1 - bounds.1).max(0.0).round(),
            );
            PANEL_BORDER.with(|slot| slot.set(Some(border)));
            border
        });
        let size = ((bounds.0 + border.0).round(), (bounds.1 + border.1).round());
        if PANEL_SIZE_LAST.with(Cell::get) == Some(size) {
            return;
        }
        if !PANEL_SIZE_SEEN.with(|seen| seen.replace(true)) {
            // What it opened at, not a resize.
            PANEL_SIZE_LAST.with(|last| last.set(Some(size)));
            return;
        }
        let sequence = PANEL_SIZE_SEQ.with(|seq| {
            seq.set(seq.get().wrapping_add(1));
            seq.get()
        });
        set_timeout(Duration::from_millis(700), move || {
            if PANEL_SIZE_SEQ.with(Cell::get) != sequence {
                return;
            }
            PANEL_SIZE_LAST.with(|last| last.set(Some(size)));
            save_panel_size(store, size);
        });
    });
}

fn save_panel_size(store: Store, (width, height): (f32, f32)) {
    if !(PANEL_MIN_WIDTH..=PANEL_MAX_SIZE).contains(&width)
        || !(PANEL_MIN_HEIGHT..=PANEL_MAX_SIZE).contains(&height)
    {
        return;
    }
    let mut config = store.config.get();
    let panel = config::PanelSize { width, height };
    if config.panel() == Some(panel) {
        return;
    }
    config.set_panel(panel);
    if let Err(error) = config.save() {
        eprintln!("[quota-bar] config: {error}");
        return;
    }
    store.config.set(config);
}

/// Tall enough for one card per enabled provider, never taller than before.
fn panel_height(store: Store) -> f32 {
    let config = store.config.get();
    let count = quota::PROVIDERS
        .iter()
        .filter(|provider| config.enabled(provider.id))
        .count();
    (104.0 + count as f32 * 184.0).clamp(220.0, PANEL_MAX_HEIGHT)
}

/// The size the panel opens with: the one it was left at, else the one the
/// enabled providers ask for.
fn opening_size(store: Store) -> (f32, f32) {
    match store.config.get().panel() {
        Some(saved) => (
            saved.width.clamp(PANEL_MIN_WIDTH, PANEL_MAX_SIZE),
            saved.height.clamp(PANEL_MIN_HEIGHT, PANEL_MAX_SIZE),
        ),
        None => (PANEL_WIDTH, panel_height(store)),
    }
}

fn open_panel(store: Store, bounds: Option<Bounds<Points>>, pinned: bool) {
    store.pinned.set(pinned);
    PANEL_SIZE_SEEN.with(|seen| seen.set(false));
    PANEL_SIZE_LAST.with(|last| last.set(None));
    PANEL_BORDER.with(|border| border.set(None));
    let (width, height) = opening_size(store);
    PANEL_REQUEST.with(|request| request.set(Some((width, height))));
    let position = bounds.map(|icon| {
        let x = icon.origin.x.0 + icon.size.width.0 / 2.0 - width / 2.0;
        let y = icon.origin.y.0 + icon.size.height.0 + 6.0;
        WindowPosition::At(point(px(x.max(8.0)), px(y)))
    });

    let debug_at = std::env::var("QUOTA_BAR_PANEL_AT").ok().and_then(|value| {
        let (x, y) = value.split_once(',')?;
        Some(WindowPosition::At(point(
            px(x.trim().parse().ok()?),
            px(y.trim().parse().ok()?),
        )))
    });

    let mut builder = window()
        .id("panel")
        .kind(WindowKind::Popup)
        .attributes(glass_attributes(pinned))
        .default_size(size(px(width), px(height)))
        .on_open_change(move |change| {
            if !change.open {
                PANEL.with(|slot| *slot.borrow_mut() = None);
                store.pinned.set(false);
            }
        })
        .content(move || panel(store));
    if let Some(position) = debug_at.or(position) {
        builder = builder.default_position(position);
    }
    match open_window(builder) {
        Some(panel) => {
            PANEL.with(|slot| *slot.borrow_mut() = Some(panel.clone()));
            OPENED_AT.with(|at| at.set(unix_ms()));
            panel.focus();
            // macOS may turn the first activation down; asking again once the
            // window exists takes the keyboard from the app in front.
            #[cfg(target_os = "macos")]
            {
                let again = panel.clone();
                set_timeout(Duration::from_millis(180), move || {
                    again.focus();
                });
            }
        }
        None => eprintln!("[quota-bar] panel window was refused"),
    }
}

// ---------------------------------------------------------------------- UI

/// Nicer than a traffic light: healthy quota keeps the text color, low ones
/// turn amber, empty ones red.
fn tone(remaining: Option<f64>) -> AdaptiveColor {
    match remaining {
        Some(value) if value < 15.0 => rgb(RED).into(),
        Some(value) if value < 40.0 => rgb(AMBER).into(),
        Some(_) => rgb(BLUE).into(),
        None => theme().muted_foreground,
    }
}

/// A translucent white sheet over the window's material.
/// The translucent sheet the whole panel sits on.
fn sheet_fill() -> AdaptiveColor {
    AdaptiveColor::new(rgba(0xffffffe6), rgba(0x1c1c1ee0))
}

fn card_fill() -> AdaptiveColor {
    AdaptiveColor::new(rgba(0xffffff70), rgba(0xffffff0f))
}

fn glass_border() -> AdaptiveColor {
    AdaptiveColor::new(rgba(0x00000012), rgba(0xffffff1c))
}

/// The hairline that reads as a lit edge along the top of the glass.
fn glass_highlight() -> AdaptiveColor {
    AdaptiveColor::new(rgba(0xffffffcc), rgba(0xffffff2e))
}

fn value_color(remaining: Option<f64>) -> AdaptiveColor {
    match remaining {
        Some(value) if value >= 15.0 => theme().foreground,
        Some(_) => rgb(RED).into(),
        None => theme().muted_foreground,
    }
}

fn panel(store: Store) -> impl IntoElement {
    track_panel_size(store);
    // A window's content is made once: the cards and the empty states live
    // in `when` / `each` regions so a fetch that lands swaps them, and the
    // cards follow the list's revision.
    view()
        .flex_1()
        .flex_col()
        .w_full()
        .rounded(px(14.0))
        .overflow_hidden()
        .bg(sheet_fill())
        .border(px(1.0))
        .border_color(glass_border())
        .text_color(move || theme().popover_foreground)
        .child(view().w_full().h(px(1.0)).bg(glass_highlight()))
        .child(header(store))
        .child(
            scroll().flex_1().w_full().child(
                view()
                    .flex_col()
                    .w_full()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .pb(px(12.0))
                    .child(when(
                        move || store.settings.get(),
                        move || settings_body(store),
                    ))
                    .child(when(
                        move || !store.settings.get() && loading(store),
                        move || skeleton(store),
                    ))
                    .child(when(
                        move || !store.settings.get() && empty(store),
                        move || empty_state(store),
                    ))
                    .child(when(
                        move || !store.settings.get() && !loading(store) && !empty(store),
                        move || cards(store),
                    )),
            ),
        )
        .child(footer(store))
        // A frameless window resizes by these invisible grips; corners go
        // last so they win over the edges they overlap.
        .child(resize_grip(ResizeEdge::Top))
        .child(resize_grip(ResizeEdge::Bottom))
        .child(resize_grip(ResizeEdge::Left))
        .child(resize_grip(ResizeEdge::Right))
        .child(resize_grip(ResizeEdge::TopLeft))
        .child(resize_grip(ResizeEdge::TopRight))
        .child(resize_grip(ResizeEdge::BottomLeft))
        .child(resize_grip(ResizeEdge::BottomRight))
}

/// An invisible grip along one edge or corner of the frameless sheet: a
/// press on it resizes the window instead of reaching the content.
fn resize_grip(edge: ResizeEdge) -> impl IntoElement {
    let grip = view().absolute().window_resize(edge);
    match edge {
        ResizeEdge::Top => grip.top(px(0.0)).left(px(12.0)).right(px(12.0)).h(px(5.0)),
        ResizeEdge::Bottom => grip
            .bottom(px(0.0))
            .left(px(12.0))
            .right(px(12.0))
            .h(px(5.0)),
        ResizeEdge::Left => grip.left(px(0.0)).top(px(12.0)).bottom(px(12.0)).w(px(5.0)),
        ResizeEdge::Right => grip
            .right(px(0.0))
            .top(px(12.0))
            .bottom(px(12.0))
            .w(px(5.0)),
        ResizeEdge::TopLeft => grip.top(px(0.0)).left(px(0.0)).size(px(12.0)),
        ResizeEdge::TopRight => grip.top(px(0.0)).right(px(0.0)).size(px(12.0)),
        ResizeEdge::BottomLeft => grip.bottom(px(0.0)).left(px(0.0)).size(px(12.0)),
        ResizeEdge::BottomRight => grip.bottom(px(0.0)).right(px(0.0)).size(px(12.0)),
    }
}

fn loading(store: Store) -> bool {
    store.sources.get().is_empty() && (store.busy.get() || store.updated.get() <= 0.0)
}

fn empty(store: Store) -> bool {
    let sources = store.sources.get();
    sources.is_empty() && !store.busy.get() && store.updated.get() > 0.0
}

/// The cards, in the order the providers have, wrapping into columns.
fn cards(store: Store) -> impl IntoElement {
    view()
        .flex_row()
        .flex_wrap()
        .w_full()
        .gap(px(8.0))
        .child(each(
            move || {
                let _ = store.revision.get();
                store.sources.get()
            },
            move |source| (store.revision.get(), source.short),
            move |source, _| card(store, source.get()),
        ))
}

/// The first fetch has not answered yet.
fn skeleton(_store: Store) -> impl IntoElement {
    view()
        .flex_col()
        .w_full()
        .gap(px(8.0))
        .child(
            text(tr("正在读取额度…", "Reading quotas…"))
                .text_size(px(11.0))
                .text_color(move || theme().muted_foreground),
        )
        .children((0..3).map(|_| {
            view()
                .w_full()
                .h(px(96.0))
                .rounded(px(10.0))
                .bg(card_fill())
                .border(px(1.0))
                .border_color(glass_border())
                .child(
                    view().w_full().h(px(10.0)).p(px(11.0)).child(
                        view()
                            .w(px(88.0))
                            .h(px(8.0))
                            .rounded(px(4.0))
                            .bg(move || theme().muted),
                    ),
                )
        }))
}

/// No provider answered (or every one is off): a way back in.
fn empty_state(store: Store) -> impl IntoElement {
    view()
        .flex_col()
        .w_full()
        .gap(px(10.0))
        .p(px(12.0))
        .rounded(px(10.0))
        .bg(card_fill())
        .border(px(1.0))
        .border_color(glass_border())
        .child(
            text(tr("还没有可用的来源", "No providers yet"))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM),
        )
        .child(
            text(tr(
                "先在本机登录 Claude Code、Codex 等工具；也可以直接打开一个来源：",
                "Sign in to Claude Code, Codex and the like here, or turn one on:",
            ))
            .text_size(px(10.0))
            .text_color(move || theme().muted_foreground),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .children(quota::PROVIDERS.iter().map(|provider| {
                    choice(
                        provider.title,
                        move || store.config.get().enabled(provider.id),
                        move || {
                            set_provider(
                                store,
                                provider.id,
                                !store.config.get().enabled(provider.id),
                            )
                        },
                    )
                })),
        )
        .child(
            view()
                .px(px(9.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(move || theme().secondary)
                .on_press(move || refresh(store))
                .child(
                    text(tr("刷新", "Refresh"))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme().secondary_foreground),
                ),
        )
}

fn header(store: Store) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .w_full()
        .px(px(14.0))
        .pt(px(12.0))
        .pb(px(10.0))
        .gap(px(8.0))
        .window_drag(true)
        .child(
            text(move || {
                if store.settings.get() {
                    tr("设置", "Settings")
                } else {
                    tr("AI 额度", "AI Quota")
                }
            })
            .text_size(px(13.0))
            .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(when(
                    move || !store.settings.get() && store.updated.get() > 0.0,
                    move || {
                        text(move || i18n::ago(store.updated.get()))
                            .text_size(px(11.0))
                            .text_color(move || theme().muted_foreground)
                    },
                ))
                .child(when(
                    move || !store.settings.get(),
                    move || {
                        view()
                            .px(px(9.0))
                            .py(px(4.0))
                            .rounded(px(6.0))
                            .bg(move || {
                                if store.busy.get() {
                                    theme().muted
                                } else {
                                    theme().secondary
                                }
                            })
                            .on_press(move || refresh(store))
                            .child(
                                text(move || {
                                    if store.busy.get() {
                                        tr("刷新中", "Refreshing")
                                    } else {
                                        tr("刷新", "Refresh")
                                    }
                                })
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || theme().secondary_foreground),
                            )
                    },
                ))
                .child(when(
                    move || !store.settings.get(),
                    move || {
                        view()
                            .px(px(9.0))
                            .py(px(4.0))
                            .rounded(px(6.0))
                            .bg(move || {
                                if store.pinned.get() {
                                    theme().secondary
                                } else {
                                    theme().muted
                                }
                            })
                            .on_press(move || toggle_pin(store))
                            .child(
                                text(move || {
                                    if store.pinned.get() {
                                        tr("已置顶", "Pinned")
                                    } else {
                                        tr("置顶", "Pin")
                                    }
                                })
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || theme().secondary_foreground),
                            )
                    },
                ))
                .child(when(
                    move || store.settings.get(),
                    move || {
                        view()
                            .px(px(9.0))
                            .py(px(4.0))
                            .rounded(px(6.0))
                            .bg(move || theme().secondary)
                            .on_press(move || store.settings.set(false))
                            .child(
                                text(tr("完成", "Done"))
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(move || theme().secondary_foreground),
                            )
                    },
                )),
        )
}

/// The size of a card, and where its text goes.
fn card(store: Store, source: Source) -> impl IntoElement {
    let error = !source.error.is_empty();
    let short = source.short;
    view()
        .flex_col()
        .flex_1()
        .basis(px(240.0))
        .min_w(px(220.0))
        .gap(px(6.0))
        .p(px(11.0))
        .rounded(px(10.0))
        .bg(card_fill())
        .border(px(1.0))
        .border_color(glass_border())
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text(source.title.clone())
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(
                    text(match source.headline() {
                        Some(value) => format!("{value:.0}%"),
                        None => "—".to_owned(),
                    })
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(value_color(source.headline())),
                ),
        )
        .child(
            view()
                .flex_col()
                .gap(px(6.0))
                .children(source.metrics.iter().map(metric_row)),
        )
        .child(
            view()
                .flex_row()
                .items_start()
                .justify_between()
                .gap(px(6.0))
                .child(
                    text(if error {
                        source.error.clone()
                    } else {
                        i18n::note(&source.note)
                    })
                    .flex_1()
                    .text_size(px(10.0))
                    .text_color(if error {
                        rgb(RED).into()
                    } else {
                        theme().muted_foreground
                    }),
                )
                .child(when(
                    move || error,
                    move || {
                        view()
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .bg(move || theme().muted)
                            .on_press(move || retry_source(store, short))
                            .child(
                                text(tr("重试", "Retry"))
                                    .text_size(px(10.0))
                                    .text_color(move || theme().secondary_foreground),
                            )
                    },
                )),
        )
}

fn footer(store: Store) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .w_full()
        .px(px(14.0))
        .py(px(9.0))
        .border_t(px(1.0))
        .border_color(move || theme().border)
        .child(
            view()
                .px(px(7.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .on_press(move || store.settings.set(!store.settings.get()))
                .child(
                    text(move || {
                        if store.settings.get() {
                            tr("返回", "Back").to_owned()
                        } else {
                            let shorts = enabled_shorts(&store.config.get());
                            if shorts.is_empty() {
                                tr("来源", "Sources").to_owned()
                            } else {
                                shorts
                            }
                        }
                    })
                    .text_size(px(10.0))
                    .text_color(move || theme().muted_foreground),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(when(
                    move || store.update.get().is_some(),
                    move || {
                        view()
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .bg(move || theme().secondary)
                            .on_press(|| open_url(RELEASES_URL))
                            .child(
                                text(move || {
                                    store
                                        .update
                                        .get()
                                        .map(|tag| format!("v{tag}"))
                                        .unwrap_or_default()
                                })
                                .text_size(px(10.0))
                                .text_color(move || theme().secondary_foreground),
                            )
                    },
                ))
                .child(
                    view()
                        .px(px(9.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .on_press(move || quit())
                        .child(
                            text(tr("退出", "Quit"))
                                .text_size(px(11.0))
                                .text_color(move || theme().muted_foreground),
                        ),
                ),
        )
}

/// The settings page inside the panel: sources, menu bar, language, login
/// item and notifications.
fn settings_body(store: Store) -> impl IntoElement {
    view()
        .flex_col()
        .w_full()
        .gap(px(10.0))
        .child(section_title(tr("来源", "Providers")))
        .children(quota::PROVIDERS.iter().map(|provider| {
            toggle_row(
                provider.title,
                move || store.config.get().enabled(provider.id),
                move || set_provider(store, provider.id, !store.config.get().enabled(provider.id)),
            )
        }))
        .child(divider())
        .child(section_title(tr("菜单栏", "Menu bar")))
        .child(
            view()
                .flex_row()
                .gap(px(6.0))
                .child(choice(
                    tr("全部", "All"),
                    move || store.config.get().menu_bar() == config::MenuBarMode::All,
                    move || set_menu_bar(store, config::MenuBarMode::All),
                ))
                .child(choice(
                    tr("紧凑", "Compact"),
                    move || store.config.get().menu_bar() == config::MenuBarMode::Compact,
                    move || set_menu_bar(store, config::MenuBarMode::Compact),
                )),
        )
        .child(divider())
        .child(section_title(tr("语言", "Language")))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .child(choice(
                    tr("跟随系统", "System"),
                    move || store.config.get().language() == config::Language::Auto,
                    move || set_language(store, config::Language::Auto),
                ))
                .child(choice(
                    "中文",
                    move || store.config.get().language() == config::Language::Zh,
                    move || set_language(store, config::Language::Zh),
                ))
                .child(choice(
                    "English",
                    move || store.config.get().language() == config::Language::En,
                    move || set_language(store, config::Language::En),
                )),
        )
        .child(divider())
        .child(section_title(tr("其他", "More")))
        .child(toggle_row(
            tr("开机自启", "Launch at Login"),
            move || store.autostart.get(),
            move || {
                let on = !store.autostart.get();
                match set_autostart(on) {
                    Ok(()) => store.autostart.set(on),
                    Err(error) => eprintln!("[quota-bar] autostart: {error}"),
                }
            },
        ))
        .child(toggle_row(
            tr("低额度提醒", "Low quota alerts"),
            move || store.config.get().notify().low,
            move || {
                let mut notify = store.config.get().notify();
                notify.low = !notify.low;
                set_notify(store, notify);
            },
        ))
        .child(toggle_row(
            tr("恢复提醒", "Recovery alerts"),
            move || store.config.get().notify().reset,
            move || {
                let mut notify = store.config.get().notify();
                notify.reset = !notify.reset;
                set_notify(store, notify);
            },
        ))
        .child(
            text(move || {
                let version = app_version();
                match store.update.get() {
                    Some(tag) => format!("Quota Bar {version} · {} v{tag}", tr("新版", "update")),
                    None => format!("Quota Bar {version}"),
                }
            })
            .text_size(px(10.0))
            .text_color(move || theme().muted_foreground),
        )
}

fn section_title(label: &'static str) -> impl IntoElement {
    text(label)
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(move || theme().muted_foreground)
}

fn divider() -> impl IntoElement {
    view().w_full().h(px(1.0)).bg(move || theme().border)
}

fn toggle_row(
    label: &'static str,
    on: impl Fn() -> bool + 'static,
    press: impl Fn() + 'static,
) -> impl IntoElement {
    let on: Rc<dyn Fn() -> bool> = Rc::new(on);
    let (bg_on, text_on) = (Rc::clone(&on), Rc::clone(&on));
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            text(label)
                .text_size(px(11.0))
                .text_color(move || theme().foreground),
        )
        .child(
            view()
                .px(px(9.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .bg(move || {
                    if bg_on() {
                        theme().secondary
                    } else {
                        theme().muted
                    }
                })
                .on_press(press)
                .child(
                    text(move || {
                        if text_on() {
                            tr("开", "On")
                        } else {
                            tr("关", "Off")
                        }
                    })
                    .text_size(px(10.0))
                    .text_color(move || theme().secondary_foreground),
                ),
        )
}

/// A labelled pill: highlighted when `selected` is true.
fn choice(
    label: &'static str,
    selected: impl Fn() -> bool + 'static,
    press: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .px(px(9.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(move || {
            if selected() {
                theme().secondary
            } else {
                theme().muted
            }
        })
        .on_press(press)
        .child(
            text(label)
                .text_size(px(10.0))
                .text_color(move || theme().secondary_foreground),
        )
}

fn metric_row(metric: &quota::Metric) -> impl IntoElement {
    let remaining = metric.remaining;
    // A fraction of the track, so a wider card gets a longer bar.
    let filled = (remaining.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0) as f32;
    view()
        .flex_col()
        .gap(px(3.0))
        .child(
            view()
                .flex_row()
                .justify_between()
                .items_center()
                .child(
                    text(i18n::metric_label(&metric.label))
                        .text_size(px(11.0))
                        .text_color(move || theme().foreground),
                )
                .child(
                    text(match remaining {
                        Some(value) => format!("{value:.0}%"),
                        None => "不适用".to_owned(),
                    })
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(value_color(remaining)),
                ),
        )
        .child(
            view()
                .w_full()
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(move || theme().muted)
                .overflow_hidden()
                .child(
                    view()
                        .w(relative(filled))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(tone(remaining)),
                ),
        )
        .child(
            text(i18n::metric_detail(&metric.detail))
                .text_size(px(10.0))
                .text_color(move || theme().muted_foreground),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_panel_stays_open_when_focus_moves_to_another_app() {
        for pinned in [false, true] {
            assert_eq!(glass_attributes(pinned).close_on_blur, Some(false));
        }
    }

    fn metric(remaining: Option<f64>) -> quota::Metric {
        quota::Metric {
            label: "周额度".to_owned(),
            remaining,
            detail: String::new(),
        }
    }

    #[test]
    fn headline_takes_the_tightest_window() {
        let mut source = quota::Source::empty("ag", "Antigravity");
        source.metrics = vec![metric(Some(80.0)), metric(Some(12.0)), metric(None)];
        assert_eq!(source.headline(), Some(12.0));
    }

    #[test]
    fn every_provider_has_a_unique_id_and_place() {
        let ids: std::collections::HashSet<_> = quota::PROVIDERS
            .iter()
            .map(|provider| provider.id)
            .collect();
        assert_eq!(ids.len(), quota::PROVIDERS.len());

        for (index, provider) in quota::PROVIDERS.iter().enumerate() {
            assert_eq!(quota::Provider::position(provider.short), index);
            assert_eq!(
                quota::Provider::by_short(provider.short).map(|found| found.id),
                Some(provider.id)
            );
        }
    }

    #[test]
    fn merge_keeps_last_numbers_when_a_source_fails() {
        let mut old = quota::Source::empty("cc", "Claude Code");
        old.metrics = vec![metric(Some(40.0))];
        old.note = "credits 0".to_owned();

        let mut failed = quota::Source::empty("cc", "Claude Code");
        failed.error = "claude: not logged in".to_owned();

        let merged = keep_last(vec![old], failed);
        assert_eq!(merged.metrics.len(), 1);
        assert_eq!(merged.metrics[0].remaining, Some(40.0));
        assert_eq!(merged.note, "credits 0");
        assert!(merged.error.contains("not logged in"));
    }
}
