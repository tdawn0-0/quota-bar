//! Quota Bar: a menu bar app showing the remaining quota of the local AI CLIs.
//!
//! Resource budget: nothing polls in the background. Data is fetched once at
//! launch, again when the panel is opened after two minutes, and whenever the
//! refresh control or menu item asks. The panel is a popup window that closes
//! itself, so an idle app is one tray item and a few kilobytes of state.

mod quota;

use std::time::Duration;

use nsh_base::theme;
use nsh_core::color::rgb;
use nsh_core::desktop::{TrayTitleRun, WindowKind, WindowPosition};
use nsh_core::element::desktop::{MenuItem, open_window, quit, set_tray, tray, window};
use nsh_core::geometry::{Bounds, Points, point, px, size};
use nsh_core::prelude::*;
use nsh_core::time::unix_ms;

use quota::Source;

const PANEL_WIDTH: f32 = 352.0;
const PANEL_HEIGHT: f32 = 660.0;
const STALE_MS: f64 = 120_000.0;

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

    // `QUOTA_BAR_OPEN_PANEL=1` opens the panel at launch, for screenshots.
    if std::env::var_os("QUOTA_BAR_OPEN_PANEL").is_some() {
        set_timeout(Duration::from_millis(700), move || open_panel(store, None));
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
        let agy = blocking(move || quota::antigravity(now));
        let claude = blocking(quota::claude);
        let opencode = quota::opencode().await;

        let fetched = vec![
            agy.await.unwrap_or_else(|_| quota::Source::empty("ag", "Antigravity")),
            claude.await.unwrap_or_else(|_| quota::Source::empty("cc", "Claude Code")),
            opencode,
        ];
        store.sources.set(merge(store.sources.get(), fetched));
        store.updated.set(unix_ms());
        store.busy.set(false);
    });
}

/// Keeps the last numbers of a source that failed, and says why it failed.
fn merge(previous: Vec<Source>, fetched: Vec<Source>) -> Vec<Source> {
    fetched
        .into_iter()
        .map(|mut source| {
            if !source.error.is_empty() {
                if let Some(old) = previous.iter().find(|s| s.short == source.short) {
                    if !old.metrics.is_empty() {
                        source.metrics = old.metrics.clone();
                        source.note = old.note.clone();
                    }
                }
            }
            source
        })
        .collect()
}

fn tray_runs(store: Store) -> Vec<TrayTitleRun> {
    let mut runs = Vec::new();
    for (index, source) in store.sources.get().iter().enumerate() {
        if index > 0 {
            runs.push(TrayTitleRun::new("  "));
        }
        match source.headline() {
            Some(remaining) => {
                runs.push(TrayTitleRun::new(format!("{} {:.0}%", source.short, remaining)));
            }
            None => runs.push(TrayTitleRun::new(source.short.to_owned())),
        }
    }
    runs
}

fn set_tray_menu(store: Store) {
    set_tray(
        tray()
            .title(tray_runs(store))
            .tooltip("AI 额度：ag / cc / oc")
            .on_click(move |event| open_panel(store, event.bounds))
            .menu([
                MenuItem::new("刷新").on_select(move || refresh(store)),
                MenuItem::new("面板").on_select(move || open_panel(store, None)),
                MenuItem::separator(),
                MenuItem::new("退出").on_select(quit),
            ]),
    );
}

fn open_panel(store: Store, bounds: Option<Bounds<Points>>) {
    let position = bounds.map(|icon| {
        let x = icon.origin.x.0 + icon.size.width.0 / 2.0 - PANEL_WIDTH / 2.0;
        let y = icon.origin.y.0 + icon.size.height.0 + 6.0;
        WindowPosition::At(point(px(x.max(8.0)), px(y)))
    });

    let mut builder = window()
        .id("panel")
        .kind(WindowKind::Popup)
        .default_size(size(px(PANEL_WIDTH), px(PANEL_HEIGHT)))
        .content(move || panel(store));
    if let Some(position) = position {
        builder = builder.default_position(position);
    }
    open_window(builder);

    if unix_ms() - store.updated.get() > STALE_MS {
        refresh(store);
    }
}



