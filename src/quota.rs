//! Quota collectors: each source runs its CLI or API and returns display rows.

use std::future::Future;
use std::io::{BufRead, BufReader, Write};
use std::pin::Pin;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nsh_core::prelude::{HttpRequest, blocking, fetch};
use nsh_core::time::unix_ms;
use serde_json::{Value, json};

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

// --------------------------------------------------------------- providers

/// One source the app knows how to read.
pub struct Provider {
    /// Stable id in the config file; it never changes.
    pub id: &'static str,
    /// Two-letter label used in the menu bar.
    pub short: &'static str,
    pub title: &'static str,
    pub kind: Kind,
    /// Whether the tool shows up on this machine, so a missing one starts
    /// hidden instead of showing an error.
    pub detect: fn() -> bool,
}

/// How a provider gets its numbers.
pub enum Kind {
    /// Runs a CLI, which blocks its thread until it answers.
    Blocking(fn(u64) -> Source),
    /// Fetches over the network.
    Async(fn() -> Pin<Box<dyn Future<Output = Source>>>),
}

/// Every provider in the order the menu bar and the panel show them. Adding
/// one here is all it takes for the config file and the menu to know it.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "antigravity",
        short: "ag",
        title: "Antigravity",
        kind: Kind::Blocking(antigravity),
        detect: || program_exists("agy"),
    },
    Provider {
        id: "claude",
        short: "cc",
        title: "Claude Code",
        kind: Kind::Blocking(|_now| claude()),
        detect: || program_exists("claude"),
    },
    Provider {
        id: "codex",
        short: "cx",
        title: "Codex",
        kind: Kind::Async(|| Box::pin(codex())),
        detect: || codex_home().join("auth.json").is_file() || program_exists("codex"),
    },
    Provider {
        id: "grok",
        short: "gk",
        title: "Grok Build",
        kind: Kind::Async(|| Box::pin(grok())),
        detect: || grok_home().join("auth.json").is_file(),
    },
    Provider {
        id: "grokbot",
        short: "gb",
        title: "Grok Bot",
        kind: Kind::Async(|| Box::pin(grok_bot())),
        detect: || cursor_access_token().is_ok(),
    },
    Provider {
        id: "opencode",
        short: "oc",
        title: "OpenCode Go",
        kind: Kind::Async(|| Box::pin(opencode())),
        detect: || program_exists("opencode"),
    },
];

impl Provider {
    pub fn detected(&self) -> bool {
        (self.detect)()
    }

    pub fn by_short(short: &str) -> Option<&'static Self> {
        PROVIDERS.iter().find(|provider| provider.short == short)
    }

    /// Its place in the menu bar and the panel.
    pub fn position(short: &str) -> usize {
        PROVIDERS
            .iter()
            .position(|provider| provider.short == short)
            .unwrap_or(usize::MAX)
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

/// Where a CLI usually lives, for a GUI app launched with a small `PATH`.
/// The bare name lets `Command` fall back to `PATH`.
fn find_program(name: &str) -> Option<String> {
    let candidates = [
        home(&format!(".local/bin/{name}")),
        format!("/opt/homebrew/bin/{name}"),
        format!("/usr/local/bin/{name}"),
        format!("/usr/bin/{name}"),
        home(&format!(".npm-global/bin/{name}")),
        home(&format!(".bun/bin/{name}")),
        home(&format!(".volta/bin/{name}")),
        home(&format!(".local/share/pnpm/{name}")),
    ];
    for candidate in candidates {
        if std::path::Path::new(&candidate).is_file() {
            return Some(candidate);
        }
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn program(name: &str) -> String {
    find_program(name).unwrap_or_else(|| name.to_owned())
}

fn program_exists(name: &str) -> bool {
    find_program(name).is_some()
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
    let agy = program("agy");
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
    let claude = program("claude");
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

// -------------------------------------------------------------------- codex

/// Codex's quota as its own CLI reads it: the account's usage endpoint when
/// the stored login still works, otherwise the CLI's app server, which can
/// refresh that login without starting a model turn.
pub async fn codex() -> Source {
    let mut source = Source::empty("cx", "Codex");
    let outcome = match fetch_codex().await {
        Ok(outcome) => Ok(outcome),
        Err(failure) if failure.use_cli => match blocking(collect_codex_cli).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(error)) => Err(format!("{}；CLI: {error}", failure.message)),
            Err(error) => Err(format!("{}；CLI: {error}", failure.message)),
        },
        Err(failure) => Err(failure.message),
    };
    match outcome {
        Ok((metrics, note)) => {
            source.metrics = metrics;
            source.note = note;
        }
        Err(error) => source.error = error,
    }
    source
}

struct CodexFailure {
    message: String,
    /// The CLI refreshes a stale login itself, so it is worth a try.
    use_cli: bool,
}

impl CodexFailure {
    fn cli(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            use_cli: true,
        }
    }

    fn other(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            use_cli: false,
        }
    }
}

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

