//! The panel's text in the two languages it speaks: Chinese as written, and
//! English for the rest of the world.
//!
//! The choice comes from `QUOTA_BAR_LANG` (`zh` / `en`), else the config
//! file's `language`, else the system's UI language: English when that is
//! English, Chinese otherwise (the app's first audience). Changing it in the
//! settings writes the config and reopens the panel, so every string is made
//! again in the new language.

use std::sync::atomic::{AtomicU8, Ordering};

use nsh_core::time::unix_ms;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

/// 0: not chosen yet, 1: Chinese, 2: English.
static CHOSEN: AtomicU8 = AtomicU8::new(0);

/// Fixes the language for this run: an explicit choice, else what the system
/// says, else Chinese.
pub fn choose(explicit: Option<Lang>) {
    let lang = explicit.unwrap_or_else(detect);
    CHOSEN.store(
        match lang {
            Lang::Zh => 1,
            Lang::En => 2,
        },
        Ordering::Relaxed,
    );
}

pub fn lang() -> Lang {
    match CHOSEN.load(Ordering::Relaxed) {
        2 => Lang::En,
        _ => Lang::Zh,
    }
}

pub fn is_english() -> bool {
    lang() == Lang::En
}

/// `zh` or `en` in `QUOTA_BAR_LANG`, for screenshots and tests.
fn from_env() -> Option<Lang> {
    let value = std::env::var("QUOTA_BAR_LANG").ok()?;
    let value = value.trim().to_ascii_lowercase();
    if value.starts_with("en") {
        Some(Lang::En)
    } else if value.starts_with("zh") {
        Some(Lang::Zh)
    } else {
        None
    }
}

fn detect() -> Lang {
    if let Some(lang) = from_env() {
        return lang;
    }
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()));
    if let Some(locale) = locale {
        let locale = locale.to_ascii_lowercase();
        if locale.starts_with("en") {
            return Lang::En;
        }
        if locale.starts_with("zh") {
            return Lang::Zh;
        }
    }
    // macOS GUI apps usually have no LANG; its list of preferred languages
    // is one cheap read.
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        if let Some(first) = text.split('"').nth(1) {
            if first.starts_with("en") {
                return Lang::En;
            }
            if first.starts_with("zh") {
                return Lang::Zh;
            }
        }
    }
    Lang::Zh
}

/// The Chinese text, or the English one.
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    if is_english() { en } else { zh }
}

/// `22 秒前` said the way a person would: 刚刚, 3 分钟前, 1 小时前.
pub fn ago(at_ms: f64) -> String {
    let seconds = ((unix_ms() - at_ms) / 1000.0).max(0.0);
    if seconds < 45.0 {
        return tr("刚刚", "just now").to_owned();
    }
    let minutes = (seconds / 60.0).round() as u64;
    if minutes < 60 {
        return if is_english() {
            format!("{minutes} min ago")
        } else {
            format!("{minutes} 分钟前")
        };
    }
    let hours = (seconds / 3600.0).round() as u64;
    if hours < 24 {
        return if is_english() {
            format!("{hours} h ago")
        } else {
            format!("{hours} 小时前")
        };
    }
    let days = (seconds / 86_400.0).round() as u64;
    if is_english() {
        format!("{days} d ago")
    } else {
        format!("{days} 天前")
    }
}

/// A window's label in the panel's language (`会话`, `5 小时`, `周 (all models)`).
pub fn metric_label(label: &str) -> String {
    if !is_english() {
        return label.to_owned();
    }
    let mut text = label.to_owned();
    for (zh, en) in [
        ("3P 周额度", "3P weekly"),
        ("3P 5 小时", "3P 5 hours"),
        ("本周额度", "This week"),
        ("本月额度", "This month"),
        ("滚动窗口", "Rolling window"),
        ("5 小时", "5 hours"),
        ("周额度", "Weekly"),
        ("月额度", "Monthly"),
        ("会话", "Session"),
        ("周", "Week"),
        ("额度", "Quota"),
    ] {
        text = text.replace(zh, en);
    }
    text
}

/// A reset line in the panel's language (`1 天 18 小时后重置`, `重置 11-16 08:00`).
pub fn metric_detail(detail: &str) -> String {
    if !is_english() {
        return detail.to_owned();
    }
    if detail == "即将重置" {
        return "resetting".to_owned();
    }
    if detail == "当前不适用" {
        return "Not applicable".to_owned();
    }
    if let Some(rest) = detail.strip_prefix("登录凭据 ") {
        return format!("Login expires {}", rest.trim_end_matches(" 到期"));
    }
    if let Some(rest) = detail.strip_prefix("重置 ") {
        return format!("resets {rest}");
    }
    if let Some(rest) = detail.strip_suffix("后重置") {
        let (mut number, mut parts) = (String::new(), Vec::new());
        for ch in rest.chars() {
            if ch.is_ascii_digit() {
                number.push(ch);
            } else if ch == '天' {
                parts.push(format!("{}d", std::mem::take(&mut number)));
            } else if ch == '时' {
                parts.push(format!("{}h", std::mem::take(&mut number)));
            } else if ch == '钟' {
                parts.push(format!("{}m", std::mem::take(&mut number)));
            }
        }
        if !parts.is_empty() {
            return format!("resets in {}", parts.join(" "));
        }
    }
    detail.to_owned()
}

/// A card's final line (`登录凭据 … 到期` and the like).
pub fn note(note: &str) -> String {
    metric_detail(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The language is process-wide, so one test owns it.
    #[test]
    fn texts_follow_the_chosen_language() {
        choose(Some(Lang::Zh));
        assert_eq!(tr("额度", "Quota"), "额度");
        assert_eq!(ago(unix_ms()), "刚刚");
        assert_eq!(ago(unix_ms() - 180_000.0), "3 分钟前");
        choose(Some(Lang::En));
        assert_eq!(tr("额度", "Quota"), "Quota");
        assert_eq!(ago(unix_ms()), "just now");
        assert_eq!(ago(unix_ms() - 180_000.0), "3 min ago");
        assert_eq!(ago(unix_ms() - 7_200_000.0), "2 h ago");
        assert_eq!(metric_label("周 (all models)"), "Week (all models)");
        assert_eq!(metric_label("5 小时"), "5 hours");
        assert_eq!(metric_detail("1 天 18 小时后重置"), "resets in 1d 18h");
        assert_eq!(metric_detail("4 小时后重置"), "resets in 4h");
        assert_eq!(metric_detail("重置 11-16 08:00"), "resets 11-16 08:00");
    }
}
