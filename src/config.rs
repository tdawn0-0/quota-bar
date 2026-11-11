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

use nsh_core::prelude::data_dir;
use serde_json::{Map, Value, json};

/// Which providers are on, by provider id. Missing ids are enabled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    enabled: BTreeMap<String, bool>,
}

impl Config {
    pub fn enabled(&self, id: &str) -> bool {
        self.enabled.get(id).copied().unwrap_or(true)
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
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// Writes the file, making its directory when needed.
    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or("no config path")?;
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(&path, format!("{text}\n")).map_err(|e| format!("{}: {e}", path.display()))
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
        assert_eq!(
            Config::parse(r#"{"providers":{"claude":{"enabled":true}}}"#)
                .unwrap()
                .enabled("opencode"),
            true
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