async fn fetch_codex() -> Result<(Vec<Metric>, String), CodexFailure> {
    let (token, account) = codex_credentials()?;
    let mut request = HttpRequest::get(CODEX_USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("originator", "codex_cli_rs")
        .header("OpenAI-Beta", "responses=experimental");
    if let Some(account) = account {
        request = request.header("chatgpt-account-id", account);
    }
    let response = fetch(request)
        .await
        .map_err(|e| CodexFailure::other(format!("codex: {e}")))?;
    let status = response.status();
    let body = response.text();
    if status == 401 || status == 403 {
        return Err(CodexFailure::cli(
            "codex: 登录已过期（重新执行 codex login）",
        ));
    }
    if !(200..300).contains(&status) {
        return Err(CodexFailure::other(format!("codex: HTTP {status}")));
    }
    let value: Value =
        serde_json::from_str(&body).map_err(|e| CodexFailure::other(format!("codex: {e}")))?;
    codex_from_direct(&value).map_err(CodexFailure::other)
}

fn codex_credentials() -> Result<(String, Option<String>), CodexFailure> {
    let path = codex_home().join("auth.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CodexFailure::cli(format!("codex: {}: {e}", path.display())))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| CodexFailure::other(format!("codex: {}: {e}", path.display())))?;
    let Some(tokens) = value.get("tokens").and_then(Value::as_object) else {
        if value
            .get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .is_some_and(|key| !key.trim().is_empty())
        {
            return Err(CodexFailure::other(
                "codex: API key 登录没有 ChatGPT 额度可查",
            ));
        }
        return Err(CodexFailure::cli("codex: 未登录（执行 codex login）"));
    };
    let token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| CodexFailure::cli("codex: 未登录（执行 codex login）"))?;
    let account = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((token.to_owned(), account))
}

fn codex_home() -> std::path::PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(home(".codex")))
}

fn codex_from_direct(data: &Value) -> Result<(Vec<Metric>, String), String> {
    let limit = data
        .get("rate_limit")
        .filter(|value| value.is_object())
        .ok_or_else(|| "codex: 响应里没有额度".to_owned())?;
    let limited = limit
        .get("limit_reached")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut metrics = Vec::new();
    for (key, fallback) in [
        ("primary_window", "5 小时"),
        ("secondary_window", "本周额度"),
    ] {
        let Some(window) = limit.get(key).filter(|value| value.is_object()) else {
            continue;
        };
        metrics.push(codex_metric(
            window.get("used_percent").and_then(Value::as_f64),
            window.get("limit_window_seconds").and_then(Value::as_i64),
            window.get("reset_at").and_then(Value::as_i64),
            limited,
            fallback,
        ));
    }
    if metrics.is_empty() {
        return Err("codex: 响应里没有额度窗口".to_owned());
    }
    let mut note = Vec::new();
    if let Some(plan) = data.get("plan_type").and_then(Value::as_str) {
        note.push(codex_plan(plan));
    }
    if let Some(credits) = codex_credits(data.get("credits")) {
        note.push(credits);
    }
    Ok((metrics, note.join(" · ")))
}

