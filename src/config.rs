//! The config file: which providers the tray and panel show.
//!
//! Every provider is on unless its entry says otherwise, so a missing or
//! empty file shows everything, and a provider added later starts on without
//! touching anyone's file. A provider can be a bool or an object with
//! `enabled`, which leaves room for per-provider settings later:
//!
//! ```json
//! {
//!   "providers": {
//!     "antigravity": true,
//!     "claude": { "enabled": false },
//!     "opencode": true
//!   }
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Map, Value, json};

/// What the file said when we last read or wrote it, so a running app can
/// tell the user's edit from its own write.
static SEEN: Mutex<Option<String>> = Mutex::new(None);

fn seen() -> std::sync::MutexGuard<'static, Option<String>> {
    SEEN.lock().unwrap_or_else(|error| error.into_inner())
}

/// What the menu bar shows when several providers are on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuBarMode {
    /// Every enabled provider, two lines.
    #[default]
    All,
    /// The two with the least left, so the item fits beside a notch.
    Compact,
}

/// The panel's language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    /// The system's UI language.
    #[default]
    Auto,
    Zh,
    En,
}

/// Which system notifications the app may show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notify {
    /// A provider fell below 15%.
    pub low: bool,
    /// A 5-hour window came back to full.
    pub reset: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            low: true,
            reset: true,
        }
    }
}

/// The size the user last resized the panel to, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanelSize {
    pub width: f32,
    pub height: f32,
}

/// Which providers are on, by provider id. Missing ids are enabled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    enabled: BTreeMap<String, bool>,
    /// Providers startup detection found missing. They are off without
    /// being written to the file: installing one later finds it on again.
    hidden: BTreeSet<String>,
    panel: Option<PanelSize>,
    menu_bar: MenuBarMode,
    language: Language,
    notify: Notify,
}

impl Config {
    pub fn enabled(&self, id: &str) -> bool {
        match self.enabled.get(id) {
            Some(on) => *on,
            None => !self.hidden.contains(id),
        }
    }

    /// Leaves a provider off without choosing it: detection does this for a
    /// tool this machine does not have.
    pub fn hide(&mut self, id: &str) {
        self.hidden.insert(id.to_owned());
    }

    /// Whether the file names this provider (rather than leaving it to the
    /// default), so startup detection does not override a chosen value.
    pub fn declared(&self, id: &str) -> bool {
        self.enabled.contains_key(id)
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) {
        self.enabled.insert(id.to_owned(), on);
    }

    /// The panel size the file remembers, if any.
    pub fn panel(&self) -> Option<PanelSize> {
        self.panel
    }

    pub fn set_panel(&mut self, size: PanelSize) {
        self.panel = Some(size);
    }

    pub fn menu_bar(&self) -> MenuBarMode {
        self.menu_bar
    }

    pub fn set_menu_bar(&mut self, mode: MenuBarMode) {
        self.menu_bar = mode;
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn set_language(&mut self, language: Language) {
        self.language = language;
    }

    pub fn notify(&self) -> Notify {
        self.notify
    }

    pub fn set_notify(&mut self, notify: Notify) {
        self.notify = notify;
    }

    /// Whether a config file exists: a first run has none, and shows the
    /// panel once so the app does not look like it failed to start.
    pub fn exists() -> bool {
        path().is_some_and(|path| path.is_file())
    }

    /// Reads the file; every provider is on when there is none. A file that
    /// cannot be read or parsed is an error, so a running app keeps the
    /// providers it already has instead of flipping them all back on.
    pub fn load() -> Result<Self, String> {
        let Some(path) = path() else {
            return Ok(Self::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                *seen() = None;
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let config = Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()));
        *seen() = Some(text);
        config
    }

    /// Reads the file again, but only when someone else changed it: the
    /// app's own writes are already in `current`. A missing or broken file
    /// leaves `current` as it is.
    pub fn reload(current: &Self) -> Self {
        let Some(path) = path() else {
            return current.clone();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return current.clone(),
            Err(error) => {
                eprintln!("[quota-bar] {}: {error}", path.display());
                return current.clone();
            }
        };
        if seen().as_deref() == Some(text.as_str()) {
            return current.clone();
        }
        match Self::parse(&text) {
            Ok(config) => {
                *seen() = Some(text);
                config
            }
            Err(error) => {
                eprintln!("[quota-bar] {}: {error}", path.display());
                *seen() = Some(text);
                current.clone()
            }
        }
    }

    /// Writes the file, making its directory when needed.
    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no config path")?;
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = format!(
            "{}\n",
            serde_json::to_string_pretty(&self.to_json()).map_err(|e| e.to_string())?
        );
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        *seen() = Some(text);
        Ok(())
    }

    fn to_json(&self) -> Value {
        let providers: Map<String, Value> = self
            .enabled
            .iter()
            .map(|(id, on)| (id.clone(), json!(on)))
            .collect();
        let mut value = json!({ "providers": providers });
        if let Some(panel) = self.panel {
            value["panel"] = json!({ "width": panel.width, "height": panel.height });
        }
        if self.menu_bar == MenuBarMode::Compact {
            value["menuBar"] = json!("compact");
        }
        match self.language {
            Language::Auto => {}
            Language::Zh => value["language"] = json!("zh"),
            Language::En => value["language"] = json!("en"),
        }
        if self.notify != Notify::default() {
            value["notify"] = json!({ "low": self.notify.low, "reset": self.notify.reset });
        }
        value
    }

    fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut config = Self::default();
        if let Some(mode) = value.get("menuBar") {
            config.menu_bar = match mode.as_str() {
                Some("all") => MenuBarMode::All,
                Some("compact") => MenuBarMode::Compact,
                _ => return Err("menuBar is not \"all\" or \"compact\"".to_owned()),
            };
        }
        if let Some(language) = value.get("language") {
            config.language = match language.as_str() {
                Some("auto") => Language::Auto,
                Some("zh") => Language::Zh,
                Some("en") => Language::En,
                _ => return Err("language is not \"auto\", \"zh\" or \"en\"".to_owned()),
            };
        }
        if let Some(notify) = value.get("notify") {
            let notify = notify
                .as_object()
                .ok_or_else(|| "notify is not an object".to_owned())?;
            config.notify = Notify {
                low: notify.get("low").and_then(Value::as_bool).unwrap_or(true),
                reset: notify.get("reset").and_then(Value::as_bool).unwrap_or(true),
            };
        }
        if let Some(panel) = value.get("panel").filter(|panel| !panel.is_null()) {
            let width = panel.get("width").and_then(Value::as_f64);
            let height = panel.get("height").and_then(Value::as_f64);
            let (Some(width), Some(height)) = (width, height) else {
                return Err("panel needs a width and a height".to_owned());
            };
            config.panel = Some(PanelSize {
                width: width as f32,
                height: height as f32,
            });
        }
        let Some(providers) = value.get("providers") else {
            return Ok(config);
        };
        let providers = providers
            .as_object()
            .ok_or_else(|| "providers is not an object".to_owned())?;
        for (id, entry) in providers {
            let on = match entry {
                Value::Bool(on) => *on,
                Value::Object(fields) => fields
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                _ => return Err(format!("providers.{id} is not a bool or an object")),
            };
            config.set_enabled(id, on);
        }
        Ok(config)
    }
}

/// The app's data directory, without the reactive store (`data_dir` needs
/// the app running; the single-instance lock is taken before that).
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Application Support/bar.jyeu.quota"))
    }
    #[cfg(target_os = "linux")]
    {
        Some(
            std::env::var_os("XDG_DATA_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("bar.jyeu.quota"),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = home;
        None
    }
}

/// `$QUOTA_BAR_CONFIG`, else `config.json` in the app's data directory.
pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("QUOTA_BAR_CONFIG").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    dir().map(|dir| dir.join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_entries_are_enabled() {
        let config = Config::parse(r#"{"providers":{"claude":false}}"#).unwrap();
        assert!(!config.enabled("claude"));
        assert!(config.enabled("antigravity"));
        assert!(config.enabled("opencode"));
        assert!(config.enabled("a provider added later"));
    }

    #[test]
    fn entries_can_be_bools_or_objects() {
        let config = Config::parse(
            r#"{"providers":{"antigravity":true,"claude":{"enabled":false},"opencode":{}}}"#,
        )
        .unwrap();
        assert!(config.enabled("antigravity"));
        assert!(!config.enabled("claude"));
        assert!(config.enabled("opencode"));
    }

    #[test]
    fn an_empty_or_absent_providers_object_shows_everything() {
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
        assert_eq!(
            Config::parse(r#"{"providers":{}}"#).unwrap(),
            Config::default()
        );
        assert!(
            Config::parse(r#"{"providers":{"claude":{"enabled":true}}}"#)
                .unwrap()
                .enabled("opencode")
        );
    }

    #[test]
    fn a_bad_file_is_an_error() {
        assert!(Config::parse("not json").is_err());
        assert!(Config::parse(r#"{"providers":[]}"#).is_err());
        assert!(Config::parse(r#"{"providers":{"claude":"off"}}"#).is_err());
    }

    #[test]
    fn what_is_saved_reads_back() {
        let mut config = Config::default();
        config.set_enabled("claude", false);
        config.set_enabled("opencode", true);
        let text = serde_json::to_string(&config.to_json()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), config);
    }

    #[test]
    fn the_panel_size_is_saved_and_read_back() {
        let mut config = Config::default();
        config.set_panel(PanelSize {
            width: 512.0,
            height: 400.0,
        });
        let text = serde_json::to_string(&config.to_json()).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), config);
        assert_eq!(
            Config::parse(r#"{"panel":{"width":300,"height":250}}"#)
                .unwrap()
                .panel(),
            Some(PanelSize {
                width: 300.0,
                height: 250.0
            })
        );
    }

    #[test]
    fn detection_stays_out_of_the_file() {
        let mut config = Config::parse(r#"{"providers":{"claude":true}}"#).unwrap();
        config.hide("antigravity");
        assert!(!config.enabled("antigravity"));
        assert!(!config.declared("antigravity"));
        let text = serde_json::to_string(&config.to_json()).unwrap();
        assert!(!text.contains("antigravity"));
        // Installing the tool later finds it on again.
        assert!(Config::parse(&text).unwrap().enabled("antigravity"));
    }

    #[test]
    fn a_panel_without_a_size_is_an_error() {
        assert!(Config::parse(r#"{"panel":{"width":300}}"#).is_err());
        assert!(Config::parse(r#"{"panel":true}"#).is_err());
        assert_eq!(
            Config::parse(r#"{"panel":null}"#).unwrap(),
            Config::default()
        );
    }
}
