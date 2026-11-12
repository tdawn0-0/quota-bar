//! The macOS menu bar item as this app wants it: two compact lines of quota
//! values and no icon.
//!
//! NSH 0.5's tray always puts an image on the status item (a 1×1 transparent
//! one here), which leaves an empty icon-sized gap before the title, and its
//! title is one line in the menu bar's font. After each `set_tray`, this finds
//! the app's own `NSStatusBarButton` through AppKit's window list (the one
//! whose tooltip is ours), drops its image and sets a two-line attributed
//! title in a smaller font. The menu and click handling stay NSH's.

#![cfg(target_os = "macos")]

use objc2::MainThreadMarker;
use objc2::Message;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSButton, NSCellImagePosition, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSTextAlignment, NSView,
};
use objc2_foundation::{NSMutableAttributedString, NSRange, NSString};

/// Applies `title` (two lines, a `\n` between them) to the app's status item.
pub fn apply(title: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    for window in app.windows().iter() {
        let Some(content) = window.contentView() else {
            continue;
        };
        if let Some(button) = find_button(&content) {
            style(&button, title);
            return;
        }
    }
}

/// The status item's button: NSH sets the tooltip, which tells ours from any
/// other status bar window in the app.
fn find_button(view: &NSView) -> Option<Retained<NSButton>> {
    if let Some(button) = view.downcast_ref::<NSButton>()
        && button
            .toolTip()
            .is_some_and(|tip| tip.to_string().contains("AI 额度"))
    {
        return Some(button.retain());
    }
    for child in view.subviews().iter() {
        if let Some(button) = find_button(&child) {
            return Some(button);
        }
    }
    None
}

fn style(button: &NSButton, title: &str) {
    // No image at all: clearing it removes the icon-sized gap.
    button.setImage(None);
    button.setImagePosition(NSCellImagePosition::NoImage);

    let text = NSMutableAttributedString::from_nsstring(&NSString::from_str(title));
    let length = title.encode_utf16().count();
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(NSTextAlignment::Left);
    // SAFETY: the attribute names are AppKit's, their values are the matching
    // objects, and the range is within the string.
    unsafe {
        text.addAttribute_value_range(
            NSFontAttributeName,
            &NSFont::menuBarFontOfSize(10.0),
            NSRange::new(0, length),
        );
        text.addAttribute_value_range(
            NSForegroundColorAttributeName,
            &NSColor::labelColor(),
            NSRange::new(0, length),
        );
        text.addAttribute_value_range(
            NSParagraphStyleAttributeName,
            &paragraph,
            NSRange::new(0, length),
        );
    }
    button.setAttributedTitle(&text);
}
