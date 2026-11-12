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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use nsh_core::prelude::data_dir;
use serde_json::{Map, Value, json};

/// What the file said when we last read or wrote it, so a running app can
/// tell the user's edit from its own write.
static SEEN: Mutex<Option<String>> = Mutex::new(None);

fn seen() -> std::sync::MutexGuard<'static, Option<String>> {
    SEEN.lock().unwrap_or_else(|error| error.into_inner())
}

/// Which providers are on, by provider id. Missing ids are enabled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    enabled: BTreeMap<String, bool>,
}

impl Config {
    pub fn enabled(&self, id: &str) -> bool {
        self.enabled.get(id).copied().unwrap_or(true)
    }

    /// Whether the file names this provider (rather than leaving it to the
    /// default), so startup detection does not override a chosen value.
    pub fn declared(&self, id: &str) -> bool {
        self.enabled.contains_key(id)
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) {
        self.enabled.insert(id.to_owned(), on);
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
        json!({ "providers": providers })
    }

    fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut config = Self::default();
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

/// `$QUOTA_BAR_CONFIG`, else `config.json` in the app's data directory,
/// else `~/.config/quota-bar/config.json` when the host keeps no files.
pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("QUOTA_BAR_CONFIG").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(dir) = data_dir() {
        return Some(dir.join("config.json"));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/quota-bar/config.json"))
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
}
