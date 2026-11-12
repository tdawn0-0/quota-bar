//! Quota Bar: a menu bar app showing the remaining quota of the local AI CLIs.
//!
//! Resource budget: no busy loop. Data is fetched at launch, every five
//! minutes, whenever the refresh control or menu item asks, and when a
//! provider is turned back on; opening the panel only reads what is already in
//! memory. The panel is a popup window that closes on blur, so an idle app is
//! one tray item and a few kilobytes of state.

mod config;
#[cfg(target_os = "macos")]
mod macos_tray;
mod quota;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use nsh_base::theme;
use nsh_core::color::{rgb, rgba};
use nsh_core::desktop::{TrayTitleRun, WindowAttributes, WindowKind, WindowPosition};
use nsh_core::element::desktop::{MenuItem, WindowRef, open_window, quit, set_tray, tray, window};
use nsh_core::geometry::{Bounds, Points, point, px, size};
use nsh_core::platform::Lifecycle;
use nsh_core::prelude::*;
use nsh_core::time::unix_ms;

use quota::Source;

const PANEL_WIDTH: f32 = 344.0;
const PANEL_MAX_HEIGHT: f32 = 656.0;

const BLUE: u32 = 0x0a84ff;
const AMBER: u32 = 0xff9f0a;
const RED: u32 = 0xff3b30;

/// The menu bar item's picture. On macOS its title carries the item, so a
/// 1×1 transparent icon leaves it text-only; Linux needs a visible icon.
#[cfg(target_os = "macos")]
const TRAY_ICON: &str = "data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 width=%271%27 height=%271%27/%3E";
#[cfg(target_os = "linux")]
const TRAY_ICON: &str = "data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 viewBox=%270 0 16 16%27%3E%3Crect x=%272.5%27 y=%279%27 width=%272.5%27 height=%275%27 rx=%271%27/%3E%3Crect x=%276.75%27 y=%275.5%27 width=%272.5%27 height=%278.5%27 rx=%271%27/%3E%3Crect x=%2711%27 y=%272%27 width=%272.5%27 height=%2712%27 rx=%271%27/%3E%3C/svg%3E";

thread_local! {
    /// The open panel, so a second tray click closes it.
    static PANEL: RefCell<Option<WindowRef>> = const { RefCell::new(None) };
    /// When it opened, so activation that arrives late does not close it.
    static OPENED_AT: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// A pinned panel (screenshots) stays open when another app comes forward.
    static PINNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
    /// Linux tray hosts can miss a label set before they start; the label is
    /// sent once more after startup.
    tray_ready: Signal<bool>,
}

#[native_module("quota-bar/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