// ---------------------------------------------------------------------- UI

fn tone(remaining: Option<f64>) -> AdaptiveColor {
    match remaining {
        Some(value) if value >= 50.0 => rgb(0x2f9e44).into(),
        Some(value) if value >= 15.0 => rgb(0xd98e04).into(),
        Some(_) => rgb(0xd92d20).into(),
        None => theme().muted_foreground,
    }
}

fn panel(store: Store) -> impl IntoElement {
    view()
        .flex_1()
        .flex_col()
        .w_full()
        .bg(move || theme().popover)
        .text_color(move || theme().popover_foreground)
        .child(header(store))
        .child(
            scroll().flex_1().w_full().child(
                view()
                    .flex_col()
                    .w_full()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .pb(px(10.0))
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
        .px(px(12.0))
        .pt(px(12.0))
        .pb(px(8.0))
        .gap(px(8.0))
        .child(text("AI 额度").text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    text(move || {
                        let at = store.updated.get();
                        if at <= 0.0 {
                            "尚未刷新".to_owned()
                        } else {
                            format!("{} 秒前", (((unix_ms() - at) / 1000.0) as u64).min(3600))
                        }
                    })
                    .text_size(px(11.0))
                    .text_color(move || theme().muted_foreground),
                )
                .child(
                    view()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .bg(move || theme().secondary)
                        .on_press(move || refresh(store))
                        .child(
                            text(move || if store.busy.get() { "刷新中" } else { "刷新" })
                                .text_size(px(11.0))
                                .text_color(move || theme().secondary_foreground),
                        ),
                ),
        )
}

fn card(source: Source) -> impl IntoElement {
    let muted = theme().muted_foreground;
    view()
        .flex_col()
        .gap(px(8.0))
        .p(px(10.0))
        .rounded(px(10.0))
        .bg(move || theme().card)
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
                        Some(value) => format!("{value:.0}% 剩余"),
                        None => "—".to_owned(),
                    })
                    .text_size(px(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tone(source.headline())),
                ),
        )
        .child(
            view()
                .flex_col()
                .gap(px(6.0))
                .children(source.metrics.iter().map(metric_row)),
        )
        .child(
            text(if !source.error.is_empty() {
                source.error.clone()
            } else {
                source.note.clone()
            })
            .text_size(px(10.0))
            .text_color(if source.error.is_empty() {
                muted
            } else {
                rgb(0xd92d20).into()
            }),
        )
}

fn metric_row(metric: &quota::Metric) -> impl IntoElement {
    let remaining = metric.remaining;
    let width = PANEL_WIDTH - 12.0 * 2.0 - 10.0 * 2.0;
    let filled = (remaining.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0 * width as f64) as f32;
    view()
        .flex_col()
        .gap(px(3.0))
        .child(
            view()
                .flex_row()
                .justify_between()
                .items_center()
                .child(text(metric.label.clone()).text_size(px(11.0)))
                .child(
                    text(match remaining {
                        Some(value) => format!("{value:.0}%"),
                        None => "不适用".to_owned(),
                    })
                    .text_size(px(11.0))
                    .text_color(tone(remaining)),
                ),
        )
        .child(
            view()
                .w(px(width))
                .h(px(5.0))
                .rounded(px(2.5))
                .bg(move || theme().muted)
                .overflow_hidden()
                .child(
                    view()
                        .w(px(filled))
                        .h(px(5.0))
                        .rounded(px(2.5))
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
        .px(px(12.0))
        .py(px(8.0))
        .child(
            text("ag · cc · oc")
                .text_size(px(10.0))
                .text_color(move || theme().muted_foreground),
        )
        .child(
            view()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(move || theme().muted)
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

        let merged = merge(vec![old], vec![failed]);
        assert_eq!(merged[0].metrics.len(), 1);
        assert_eq!(merged[0].metrics[0].remaining, Some(40.0));
        assert_eq!(merged[0].note, "credits 0");
        assert!(merged[0].error.contains("not logged in"));
    }


}