fn codex_from_cli(result: &Value) -> Result<(Vec<Metric>, String), String> {
    let snapshot = result
        .pointer("/rateLimitsByLimitId/codex")
        .or_else(|| result.get("rateLimits"))
        .filter(|value| value.is_object())
        .ok_or_else(|| "codex: app-server 没有返回额度".to_owned())?;
    let limited = snapshot
        .get("rateLimitReachedType")
        .is_some_and(|value| !value.is_null());
    let mut metrics = Vec::new();
    for (key, fallback) in [("primary", "5 小时"), ("secondary", "本周额度")] {
        let Some(window) = snapshot.get(key).filter(|value| value.is_object()) else {
            continue;
        };
        let seconds = window
            .get("windowDurationMins")
            .and_then(Value::as_i64)
            .map(|minutes| minutes.saturating_mul(60));
        metrics.push(codex_metric(
            window.get("usedPercent").and_then(Value::as_f64),
            seconds,
            window.get("resetsAt").and_then(Value::as_i64),
            limited,
            fallback,
        ));
    }
    if metrics.is_empty() {
        return Err("codex: app-server 没有返回额度窗口".to_owned());
    }
    let mut note = Vec::new();
    if let Some(plan) = snapshot.get("planType").and_then(Value::as_str) {
        note.push(codex_plan(plan));
    }
    if let Some(credits) = codex_credits(snapshot.get("credits")) {
        note.push(credits);
    }
    Ok((metrics, note.join(" · ")))
}

fn codex_metric(
    used: Option<f64>,
    window_seconds: Option<i64>,
    reset_at: Option<i64>,
    limited: bool,
    fallback: &str,
) -> Metric {
    Metric {
        label: window_seconds
            .map(codex_window_label)
            .unwrap_or_else(|| fallback.to_owned()),
        remaining: used.map(|used| (100.0 - used).max(0.0)),
        detail: if limited {
            "已限流".to_owned()
        } else {
            reset_at
                .map(|at| format!("重置 {}", local_stamp_secs(at)))
                .unwrap_or_default()
        },
    }
}

fn codex_window_label(seconds: i64) -> String {
    let minutes = (seconds / 60).max(1);
    match minutes {
        300 => "5 小时".to_owned(),
        10080 => "本周额度".to_owned(),
        minutes if minutes % 1440 == 0 => format!("{} 天", minutes / 1440),
        minutes if minutes % 60 == 0 => format!("{} 小时", minutes / 60),
        minutes => format!("{minutes} 分钟"),
    }
}

fn codex_plan(plan: &str) -> String {
    match plan {
        "prolite" => "Pro Lite".to_owned(),
        "promax" => "Pro Max".to_owned(),
        other => {
            let mut text = other.replace('_', " ");
            if let Some(first) = text.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            text
        }
    }
}

fn codex_credits(credits: Option<&Value>) -> Option<String> {
    let credits = credits?;
    if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
        return Some("credits 无限".to_owned());
    }
    credits
        .get("balance")
        .and_then(Value::as_str)
        .map(|balance| format!("credits {balance}"))
}

/// Asks a fresh app server for the same snapshot over JSON-RPC. It never
/// starts a turn; it only initializes, reads the rate limits, and exits.
fn collect_codex_cli() -> Result<(Vec<Metric>, String), String> {
    let mut child = Command::new(codex_program())
        .args(["app-server", "--listen", "stdio://"])
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("codex: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "codex: app-server stdin".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "codex: app-server stdout".to_owned())?;
    let mut reader = BufReader::new(stdout);

    // The app server can wait on the network; a watchdog ends it so a bad
    // login or a dropped connection cannot hold a refresh forever.
    let done = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let done = Arc::clone(&done);
        let timed_out = Arc::clone(&timed_out);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !done.load(Ordering::Relaxed) {
                if Instant::now() >= deadline {
                    timed_out.store(true, Ordering::Relaxed);
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = child.kill();
            let _ = child.wait();
        })
    };

    let result = codex_app_server_read(&mut stdin, &mut reader);
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();
    if result.is_err() && timed_out.load(Ordering::Relaxed) {
        return Err("codex: 查询超时".to_owned());
    }
    result
}

