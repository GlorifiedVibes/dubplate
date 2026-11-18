//! The deck switches that should be as you left them next time: key lock and auto gain, per deck.
//!
//! Kept in `decks.toml` in the config folder, written whenever one is switched. A missing or unreadable file means
//! both on for both decks, which is what a new install starts with.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn on() -> [bool; 2] {
    [true; 2]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckSettings {
    #[serde(default = "on")]
    pub key_lock: [bool; 2],
    #[serde(default = "on")]
    pub auto_gain: [bool; 2],
}

impl Default for DeckSettings {
    fn default() -> Self {
        Self { key_lock: on(), auto_gain: on() }
    }
}

fn path() -> PathBuf {
    dubplate_engine::paths::config_dir().join("decks.toml")
}

impl DeckSettings {
    pub fn load() -> Self {
        std::fs::read_to_string(path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path(), s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_toml() {
        let s = DeckSettings { key_lock: [false, true], auto_gain: [true, false] };
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(toml::from_str::<DeckSettings>(&text).unwrap(), s, "{text}");
    }

    #[test]
    fn an_empty_or_partial_file_means_on() {
        assert_eq!(toml::from_str::<DeckSettings>("").unwrap(), DeckSettings::default());
        let s: DeckSettings = toml::from_str("key_lock = [false, false]").unwrap();
        assert_eq!(s, DeckSettings { key_lock: [false; 2], auto_gain: [true; 2] });
    }
}