#[component]
pub fn App() -> impl IntoElement {
    let store = Store {
        sources: signal(Vec::<Source>::new()),
        updated: signal(0.0_f64),
        busy: signal(false),
        pending: signal(false),
        config: signal(config::Config::load().unwrap_or_else(|error| {
            eprintln!("[quota-bar] config: {error}; showing every provider");
            config::Config::default()
        })),
        tray_ready: signal(cfg!(not(target_os = "linux"))),
    };

    // One fetch at launch, then only on demand.
    refresh(store);

    effect(move || set_tray_menu(store));

    // GNOME's AppIndicator extension can miss the first label; sending an
    // empty one first and the real one shortly after makes it show.
    #[cfg(target_os = "linux")]
    set_timeout(Duration::from_secs(2), move || store.tray_ready.set(true));

    // Switching to another app closes the panel. The delay lets the
    // activation that follows a tray click land first.
    effect(move || {
        if lifecycle() == Lifecycle::Active || PINNED.with(|pinned| pinned.get()) {
            return;
        }
        set_timeout(Duration::from_millis(400), move || {
            let age = unix_ms() - OPENED_AT.with(|at| at.get());
            if lifecycle() != Lifecycle::Active && age > 900.0 {
                close_panel();
            }
        });
    });

    // Steady background refresh; opening the panel only reads what is there.
    let interval = std::env::var("QUOTA_BAR_REFRESH_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs >= 5)
        .unwrap_or(300);
    set_interval(Duration::from_secs(interval), move || refresh(store));

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

/// Collects every enabled source and updates the signals. Runs at most once
/// at a time; a request that arrives meanwhile runs once this one is done.
fn refresh(store: Store) {
    if store.busy.get() {
        store.pending.set(true);
        return;
    }
    store.pending.set(false);

    // Pick up a config file edited outside the app.
    let config = config::Config::reload(&store.config.get());
    if config != store.config.get() {
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
        store.sources.set(Vec::new());
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
    store.sources.set(sources);
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
        store.sources.set(sources);
    }
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
    let config = store.config.get();
    let sources = store.sources.get();
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for provider in quota::PROVIDERS
        .iter()
        .filter(|provider| config.enabled(provider.id))
    {
        let source = sources.iter().find(|source| source.short == provider.short);
        let five = source.and_then(|source| window_remaining(source, true));
        let week = source.and_then(|source| window_remaining(source, false));
        let segment = format!("{}{}", provider.short, tray_value(five));
        let width = segment.chars().count();
        top.push(segment);
        bottom.push(format!("{:>width$}", tray_value(week)));
    }
    if top.is_empty() {
        return "额度".to_owned();
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
#[cfg(target_os = "macos")]
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

fn tray_runs(store: Store) -> Vec<TrayTitleRun> {
    if !store.tray_ready.get() {
        return Vec::new();
    }
    let config = store.config.get();
    let sources = store.sources.get();
    let mut runs = Vec::new();
    for provider in quota::PROVIDERS
        .iter()
        .filter(|provider| config.enabled(provider.id))
    {
        if !runs.is_empty() {
            runs.push(TrayTitleRun::new("  "));
        }
        let source = sources.iter().find(|source| source.short == provider.short);
        match source.and_then(Source::headline) {
            Some(remaining) => {
                runs.push(TrayTitleRun::new(format!(
                    "{} {:.0}%",
                    provider.short, remaining
                )));
            }
            None => runs.push(TrayTitleRun::new(provider.short.to_owned())),
        }
    }
    // An empty title would leave no menu bar item to click.
    if runs.is_empty() {
        runs.push(TrayTitleRun::new("额度"));
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
    if shorts.is_empty() {
        "未选择来源".to_owned()
    } else {
        shorts.join(" · ")
    }
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
    set_tray(
        tray()
            .icon(TRAY_ICON)
            .title(tray_runs(store))
            .tooltip(format!("AI 额度：{}", enabled_shorts(&config)))
            .on_click(move |event| toggle_panel(store, event.bounds, false))
            .menu([
                MenuItem::new("刷新").on_select(move || refresh(store)),
                provider_menu(store),
                MenuItem::new("面板").on_select(move || toggle_panel(store, None, false)),
                MenuItem::separator(),
                MenuItem::new("退出").on_select(quit),
            ]),
    );
    // NSH sets its one-line title and transparent icon first; AppKit gets
    // the compact two-line, icon-less item right after.
    #[cfg(target_os = "macos")]
    set_timeout(Duration::from_millis(80), move || {
        // The task is queued; dropping the future only drops its result.
        drop(run_on_main_thread(move || macos_tray::apply(&macos_title)));
    });
}

fn glass_attributes(pinned: bool) -> WindowAttributes {
    let mut attributes = WindowAttributes::default();
    attributes.transparent = Some(true);
    // No popover material: NSH 0.5 puts macOS's NSVisualEffectView behind
    // the whole rectangular window, so it shows as a square layer outside
    // the sheet's rounded corners. The sheet's own fill is the material.
    attributes.close_on_blur = Some(!pinned);
    attributes.activate = Some(true);
    attributes
}

/// A tray click closes an open panel and opens a closed one.
fn toggle_panel(store: Store, bounds: Option<Bounds<Points>>, pinned: bool) {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        if panel.state().map(|state| state.visible).unwrap_or(false) {
            panel.close();
            PANEL.with(|slot| *slot.borrow_mut() = None);
            PINNED.with(|slot| slot.set(false));
            return;
        }
    }
    open_panel(store, bounds, pinned);
}

fn close_panel() {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        panel.close();
        PANEL.with(|slot| *slot.borrow_mut() = None);
        PINNED.with(|slot| slot.set(false));
    }
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

fn open_panel(store: Store, bounds: Option<Bounds<Points>>, pinned: bool) {
    PINNED.with(|slot| slot.set(pinned));
    let position = bounds.map(|icon| {
        let x = icon.origin.x.0 + icon.size.width.0 / 2.0 - PANEL_WIDTH / 2.0;
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
        .default_size(size(px(PANEL_WIDTH), px(panel_height(store))))
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
            let again = panel.clone();
            set_timeout(Duration::from_millis(180), move || {
                again.focus();
            });
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
    // Read the list here, not only in the rows: an open panel must rebuild
    // when a fetch lands, and the row signals alone did not repaint it.
    let sources = store.sources.get();
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
                    .children(sources.into_iter().map(card)),
            ),
        )
        .child(footer(store))
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
        .child(
            text("AI 额度")
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    text(move || {
                        let at = store.updated.get();
                        if at <= 0.0 {
                            "未刷新".to_owned()
                        } else {
                            format!("{} 秒前", (((unix_ms() - at) / 1000.0) as u64).min(3600))
                        }
                    })
                    .text_size(px(11.0))
                    .text_color(move || theme().muted_foreground),
                )
                .child(
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
                                    "刷新中"
                                } else {
                                    "刷新"
                                }
                            })
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(move || theme().secondary_foreground),
                        ),
                ),
        )
}

fn card(source: Source) -> impl IntoElement {
    let error = !source.error.is_empty();
    view()
        .flex_col()
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
            text(if error {
                source.error.clone()
            } else {
                source.note.clone()
            })
            .text_size(px(10.0))
            .text_color(if error {
                rgb(RED).into()
            } else {
                theme().muted_foreground
            }),
        )
}

fn metric_row(metric: &quota::Metric) -> impl IntoElement {
    let remaining = metric.remaining;
    let width = PANEL_WIDTH - 12.0 * 2.0 - 12.0 * 2.0;
    let filled = (remaining.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0 * width as f64) as f32;
    view()
        .flex_col()
        .gap(px(3.0))
        .child(
            view()
                .flex_row()
                .justify_between()
                .items_center()
                .child(
                    text(metric.label.clone())
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
                .w(px(width))
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(move || theme().muted)
                .overflow_hidden()
                .child(
                    view()
                        .w(px(filled))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(tone(remaining)),
                ),
        )
        .child(
            text(metric.detail.clone())
                .text_size(px(10.0))
                .text_color(move || theme().muted_foreground),
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
            text(move || enabled_shorts(&store.config.get()))
                .text_size(px(10.0))
                .text_color(move || theme().muted_foreground),
        )
        .child(
            view()
                .px(px(9.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .on_press(move || quit())
                .child(
                    text("退出")
                        .text_size(px(11.0))
                        .text_color(move || theme().muted_foreground),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

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