fn codex_app_server_read(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
) -> Result<(Vec<Metric>, String), String> {
    codex_send(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"clientInfo": {"name": "quota-bar", "version": "0.1.0"}},
        }),
    )?;
    codex_response(reader, 1)?;
    codex_send(stdin, &json!({"jsonrpc": "2.0", "method": "initialized"}))?;
    codex_send(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "account/rateLimits/read",
            "params": {"excludeResetCreditDetails": true},
        }),
    )?;
    let result = codex_response(reader, 2)?;
    codex_from_cli(&result)
}

fn codex_send(stdin: &mut ChildStdin, value: &Value) -> Result<(), String> {
    writeln!(stdin, "{value}").map_err(|e| format!("codex: app-server: {e}"))?;
    stdin.flush().map_err(|e| format!("codex: app-server: {e}"))
}

fn codex_response(reader: &mut BufReader<ChildStdout>, id: u64) -> Result<Value, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("codex: app-server: {e}"))?;
        if read == 0 {
            return Err("codex: app-server 提前退出".to_owned());
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(format!("codex: {message}"));
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

fn codex_program() -> String {
    if let Ok(path) = std::env::var("CODEX_BIN")
        && !path.trim().is_empty()
    {
        return path;
    }
    program("codex")
}

// --------------------------------------------------------------------- grok

/// Grok Build's quota, read from the same billing endpoint its CLI uses.
/// `grok usage --json` currently refuses outside the TUI, but the OIDC login
/// in `~/.grok/auth.json` calls the endpoint directly; an expired access
/// token is refreshed with the stored refresh token and written back.
pub async fn grok() -> Source {
    let mut source = Source::empty("gk", "Grok Build");
    match fetch_grok().await {
        Ok((metrics, note)) => {
            source.metrics = metrics;
            source.note = note;
        }
        Err(error) => source.error = error,
    }
    source
}

const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const GROK_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

struct GrokAuth {
    path: std::path::PathBuf,
    entry: String,
    issuer: String,
    client_id: String,
    key: String,
    refresh_token: Option<String>,
}

async fn fetch_grok() -> Result<(Vec<Metric>, String), String> {
    let mut auth = read_grok_auth()?;
    let (mut status, mut body) = grok_billing(&auth.key).await?;
    if status == 401 || status == 403 {
        refresh_grok(&mut auth).await?;
        (status, body) = grok_billing(&auth.key).await?;
    }
    if status == 401 || status == 403 {
        return Err("grok: 登录已过期（grok login）".to_owned());
    }
    if !(200..300).contains(&status) {
        return Err(format!("grok: HTTP {status}"));
    }
    let value: Value = serde_json::from_str(&body).map_err(|e| format!("grok: {e}"))?;
    grok_from_billing(&value, unix_ms() as u64)
}

fn grok_home() -> std::path::PathBuf {
    std::env::var_os("GROK_HOME")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(home(".grok")))
}

