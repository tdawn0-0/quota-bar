//! Reapply NSH's pixel snapping when a window moves between display scales.

use gtk4::{Window, prelude::*};

/// Called on the GTK thread once its main loop starts. Watch existing windows
/// and panels opened later, without changing their size or visibility.
pub fn install() {
    let windows = Window::toplevels();
    windows.connect_items_changed(|windows, position, _, added| {
        for index in position..position + added {
            if let Some(window) = windows.item(index).and_downcast::<Window>() {
                watch_window(&window);
            }
        }
    });
    for index in 0..windows.n_items() {
        if let Some(window) = windows.item(index).and_downcast::<Window>() {
            watch_window(&window);
        }
    }
}

fn watch_window(window: &Window) {
    if window.is_realized() {
        watch_surface(window);
    }
    window.connect_realize(watch_surface);
}

fn watch_surface(window: &Window) {
    let Some(surface) = window.surface() else {
        return;
    };
    let window = window.downgrade();
    // The GDK surface exposes fractional changes (e.g. 150% -> 170%) that
    // GtkWidget's integer scale-factor notification cannot distinguish.
    surface.connect_scale_notify(move |_| {
        if let Some(window) = window.upgrade() {
            reallocate(window.upcast_ref());
        }
    });
}

fn reallocate(widget: &gtk4::Widget) {
    // NSH 0.5 snaps child transforms during allocation. A scale change alone
    // leaves logical sizes unchanged, so GTK can retain the old transforms.
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        reallocate(&current);
    }
    widget.queue_allocate();
    widget.queue_draw();
}
