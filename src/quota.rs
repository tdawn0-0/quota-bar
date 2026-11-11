//! Quota collectors: each source runs its CLI or API and returns display rows.

use std::process::Command;

use nsh_core::prelude::{HttpRequest, fetch};
use serde_json::Value;

/// One quota window (session, week, month) of a source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metric {
    pub label: String,
    /// Remaining percent, 0–100. `None` when the window does not apply.
    pub remaining: Option<f64>,
    /// Reset time, already said in words.
    pub detail: String,
}

/// Everything the panel and the menu bar need about one tool.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source {
    /// Two-letter label used in the menu bar.
    pub short: &'static str,
    pub title: String,
    pub metrics: Vec<Metric>,
    pub note: String,
    pub error: String,
}

impl Source {
    pub fn empty(short: &'static str, title: &str) -> Self {
        Self {
            short,
            title: title.to_owned(),
            ..Self::default()
        }
    }

    /// The tightest remaining window, which is what runs out first.
    pub fn headline(&self) -> Option<f64> {
        self.metrics
            .iter()
            .filter_map(|m| m.remaining)
            .reduce(f64::min)
    }
}

// ------------------------------------------------------------------ helpers

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let line = detail.lines().last().unwrap_or("").trim().to_owned();
        return Err(format!(
            "{program}: exited {}{}",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_default(),
            if line.is_empty() {
                String::new()
            } else {
                format!(" ({line})")
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn home(relative: &str) -> String {
    let dir = std::env::var("HOME").unwrap_or_default();
    format!("{dir}/{relative}")
}

/// Seconds until `timestamp` (RFC 3339), said as `2 天 15 小时后重置`.
fn reset_in(timestamp: &str, now_ms: u64) -> String {
    let Some(at) = parse_rfc3339(timestamp) else {
        return String::new();
    };
    let now = (now_ms / 1000) as i64;
    let seconds = at - now;
    if seconds <= 0 {
        return "即将重置".to_owned();
    }
    let minutes = seconds / 60;
    let days = minutes / 1440;
    let hours = (minutes % 1440) / 60;
    if days > 0 {
        format!("{days} 天 {hours} 小时后重置")
    } else if hours > 0 {
        format!("{hours} 小时后重置")
    } else {
        format!("{} 分钟后重置", minutes.max(1))
    }
}

fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.trim().as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<i64> { text.get(from..from + len)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 2)? as u32, num(8, 2)? as u32);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut rest = 19;
    if text.get(rest..rest + 1) == Some(".") {
        rest += 1;
        while text
            .as_bytes()
            .get(rest)
            .is_some_and(|b| b.is_ascii_digit())
        {
            rest += 1;
        }
    }
    let offset = match text.get(rest..)? {
        "Z" | "z" => 0,
        rest => {
            let body = rest.trim_start_matches(['T', ' ']);
            let sign = if body.starts_with('+') { 1 } else { -1 };
            let (hh, mm) = body.get(1..)?.split_once(':')?;
            sign * (hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn window_label(window: &str) -> String {
    match window {
        "weekly" => "周额度".to_owned(),
        "5h" => "5 小时".to_owned(),
        "rolling" => "滚动窗口".to_owned(),
        "monthly" => "本月".to_owned(),
        other => other.to_owned(),
    }
}

// ------------------------------------------------------------- antigravity

pub fn antigravity(now_ms: u64) -> Source {
    let mut source = Source::empty("ag", "Antigravity");
    match collect_antigravity(now_ms) {
        Ok((metrics, credits)) => {
            source.metrics = metrics;
            source.note = credits;
        }
        Err(error) => source.error = error,
    }
    source
}

fn collect_antigravity(now_ms: u64) -> Result<(Vec<Metric>, String), String> {
    let agy = home(".local/bin/agy");
    let quota = run(&agy, &["-p", "/quota", "--output-format", "json"])?;
    let quota: Value = serde_json::from_str(&quota).map_err(|e| format!("agy: {e}"))?;
    if quota.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        return Err(format!(
            "agy: status {}",
            quota
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
    }

    let mut metrics = Vec::new();
    let empty = Vec::new();
    let groups = quota
        .pointer("/command/data/groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for group in groups {
        let name = group.get("name").and_then(Value::as_str).unwrap_or("");
        let short = if name.to_ascii_lowercase().contains("gemini") {
            ""
        } else {
            "3P "
        };
        for bucket in group
            .get("buckets")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
        {
            let disabled = bucket
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let remaining = bucket
                .get("remaining_fraction")
                .and_then(Value::as_f64)
                .map(|f| f * 100.0);
            let window = bucket.get("window").and_then(Value::as_str).unwrap_or("");
            let reset = bucket
                .get("reset_time")
                .and_then(Value::as_str)
                .unwrap_or("");
            metrics.push(Metric {
                label: format!("{short}{}", window_label(window)),
                remaining: if disabled { None } else { remaining },
                detail: if disabled {
                    "当前不适用".to_owned()
                } else {
                    reset_in(reset, now_ms)
                },
            });
        }
    }
    if metrics.is_empty() {
        return Err("agy: no quota buckets".to_owned());
    }

    let credits = run(&agy, &["-p", "/credits", "--output-format", "json"])
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .pointer("/command/data/remaining_credits")
                .and_then(Value::as_i64)
        })
        .map(|credits| format!("AI credits {credits}"))
        .unwrap_or_default();

    Ok((metrics, credits))
}

// ------------------------------------------------------------------ claude

pub fn claude() -> Source {
    let mut source = Source::empty("cc", "Claude Code");
    match collect_claude() {
        Ok((metrics, note)) => {
            source.metrics = metrics;
            source.note = note;
        }
        Err(error) => source.error = error,
    }
    source
}

fn collect_claude() -> Result<(Vec<Metric>, String), String> {
    let claude = home(".local/bin/claude");
    let text = run(&claude, &["-p", "/usage", "--output-format", "json"])?;
    let envelope: Value = serde_json::from_str(&text).map_err(|e| format!("claude: {e}"))?;
    let body = envelope.get("result").and_then(Value::as_str).unwrap_or("");
    if envelope
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!("claude: {}", body.trim()));
    }

    let mut metrics = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Current ") else {
            continue;
        };
        let Some((label, detail)) = rest.split_once(':') else {
            continue;
        };
        let used = detail.find('%').and_then(|index| {
            let head = &detail[..index];
            let start = head
                .rfind(|c: char| !c.is_ascii_digit() && c != '.')
                .map(|i| i + 1)
                .unwrap_or(0);
            head[start..].parse::<f64>().ok()
        });
        let label = label.replace("week", "周").replace("session", "会话");
        let reset = detail
            .split_once("resets ")
            .map(|(_, tail)| {
                let tail = tail.trim();
                let tail = tail.split_once(" (").map(|(head, _)| head).unwrap_or(tail);
                tail.replace(" at ", " ")
            })
            .unwrap_or_default();
        metrics.push(Metric {
            label,
            remaining: used.map(|used| (100.0 - used).max(0.0)),
            detail: if reset.is_empty() {
                String::new()
            } else {
                format!("重置 {reset}")
            },
        });
    }
    if metrics.is_empty() {
        return Err("claude: no limits in output".to_owned());
    }

    let note = envelope
        .get("credential_expires_at")
        .and_then(Value::as_str)
        .map(|at| format!("登录凭据 {} 到期", local_stamp(at)))
        .unwrap_or_default();
    Ok((metrics, note))
}