fn read_grok_auth() -> Result<GrokAuth, String> {
    let path = grok_home().join("auth.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("grok: {}: {e}（grok login）", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("grok: {}: {e}", path.display()))?;
    let entries = value
        .as_object()
        .ok_or_else(|| format!("grok: {} 格式不对", path.display()))?;

    let mut chosen: Option<(&String, &Value)> = None;
    for (route, entry) in entries {
        let has_key = entry
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|key| !key.trim().is_empty());
        if !has_key {
            continue;
        }
        let issuer = entry
            .get("oidc_issuer")
            .and_then(Value::as_str)
            .unwrap_or("");
        let created = entry
            .get("create_time")
            .and_then(Value::as_str)
            .unwrap_or("");
        let better = chosen.is_none_or(|(_, old)| {
            let old_issuer = old.get("oidc_issuer").and_then(Value::as_str).unwrap_or("");
            let old_created = old.get("create_time").and_then(Value::as_str).unwrap_or("");
            (issuer == "https://auth.x.ai", created)
                > (old_issuer == "https://auth.x.ai", old_created)
        });
        if better {
            chosen = Some((route, entry));
        }
    }

    let Some((entry, value)) = chosen else {
        return Err("grok: 未登录（grok login）".to_owned());
    };
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| "grok: 未登录（grok login）".to_owned())?
        .to_owned();
    Ok(GrokAuth {
        path,
        entry: entry.clone(),
        issuer: value
            .get("oidc_issuer")
            .and_then(Value::as_str)
            .unwrap_or("https://auth.x.ai")
            .to_owned(),
        client_id: value
            .get("oidc_client_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        refresh_token: value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned),
        key,
    })
}

async fn grok_billing(token: &str) -> Result<(u16, String), String> {
    let request = HttpRequest::get(GROK_BILLING_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json");
    let response = fetch(request).await.map_err(|e| format!("grok: {e}"))?;
    Ok((response.status(), response.text()))
}

async fn refresh_grok(auth: &mut GrokAuth) -> Result<(), String> {
    let Some(refresh) = auth.refresh_token.clone() else {
        return Err("grok: 登录已过期（grok login）".to_owned());
    };
    if auth.issuer != "https://auth.x.ai" || auth.client_id.is_empty() {
        return Err("grok: 登录已过期（grok login）".to_owned());
    }
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        url_encode(&refresh),
        url_encode(&auth.client_id)
    );
    let request = HttpRequest::post(GROK_TOKEN_URL, body)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json");
    let response = fetch(request)
        .await
        .map_err(|e| format!("grok: 刷新登录: {e}"))?;
    let status = response.status();
    let text = response.text();
    if !(200..300).contains(&status) {
        return Err(format!("grok: 刷新登录失败（HTTP {status}）"));
    }
    let value: Value = serde_json::from_str(&text).map_err(|e| format!("grok: 刷新登录: {e}"))?;
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "grok: 刷新登录没有返回 token".to_owned())?;
    let refresh = value.get("refresh_token").and_then(Value::as_str);
    let expires_in = value.get("expires_in").and_then(Value::as_i64);
    save_grok_auth(&auth.path, &auth.entry, access, refresh, expires_in)?;
    auth.key = access.to_owned();
    if let Some(refresh) = refresh {
        auth.refresh_token = Some(refresh.to_owned());
    }
    Ok(())
}

