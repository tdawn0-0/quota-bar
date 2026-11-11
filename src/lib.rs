//! Quota Bar: a menu bar app showing the remaining quota of the local AI CLIs.
//!
//! Resource budget: nothing polls in the background. Data is fetched once at
//! launch, again when the panel is opened after two minutes, and whenever the
//! refresh control or menu item asks. The panel is a popup window that closes
//! on blur, so an idle app is one tray item and a few kilobytes of state.

mod quota;

use std::cell::RefCell;
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
const PANEL_HEIGHT: f32 = 656.0;

const BLUE: u32 = 0x0a84ff;
const AMBER: u32 = 0xff9f0a;
const RED: u32 = 0xff3b30;

thread_local! {
    /// The open panel, so a second tray click closes it.
    static PANEL: RefCell<Option<WindowRef>> = const { RefCell::new(None) };
}

/// The three signals the app shares between its tray handler and its panel.
#[derive(Clone, Copy)]
struct Store {
    sources: Signal<Vec<Source>>,
    updated: Signal<f64>,
    busy: Signal<bool>,
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
    };

    // One fetch at launch, then only on demand.
    refresh(store);

    effect(move || set_tray_menu(store));

    // Switching to another app closes the panel, as a menu bar extra should.
    effect(move || {
        if lifecycle() != Lifecycle::Active {
            close_panel();
        }
    });

    // Steady background refresh; opening the panel only reads what is there.
    let interval = std::env::var("QUOTA_BAR_REFRESH_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs >= 5)
        .unwrap_or(300);
    set_interval(Duration::from_secs(interval), move || refresh(store));

    // `QUOTA_BAR_OPEN_PANEL=1` opens the panel at launch, for screenshots.
    if std::env::var_os("QUOTA_BAR_OPEN_PANEL").is_some() {
        let pinned = std::env::var_os("QUOTA_BAR_PIN_PANEL").is_some();
        set_timeout(Duration::from_millis(700), move || {
            toggle_panel(store, None, pinned)
        });
    }

    // The main window stays hidden (`nsh.json` `defaultVisible: false`); it
    // only exists so the tray has an owner. The panel is a popup of its own.
    view().w_full().h_full()
}

/// Collects every source and updates the signals. Runs at most once at a time.
fn refresh(store: Store) {
    if store.busy.get() {
        return;
    }
    store.busy.set(true);
    let now = unix_ms() as u64;
    spawn(async move {
        // All three start at once; each lands in the panel as soon as it is
        // ready, so the slow CLI never holds the quick one back.
        let agy = blocking(move || quota::antigravity(now));
        let claude = blocking(quota::claude);

        let opencode = quota::opencode().await;
        upsert(store, opencode);
        if let Ok(source) = claude.await {
            upsert(store, source);
        }
        if let Ok(source) = agy.await {
            upsert(store, source);
        }
        store.updated.set(unix_ms());
        store.busy.set(false);
    });
}

const ORDER: [&str; 3] = ["ag", "cc", "oc"];

/// Replaces one source in the list, keeping the tray order stable, and keeps
/// the last numbers of a source that failed.
fn upsert(store: Store, source: Source) {
    let mut sources = store.sources.get();
    let source = keep_last(sources.clone(), source);
    match sources.iter_mut().find(|s| s.short == source.short) {
        Some(slot) => *slot = source,
        None => sources.push(source),
    }
    sources.sort_by_key(|s| {
        ORDER
            .iter()
            .position(|k| *k == s.short)
            .unwrap_or(usize::MAX)
    });
    store.sources.set(sources);
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

fn tray_runs(store: Store) -> Vec<TrayTitleRun> {
    let mut runs = Vec::new();
    for (index, source) in store.sources.get().iter().enumerate() {
        if index > 0 {
            runs.push(TrayTitleRun::new("  "));
        }
        match source.headline() {
            Some(remaining) => {
                runs.push(TrayTitleRun::new(format!(
                    "{} {:.0}%",
                    source.short, remaining
                )));
            }
            None => runs.push(TrayTitleRun::new(source.short.to_owned())),
        }
    }
    runs
}

fn set_tray_menu(store: Store) {
    set_tray(
        tray()
            .icon("data:image/svg+xml,%3Csvg xmlns=%27http://www.w3.org/2000/svg%27 width=%271%27 height=%271%27/%3E")
            .title(tray_runs(store))
            .tooltip("AI 额度：ag / cc / oc")
            .on_click(move |event| toggle_panel(store, event.bounds, false))
            .menu([
                MenuItem::new("刷新").on_select(move || refresh(store)),
                MenuItem::new("面板").on_select(move || toggle_panel(store, None, false)),
                MenuItem::separator(),
                MenuItem::new("退出").on_select(quit),
            ]),
    );
}

fn glass_attributes() -> WindowAttributes {
    let mut attributes = WindowAttributes::default();
    attributes.transparent = Some(true);
    attributes.backdrop = Some(nsh_core::desktop::Backdrop::Popover);
    attributes.close_on_blur = Some(false);
    attributes
}

/// A tray click closes an open panel and opens a closed one.
fn toggle_panel(store: Store, bounds: Option<Bounds<Points>>, pinned: bool) {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        if panel.state().map(|state| state.visible).unwrap_or(false) {
            panel.close();
            PANEL.with(|slot| *slot.borrow_mut() = None);
            return;
        }
    }
    open_panel(store, bounds, pinned);
}

fn close_panel() {
    if let Some(panel) = PANEL.with(|slot| slot.borrow().clone()) {
        panel.close();
        PANEL.with(|slot| *slot.borrow_mut() = None);
    }
}

fn open_panel(store: Store, bounds: Option<Bounds<Points>>, pinned: bool) {
    let position = bounds.map(|icon| {
        let x = icon.origin.x.0 + icon.size.width.0 / 2.0 - PANEL_WIDTH / 2.0;
        let y = icon.origin.y.0 + icon.size.height.0 + 6.0;
        WindowPosition::At(point(px(x.max(8.0)), px(y)))
    });

    // The panel draws its own rounded corners on a transparent window, the
    // way a popover looks next to the menu bar.
    let mut attributes = WindowAttributes::default();
    attributes.transparent = Some(true);
    attributes.close_on_blur = Some(!pinned);
    attributes.activate = Some(true);

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
        .attributes(glass_attributes())
        .default_size(size(px(PANEL_WIDTH), px(PANEL_HEIGHT)))
        .content(move || panel(store));
    if let Some(position) = debug_at.or(position) {
        builder = builder.default_position(position);
    }
    match open_window(builder) {
        Some(panel) => {
            PANEL.with(|slot| *slot.borrow_mut() = Some(panel.clone()));
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
    view()
        .flex_1()
        .flex_col()
        .w_full()
        .rounded(px(14.0))
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
                    .child(each(
                        store.sources,
                        |source| source.short,
                        |source, _| card(source.get()),
                    )),
            ),
        )
        .child(footer())
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

fn footer() -> impl IntoElement {
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
            text("ag · cc · oc")
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