// ---------------------------------------------------------------- opencode

pub async fn opencode() -> Source {
    let mut source = Source::empty("oc", "OpenCode Go");
    match fetch_opencode().await {
        Ok((metrics, note)) => {
            source.metrics = metrics;
            source.note = note;
        }
        Err(error) => source.error = error,
    }
    source
}

async fn fetch_opencode() -> Result<(Vec<Metric>, String), String> {
    let key = opencode_key()?;
    let request = HttpRequest::get("https://opencode.ai/zen/go/v1/usage")
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json");
    let response = fetch(request).await.map_err(|e| format!("opencode: {e}"))?;
    let status = response.status();
    let body = response.text();
    if status == 401 {
        return Err("opencode: key 无效（401），重新执行 opencode auth login".to_owned());
    }
    if status == 403 {
        return Err("opencode: key 没有额度接口权限（403）".to_owned());
    }
    if !(200..300).contains(&status) {
        return Err(format!("opencode: HTTP {status}"));
    }
    let value: Value = serde_json::from_str(&body).map_err(|e| format!("opencode: {e}"))?;

    let mut metrics = Vec::new();
    for (key, label) in [
        ("rolling", "滚动窗口"),
        ("weekly", "本周额度"),
        ("monthly", "本月额度"),
    ] {
        let Some(window) = value.pointer(&format!("/usage/{key}")) else {
            continue;
        };
        let used = window.get("percent").and_then(Value::as_f64);
        let limited = window.get("status").and_then(Value::as_str) == Some("rate-limited");
        let reset = window.get("resetsAt").and_then(Value::as_str).unwrap_or("");
        metrics.push(Metric {
            label: label.to_owned(),
            remaining: used.map(|used| (100.0 - used).max(0.0)),
            detail: if limited {
                "已限流".to_owned()
            } else if reset.is_empty() {
                String::new()
            } else {
                format!("重置 {}", local_stamp(reset))
            },
        });
    }
    if metrics.is_empty() {
        return Err("opencode: no windows in response".to_owned());
    }
    Ok((metrics, String::new()))
}

fn opencode_key() -> Result<String, String> {
    if let Ok(key) = std::env::var("OPENCODE_GO_KEY") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_owned());
        }
    }
    let exported = run("opencode", &["auth", "export"])?;
    let creds: Value =
        serde_json::from_str(&exported).map_err(|e| format!("opencode auth: {e}"))?;
    let empty = Vec::new();
    for cred in creds.as_array().unwrap_or(&empty) {
        let integration = cred
            .get("integrationID")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !integration.contains("opencode") {
            continue;
        }
        let value = cred.get("value").unwrap_or(&Value::Null);
        if value.get("type").and_then(Value::as_str) != Some("key") {
            continue;
        }
        if let Some(key) = value.get("key").and_then(Value::as_str) {
            return Ok(key.to_owned());
        }
    }
    Err("opencode: 没有可用的 key（opencode auth login 或设置 OPENCODE_GO_KEY）".to_owned())
}

/// `2026-11-16T00:00:00.000Z` -> `11-16 08:00`
fn local_stamp(timestamp: &str) -> String {
    let Some(seconds) = parse_rfc3339(timestamp) else {
        return timestamp.to_owned();
    };
    let offset = local_offset_seconds();
    let shifted = seconds + offset;
    let days = shifted.div_euclid(86_400);
    let rem = shifted.rem_euclid(86_400);
    let (_, month, day) = civil_from_days(days);
    format!(
        "{:02}-{:02} {:02}:{:02}",
        month,
        day,
        rem / 3600,
        (rem % 3600) / 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The machine's offset from UTC in seconds, read from `date +%z`.
fn local_offset_seconds() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        run("date", &["+%z"])
            .ok()
            .and_then(|text| {
                let text = text.trim().to_owned();
                let sign = if text.starts_with('-') { -1 } else { 1 };
                let (hh, mm) = text.get(1..)?.split_at(2);
                Some(sign * (hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60))
            })
            .unwrap_or(0)
    })
}
