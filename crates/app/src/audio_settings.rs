//! Which audio devices Dubplate uses: the main output and the headphone monitor. Remembered between runs.

use std::path::PathBuf;

use dubplate_engine::MonitorTarget;
use serde::{Deserialize, Serialize};

/// What the Monitor picker holds. Kept apart from the engine's [`MonitorTarget`] so the file stays readable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "device", rename_all = "snake_case")]
pub enum MonitorChoice {
    #[default]
    Off,
    /// Channels 3-4 of the output device.
    SameDevice,
    Device(String),
    /// Channels 3-4 of a second 4-channel device.
    DeviceChannels34(String),
}

impl MonitorChoice {
    pub fn target(&self) -> MonitorTarget {
        match self {
            MonitorChoice::Off => MonitorTarget::Off,
            MonitorChoice::SameDevice => MonitorTarget::SameDevice,
            MonitorChoice::Device(d) => MonitorTarget::Device(d.clone()),
            MonitorChoice::DeviceChannels34(d) => MonitorTarget::DeviceChannels34(d.clone()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
    /// None = the system default output.
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub monitor: MonitorChoice,
}

fn path() -> PathBuf {
    dubplate_engine::paths::config_dir().join("audio.toml")
}

impl AudioSettings {
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
        for monitor in [
            MonitorChoice::Off,
            MonitorChoice::SameDevice,
            MonitorChoice::Device("hw:1".into()),
            MonitorChoice::DeviceChannels34("hw:2".into()),
        ] {
            let s = AudioSettings { output: Some("sysdefault:CARD=Mk2".into()), monitor };
            let text = toml::to_string_pretty(&s).unwrap();
            assert_eq!(toml::from_str::<AudioSettings>(&text).unwrap(), s, "{text}");
        }
    }

    #[test]
    fn an_empty_or_old_file_means_defaults() {
        let s: AudioSettings = toml::from_str("").unwrap();
        assert_eq!(s, AudioSettings::default());
        assert_eq!(s.monitor.target(), MonitorTarget::Off);
    }
}