/// Writes the refreshed login back to the CLI's `auth.json`, so the CLI stays
/// signed in too (a refresh can rotate the refresh token).
fn save_grok_auth(
    path: &std::path::Path,
    entry: &str,
    access: &str,
    refresh: Option<&str>,
    expires_in: Option<i64>,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let entry = value
        .get_mut(entry)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "auth.json 里找不到登录条目".to_owned())?;
    entry.insert("key".to_owned(), json!(access));
    if let Some(refresh) = refresh {
        entry.insert("refresh_token".to_owned(), json!(refresh));
    }
    if let Some(seconds) = expires_in {
        let at = unix_ms() as i64 + seconds.saturating_mul(1000);
        entry.insert("expires_at".to_owned(), json!(rfc3339_ms(at.max(0) as u64)));
    }
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())? + "\n";
    let temp = path.with_file_name(format!(".auth.json.quota-bar.{}", std::process::id()));
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", temp.display()))?;
    }
    std::fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn rfc3339_ms(ms: u64) -> String {
    let seconds = (ms / 1000) as i64;
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}000Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        ms % 1000
    )
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn grok_from_billing(data: &Value, now_ms: u64) -> Result<(Vec<Metric>, String), String> {
    let config = data
        .get("config")
        .filter(|value| value.is_object())
        .ok_or_else(|| "grok: 响应里没有额度".to_owned())?;
    let period = config
        .get("currentPeriod")
        .filter(|value| value.is_object());
    let label = match period
        .and_then(|period| period.get("type"))
        .and_then(Value::as_str)
    {
        Some("USAGE_PERIOD_TYPE_WEEKLY") => "本周额度",
        Some("USAGE_PERIOD_TYPE_MONTHLY") => "本月额度",
        Some("USAGE_PERIOD_TYPE_DAILY") => "今日额度",
        _ => "额度",
    };
    let mut metrics = Vec::new();
    if let Some(used) = config.get("creditUsagePercent").and_then(Value::as_f64) {
        let detail = period
            .and_then(|period| period.get("end"))
            .and_then(Value::as_str)
            .map(|end| reset_in(end, now_ms))
            .unwrap_or_default();
        metrics.push(Metric {
            label: label.to_owned(),
            remaining: Some((100.0 - used).max(0.0)),
            detail,
        });
    }
    if let Some(products) = config.get("productUsage").and_then(Value::as_array) {
        for product in products {
            let Some(used) = product.get("usagePercent").and_then(Value::as_f64) else {
                continue;
            };
            let name = product
                .get("product")
                .and_then(Value::as_str)
                .unwrap_or("Grok");
            metrics.push(Metric {
                label: grok_product(name),
                remaining: Some((100.0 - used).max(0.0)),
                detail: String::new(),
            });
        }
    }
    if metrics.is_empty() {
        return Err("grok: 响应里没有额度窗口".to_owned());
    }
    Ok((metrics, String::new()))
}

fn grok_product(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "grokbuild" => "Grok Build".to_owned(),
        "grokchat" => "Grok Chat".to_owned(),
        _ => name.to_owned(),
    }
}

// ----------------------------------------------------------------- grok bot

/// Grok Bot is Cursor's product: its app signs in through Cursor, and
/// Cursor's dashboard reports the weekly allowance. The token is the Cursor
/// app's login, read from its local state database.
pub async fn grok_bot() -> Source {
    let mut source = Source::empty("gb", "Grok Bot");
    match fetch_grok_bot().await {
        Ok((metrics, note)) => {
            source.metrics = metrics;
            source.note = note;
        }
        Err(error) => source.error = error,
    }
    source
}

const GROK_BOT_USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus";

async fn fetch_grok_bot() -> Result<(Vec<Metric>, String), String> {
    let token = cursor_access_token()?;
    let request = HttpRequest::post(GROK_BOT_USAGE_URL, "{}")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("x-cursor-client-type", "grok-bot")
        .header("x-ghost-mode", "true");
    let response = fetch(request).await.map_err(|e| format!("grok bot: {e}"))?;
    let status = response.status();
    let body = response.text();
    if status == 401 || status == 403 {
        return Err("grok bot: Cursor 登录已过期（打开 Cursor 登录）".to_owned());
    }
    if !(200..300).contains(&status) {
        return Err(format!("grok bot: HTTP {status}"));
    }
    let value: Value = serde_json::from_str(&body).map_err(|e| format!("grok bot: {e}"))?;
    grok_bot_from_usage(&value, unix_ms() as u64)
}

fn cursor_access_token() -> Result<String, String> {
    let path = cursor_state_db();
    if !path.is_file() {
        return Err(format!(
            "grok bot: {} 不存在（打开 Cursor 登录）",
            path.display()
        ));
    }
    let path = path.to_string_lossy();
    let text = run(
        &program("sqlite3"),
        &[
            "-readonly",
            "-noheader",
            path.as_ref(),
            "select value from ItemTable where key='cursorAuth/accessToken';",
        ],
    )
    .map_err(|e| format!("grok bot: 读取 Cursor 登录: {e}"))?;
    let text = text.trim();
    if text.is_empty() {
        return Err("grok bot: Cursor 未登录（打开 Cursor 登录）".to_owned());
    }
    let token =
        serde_json::from_str::<String>(text).unwrap_or_else(|_| text.trim_matches('"').to_owned());
    if token.trim().is_empty() {
        return Err("grok bot: Cursor 未登录（打开 Cursor 登录）".to_owned());
    }
    Ok(token)
}

fn cursor_state_db() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("CURSOR_STATE_VSCDB").filter(|path| !path.is_empty()) {
        return std::path::PathBuf::from(path);
    }
    default_cursor_state_db()
}

#[cfg(target_os = "macos")]
fn default_cursor_state_db() -> std::path::PathBuf {
    std::path::PathBuf::from(home(
        "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
    ))
}

#[cfg(target_os = "linux")]
fn default_cursor_state_db() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(home(".config")));
    base.join("Cursor/User/globalStorage/state.vscdb")
}

fn grok_bot_from_usage(data: &Value, now_ms: u64) -> Result<(Vec<Metric>, String), String> {
    if data
        .get("usesPooledEnterpriseAllowance")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Err("grok bot: 企业共享额度，没有个人周额度".to_owned());
    }
    if data.get("hasNonZeroIncludedLimit").and_then(Value::as_bool) == Some(false) {
        return Err("grok bot: 当前套餐没有个人周额度".to_owned());
    }
    let used = data
        .get("usagePercent")
        .and_then(Value::as_f64)
        .ok_or_else(|| "grok bot: 响应没有 usagePercent".to_owned())?;
    let reset = data
        .get("nextResetTimestampUtc")
        .and_then(Value::as_str)
        .unwrap_or("");
    let available = data
        .get("hasAvailableUsage")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let detail = if !available {
        "已用完".to_owned()
    } else if reset.is_empty() {
        String::new()
    } else {
        reset_in(reset, now_ms)
    };
    let note = data
        .get("grokPlanLabel")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok((
        vec![Metric {
            label: "本周额度".to_owned(),
            remaining: Some((100.0 - used).max(0.0)),
            detail,
        }],
        note,
    ))
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
    let exported = run(&program("opencode"), &["auth", "export"])?;
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
    local_stamp_secs(seconds)
}

/// A Unix timestamp in seconds -> `11-16 08:00`.
fn local_stamp_secs(seconds: i64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_direct_usage_maps_windows_and_credits() {
        let value: Value = serde_json::from_str(
            r#"{
                "plan_type": "plus",
                "rate_limit": {
                    "limit_reached": false,
                    "primary_window": {
                        "used_percent": 25,
                        "limit_window_seconds": 18000,
                        "reset_at": 1794423674
                    },
                    "secondary_window": {
                        "used_percent": 9,
                        "limit_window_seconds": 604800,
                        "reset_at": 1794883835
                    }
                },
                "credits": {"has_credits": false, "unlimited": false, "balance": "0"}
            }"#,
        )
        .unwrap();
        let (metrics, note) = codex_from_direct(&value).unwrap();
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].label, "5 小时");
        assert_eq!(metrics[0].remaining, Some(75.0));
        assert_eq!(metrics[1].label, "本周额度");
        assert_eq!(metrics[1].remaining, Some(91.0));
        assert_eq!(note, "Plus · credits 0");
    }

    #[test]
    fn codex_cli_usage_maps_the_same_windows() {
        let value = json!({
            "rateLimitsByLimitId": {
                "codex": {
                    "planType": "plus",
                    "rateLimitReachedType": null,
                    "primary": {
                        "usedPercent": 18,
                        "windowDurationMins": 300,
                        "resetsAt": 1794423674
                    },
                    "secondary": {
                        "usedPercent": 8,
                        "windowDurationMins": 10080,
                        "resetsAt": 1794883835
                    },
                    "credits": {"hasCredits": false, "unlimited": false, "balance": "0"}
                }
            }
        });
        let (metrics, note) = codex_from_cli(&value).unwrap();
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].label, "5 小时");
        assert_eq!(metrics[0].remaining, Some(82.0));
        assert_eq!(metrics[1].label, "本周额度");
        assert_eq!(metrics[1].remaining, Some(92.0));
        assert_eq!(note, "Plus · credits 0");
    }

    #[test]
    fn codex_marks_a_reached_limit() {
        let value = json!({
            "rate_limit": {
                "limit_reached": true,
                "primary_window": {
                    "used_percent": 100,
                    "limit_window_seconds": 18000,
                    "reset_at": 1794423674
                }
            }
        });
        let (metrics, _) = codex_from_direct(&value).unwrap();
        assert_eq!(metrics[0].remaining, Some(0.0));
        assert_eq!(metrics[0].detail, "已限流");
    }

    #[test]
    fn codex_window_labels_follow_the_duration() {
        assert_eq!(codex_window_label(18_000), "5 小时");
        assert_eq!(codex_window_label(604_800), "本周额度");
        assert_eq!(codex_window_label(86_400), "1 天");
        assert_eq!(codex_window_label(7_200), "2 小时");
        assert_eq!(codex_window_label(1_800), "30 分钟");
    }

    #[test]
    fn codex_usage_without_windows_is_an_error() {
        assert!(codex_from_direct(&json!({"rate_limit": {}})).is_err());
        assert!(codex_from_cli(&json!({})).is_err());
    }

    #[test]
    fn codex_unlimited_credits_say_so() {
        assert_eq!(
            codex_credits(Some(&json!({"unlimited": true}))).as_deref(),
            Some("credits 无限")
        );
        assert_eq!(
            codex_credits(Some(&json!({"unlimited": false, "balance": "12"}))).as_deref(),
            Some("credits 12")
        );
    }

    #[test]
    fn grok_billing_maps_the_weekly_credit_and_products() {
        let value = json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-11-06T03:41:51.334138+00:00",
                    "end": "2026-11-13T03:41:51.334138+00:00"
                },
                "creditUsagePercent": 17.0,
                "productUsage": [
                    {"product": "GrokBuild", "usagePercent": 10.0},
                    {"product": "GrokChat", "usagePercent": 7.0}
                ]
            }
        });
        let (metrics, note) = grok_from_billing(&value, 1_760_000_000_000).unwrap();
        assert_eq!(metrics.len(), 3);
        assert_eq!(metrics[0].label, "本周额度");
        assert_eq!(metrics[0].remaining, Some(83.0));
        assert_eq!(metrics[1].label, "Grok Build");
        assert_eq!(metrics[1].remaining, Some(90.0));
        assert_eq!(metrics[2].label, "Grok Chat");
        assert_eq!(metrics[2].remaining, Some(93.0));
        assert!(note.is_empty());
    }

    #[test]
    fn grok_billing_without_usage_is_an_error() {
        assert!(grok_from_billing(&json!({"config": {}}), 0).is_err());
        assert!(grok_from_billing(&json!({}), 0).is_err());
    }

    #[test]
    fn grok_form_values_are_percent_encoded() {
        assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(url_encode("a b+c/d"), "a%20b%2Bc%2Fd");
    }

    #[test]
    fn grok_bot_usage_maps_the_weekly_percentage() {
        let value = json!({
            "usagePercent": 7.359366,
            "nextResetTimestampUtc": "2026-11-16T15:47:13.515Z",
            "grokPlanLabel": "SuperGrok",
            "hasNonZeroIncludedLimit": true,
            "hasAvailableUsage": true
        });
        let (metrics, note) = grok_bot_from_usage(&value, 1_760_000_000_000).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].label, "本周额度");
        let remaining = metrics[0].remaining.unwrap();
        assert!((remaining - 92.640634).abs() < 0.001);
        assert_eq!(note, "SuperGrok");
    }

    #[test]
    fn grok_bot_without_an_individual_allowance_is_an_error() {
        assert!(grok_bot_from_usage(&json!({"hasNonZeroIncludedLimit": false}), 0).is_err());
        assert!(grok_bot_from_usage(&json!({"usesPooledEnterpriseAllowance": true}), 0).is_err());
        assert!(grok_bot_from_usage(&json!({}), 0).is_err());
    }
}
