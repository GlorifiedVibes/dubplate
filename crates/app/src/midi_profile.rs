//! Controller profiles: which MIDI message does which Dubplate function.
//!
//! A profile is one TOML file per controller:
//!
//! ```toml
//! [device]
//! name = "Hercules DJControl Inpulse 200 Mk2"
//! port_match = ["Inpulse 200"]      # case-insensitive substrings of the MIDI port name
//!
//! [[bind]]
//! msg = "91 06"                     # status byte + note/CC number, hex
//! func = "cue"
//! deck = 0                          # 0 = A, 1 = B; omit for global functions
//! ```
//!
//! Shift layers need no special support: a controller that sends its shifted buttons
//! on another channel simply gets more `[[bind]]` rows.
//!
//! Built-in profiles ship in `profiles/`; user profiles (including imported ones) live
//! in `~/.config/dubplate/midi/` and override a built-in with the same file name.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Every function a MIDI control can be bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Func {
    // Buttons (per deck)
    PlayPause,
    Cue,
    ToStart,
    Sync,
    Load,
    LoopIn,
    LoopOut,
    LoopHalf,
    LoopDouble,
    /// Auto loop of `arg` beats (toggles off when a loop is active).
    LoopBeats,
    /// Exit / re-engage the loop.
    LoopToggle,
    /// Hot-cue pad `arg` (0 = A … 7 = H).
    Pad,
    PadDelete,
    /// Stem mute: 0 drums, 1 bass, 2 other, 3 vocals.
    Stem,
    Quantize,
    /// Key lock on / off.
    KeyLock,
    /// Auto gain on / off.
    AutoGain,
    /// Headphone cue (pre-fader listen) on / off.
    HeadphoneCue,
    /// Adds the main mix to the headphones.
    MasterMonitor,
    JogTouch,
    /// Vinyl mode on / off: off, the platter only nudges the tempo instead of scratching.
    Vinyl,
    // Absolute controls (per deck)
    Volume,
    Gain,
    EqHigh,
    EqMid,
    EqLow,
    Filter,
    Tempo,
    // Relative controls (per deck)
    JogBend,
    JogScratch,
    /// Fast search through the track (SHIFT + jog on most controllers).
    JogSearch,
    // Global
    Crossfader,
    /// Browser encoder; `arg` multiplies the step (default 1).
    Browse,
}

/// (function, file name, global?, one-line description)
pub const FUNCS: &[(Func, &str, bool, &str)] = &[
    (Func::PlayPause, "play_pause", false, "Play / pause"),
    (Func::Cue, "cue", false, "CUE (press and release)"),
    (Func::ToStart, "to_start", false, "Jump to track start"),
    (Func::Sync, "sync", false, "SYNC to the other deck"),
    (Func::Load, "load", false, "Load the selected library track"),
    (Func::LoopIn, "loop_in", false, "Loop IN (press and release; hold = auto loop)"),
    (Func::LoopOut, "loop_out", false, "Loop OUT / exit / re-loop"),
    (Func::LoopHalf, "loop_half", false, "Halve the loop"),
    (Func::LoopDouble, "loop_double", false, "Double the loop"),
    (Func::LoopBeats, "loop_beats", false, "Auto loop of `arg` beats"),
    (Func::LoopToggle, "loop_toggle", false, "Loop on / off"),
    (Func::Pad, "pad", false, "Hot-cue pad `arg` (0-7)"),
    (Func::PadDelete, "pad_delete", false, "Delete hot-cue pad `arg` (0-7)"),
    (Func::Stem, "stem", false, "Mute stem `arg` (0 drums, 1 bass, 2 other, 3 vocals)"),
    (Func::Quantize, "quantize", false, "Quantize on / off"),
    (Func::KeyLock, "key_lock", false, "Key lock on / off"),
    (Func::AutoGain, "auto_gain", false, "Auto gain on / off"),
    (Func::HeadphoneCue, "headphone_cue", false, "Headphone cue (pre-fader listen) on / off"),
    (Func::MasterMonitor, "master_monitor", true, "Master in the headphones on / off"),
    (Func::JogTouch, "jog_touch", false, "Jog top touch sensor"),
    (Func::Vinyl, "vinyl", false, "Vinyl mode on / off (off: platter only nudges)"),
    (Func::Volume, "volume", false, "Channel fader"),
    (Func::Gain, "gain", false, "Trim / gain"),
    (Func::EqHigh, "eq_high", false, "EQ high"),
    (Func::EqMid, "eq_mid", false, "EQ mid"),
    (Func::EqLow, "eq_low", false, "EQ low"),
    (Func::Filter, "filter", false, "Filter knob"),
    (Func::Tempo, "tempo", false, "Tempo fader"),
    (Func::JogBend, "jog_bend", false, "Jog outer ring (pitch bend)"),
    (Func::JogScratch, "jog_scratch", false, "Jog platter (scratch)"),
    (Func::JogSearch, "jog_search", false, "Search through the track (SHIFT + jog)"),
    (Func::Crossfader, "crossfader", true, "Crossfader"),
    (Func::Browse, "browse", true, "Browser encoder"),
];

impl Func {
    pub fn from_name(name: &str) -> Option<Func> {
        FUNCS.iter().find(|f| f.1 == name).map(|f| f.0)
    }
    pub fn name(self) -> &'static str {
        FUNCS.iter().find(|f| f.0 == self).map(|f| f.1).unwrap_or("?")
    }
    pub fn is_global(self) -> bool {
        FUNCS.iter().find(|f| f.0 == self).map(|f| f.2).unwrap_or(false)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    /// Case-insensitive substrings of the MIDI port name this profile fits.
    #[serde(default)]
    pub port_match: Vec<String>,
    /// Breaks ties when several profiles fit one port (higher wins).
    #[serde(default)]
    pub priority: i32,
    /// Where the mapping came from, e.g. "DJUCED .djm".
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bind {
    /// Status byte and note / CC number in hex, e.g. "91 06". Note-off is folded into note-on.
    pub msg: String,
    pub func: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<i32>,
    /// 14-bit control: the LSB arrives on the CC 0x20 above the MSB.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hires: bool,
    /// Name of the control on the hardware (documentation only).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    pub device: DeviceInfo,
    #[serde(default, rename = "bind")]
    pub binds: Vec<Bind>,
}

/// A bind that passed validation, ready for the mapper.
#[derive(Clone, Copy, Debug)]
pub struct Compiled {
    pub status: u8,
    pub data1: u8,
    pub func: Func,
    pub deck: usize,
    pub arg: i32,
    pub hires: bool,
}

/// Parses "91 06" into (status, data1).
pub fn parse_msg(msg: &str) -> Result<(u8, u8), String> {
    let parts: Vec<&str> = msg.split_whitespace().collect();
    if parts.len() != 2 {
        return Err(format!("`{msg}`: expected two hex bytes like \"91 06\""));
    }
    let b = |s: &str| {
        u8::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| format!("`{msg}`: `{s}` is not a hex byte"))
    };
    let (status, data1) = (b(parts[0])?, b(parts[1])?);
    if status < 0x80 {
        return Err(format!("`{msg}`: status byte must be 80-FF"));
    }
    if data1 > 0x7F {
        return Err(format!("`{msg}`: data byte must be 00-7F"));
    }
    Ok((fold_status(status), data1))
}

/// Note-off (8n) is treated as note-on (9n) with velocity 0.
pub fn fold_status(status: u8) -> u8 {
    if status & 0xF0 == 0x80 {
        status | 0x10
    } else {
        status
    }
}

impl Profile {
    pub fn from_toml(text: &str) -> Result<Profile, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Validates every bind; an error names the offending row.
    pub fn compile(&self) -> Result<Vec<Compiled>, String> {
        let mut out = Vec::with_capacity(self.binds.len());
        for (i, b) in self.binds.iter().enumerate() {
            let at = |e: String| format!("bind #{} ({}): {e}", i + 1, b.msg);
            let (status, data1) = parse_msg(&b.msg).map_err(&at)?;
            let func = Func::from_name(&b.func).ok_or_else(|| at(format!("unknown function `{}`", b.func)))?;
            let deck = match (func.is_global(), b.deck) {
                (true, _) => 0,
                (false, Some(d)) if d < 2 => d as usize,
                (false, _) => return Err(at(format!("`{}` needs deck = 0 or 1", b.func))),
            };
            let arg = b.arg.unwrap_or(match func {
                Func::Browse => 1,
                _ => 0,
            });
            let needs_arg = matches!(func, Func::Pad | Func::PadDelete | Func::Stem | Func::LoopBeats);
            if needs_arg && b.arg.is_none() {
                return Err(at(format!("`{}` needs `arg`", b.func)));
            }
            if matches!(func, Func::Pad | Func::PadDelete) && !(0..8).contains(&arg) {
                return Err(at("pad arg must be 0-7".into()));
            }
            if func == Func::Stem && !(0..4).contains(&arg) {
                return Err(at("stem arg must be 0-3".into()));
            }
            if b.hires && status & 0xF0 != 0xB0 {
                return Err(at("hires only applies to CC messages".into()));
            }
            out.push(Compiled { status, data1, func, deck, arg, hires: b.hires });
        }
        Ok(out)
    }
}

/// A profile with its identity (file stem).
#[derive(Clone, Debug)]
pub struct ProfileEntry {
    pub id: String,
    pub profile: Profile,
    pub builtin: bool,
}

include!(concat!(env!("OUT_DIR"), "/builtin_profiles.rs"));

pub fn config_dir() -> PathBuf {
    dubplate_engine::paths::config_dir()
}

pub fn user_profile_dir() -> PathBuf {
    config_dir().join("midi")
}

/// All profiles: built-ins, overridden / extended by the user's folder.
pub fn load_profiles() -> Vec<ProfileEntry> {
    let mut map: BTreeMap<String, ProfileEntry> = BTreeMap::new();
    for (id, text) in BUILTIN {
        match Profile::from_toml(text).and_then(|p| p.compile().map(|_| p)) {
            Ok(profile) => {
                map.insert((*id).into(), ProfileEntry { id: (*id).into(), profile, builtin: true });
            }
            Err(e) => log::error!("built-in MIDI profile {id} is invalid: {e}"),
        }
    }
    if let Ok(rd) = std::fs::read_dir(user_profile_dir()) {
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
            let loaded = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| Profile::from_toml(&t))
                .and_then(|p| p.compile().map(|_| p));
            match loaded {
                Ok(profile) => {
                    map.insert(id.clone(), ProfileEntry { id, profile, builtin: false });
                }
                Err(e) => log::warn!("skipping MIDI profile {}: {e}", path.display()),
            }
        }
    }
    let mut v: Vec<_> = map.into_values().collect();
    v.sort_by(|a, b| a.profile.device.name.cmp(&b.profile.device.name));
    v
}

/// "Name" -> "name", for a file name.
pub fn slug(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

/// Saves a profile into the user's folder; returns its id.
pub fn save_user_profile(profile: &Profile) -> std::io::Result<String> {
    let id = slug(&profile.device.name);
    let dir = user_profile_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{id}.toml")), profile.to_toml())?;
    Ok(id)
}

/// Removes what changes between sessions or sockets, so a saved choice keeps matching: ALSA's trailing
/// "client:port" numbers on Linux, and the "2- " prefix Windows puts on a second identical device.
pub fn normalise_port_name(name: &str) -> String {
    let mut t = name.trim();
    if let Some((head, tail)) = t.rsplit_once(' ') {
        let is_ids = tail
            .split_once(':')
            .map(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
            })
            .unwrap_or(false);
        if is_ids {
            t = head.trim();
        }
    }
    if let Some((num, rest)) = t.split_once("- ") {
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            t = rest.trim();
        }
    }
    t.to_owned()
}

/// Best-fitting profile for a port name: the longest matching `port_match`, then priority.
pub fn detect<'a>(profiles: &'a [ProfileEntry], port: &str) -> Option<&'a ProfileEntry> {
    let port = port.to_lowercase();
    profiles
        .iter()
        .filter_map(|p| {
            let best = p
                .profile
                .device
                .port_match
                .iter()
                .filter(|m| !m.is_empty() && port.contains(&m.to_lowercase()))
                .map(|m| m.len())
                .max()?;
            Some((best, p.profile.device.priority, p))
        })
        .max_by_key(|(len, prio, _)| (*len, *prio))
        .map(|(_, _, p)| p)
}

/// The user's per-port choices. A port with no entry uses auto-detection.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MidiSettings {
    /// Normalised port name -> profile id, or "off".
    #[serde(default)]
    pub ports: BTreeMap<String, String>,
}

pub const OFF: &str = "off";

impl MidiSettings {
    fn path() -> PathBuf {
        config_dir().join("midi_settings.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, toml::to_string_pretty(self).unwrap_or_default()) {
            log::warn!("saving MIDI settings failed: {e}");
        }
    }

    /// Profile id to use for `port`, honouring an explicit choice or falling back to detection.
    pub fn resolve<'a>(&self, profiles: &'a [ProfileEntry], port: &str) -> Option<&'a ProfileEntry> {
        match self.ports.get(&normalise_port_name(port)).map(String::as_str) {
            Some(OFF) => None,
            Some(id) => profiles.iter().find(|p| p.id == id).or_else(|| detect(profiles, port)),
            None => detect(profiles, port),
        }
    }
}

/// Lookup used by the mapper: (status, data1) -> binding.
pub fn index(compiled: &[Compiled]) -> HashMap<(u8, u8), Compiled> {
    let mut m = HashMap::new();
    for c in compiled {
        m.entry((c.status, c.data1)).or_insert(*c);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(binds: &str) -> Profile {
        Profile::from_toml(&format!("[device]\nname = \"T\"\nport_match = [\"Foo\"]\n{binds}")).unwrap()
    }

    #[test]
    fn parses_and_validates_messages() {
        assert_eq!(parse_msg("91 06").unwrap(), (0x91, 0x06));
        assert_eq!(parse_msg("0xB1 0x2a").unwrap(), (0xB1, 0x2A));
        assert_eq!(parse_msg("81 06").unwrap(), (0x91, 0x06), "note-off folds into note-on");
        assert!(parse_msg("91").is_err());
        assert!(parse_msg("zz 01").is_err());
        assert!(parse_msg("91 80").is_err());
        assert!(parse_msg("11 01").is_err());
    }

    #[test]
    fn function_names_round_trip() {
        for (f, name, _, _) in FUNCS {
            assert_eq!(Func::from_name(name), Some(*f));
            assert_eq!(f.name(), *name);
        }
        assert_eq!(Func::from_name("nope"), None);
    }

    #[test]
    fn compile_reports_the_bad_row() {
        let ok = p("[[bind]]\nmsg = \"91 06\"\nfunc = \"cue\"\ndeck = 0\n");
        assert_eq!(ok.compile().unwrap().len(), 1);

        let e = p("[[bind]]\nmsg = \"91 06\"\nfunc = \"cue\"\n").compile().unwrap_err();
        assert!(e.contains("deck"), "{e}");
        let e = p("[[bind]]\nmsg = \"91 06\"\nfunc = \"flip\"\ndeck = 0\n").compile().unwrap_err();
        assert!(e.contains("unknown function"), "{e}");
        let e = p("[[bind]]\nmsg = \"96 00\"\nfunc = \"pad\"\ndeck = 0\n").compile().unwrap_err();
        assert!(e.contains("arg"), "{e}");
        let e = p("[[bind]]\nmsg = \"96 00\"\nfunc = \"pad\"\ndeck = 0\narg = 9\n").compile().unwrap_err();
        assert!(e.contains("0-7"), "{e}");
        let e = p("[[bind]]\nmsg = \"91 00\"\nfunc = \"volume\"\ndeck = 0\nhires = true\n").compile().unwrap_err();
        assert!(e.contains("hires"), "{e}");
    }

    #[test]
    fn global_functions_need_no_deck() {
        let c = p("[[bind]]\nmsg = \"B0 00\"\nfunc = \"crossfader\"\n").compile().unwrap();
        assert_eq!(c[0].func, Func::Crossfader);
        let c = p("[[bind]]\nmsg = \"B0 01\"\nfunc = \"browse\"\n").compile().unwrap();
        assert_eq!(c[0].arg, 1, "browse defaults to a step of 1");
    }

    #[test]
    fn toml_round_trips() {
        let a = p("[[bind]]\nmsg = \"B1 00\"\nfunc = \"volume\"\ndeck = 0\nhires = true\nlabel = \"VOL_A\"\n");
        let b = Profile::from_toml(&a.to_toml()).unwrap();
        assert_eq!(b.binds.len(), 1);
        assert!(b.binds[0].hires);
        assert_eq!(b.binds[0].label, "VOL_A");
    }

    #[test]
    fn port_names_lose_their_session_ids() {
        assert_eq!(
            normalise_port_name("Hercules DJControl Inpulse 200 MIDI 1 20:0"),
            "Hercules DJControl Inpulse 200 MIDI 1"
        );
        assert_eq!(normalise_port_name("Midi Through Port-0 14:0"), "Midi Through Port-0");
        assert_eq!(normalise_port_name("Inpulse 200"), "Inpulse 200");
        // Windows numbers a second identical device; macOS names are left alone.
        assert_eq!(normalise_port_name("2- DJControl Inpulse 200 MK2"), "DJControl Inpulse 200 MK2");
        assert_eq!(normalise_port_name("DJControl Inpulse 200 MK2"), "DJControl Inpulse 200 MK2");
        assert_eq!(
            normalise_port_name("Traktor Kontrol S2 - Port 1"),
            "Traktor Kontrol S2 - Port 1",
            "a dash inside a name stays"
        );
    }

    fn entry(id: &str, matches: &[&str], priority: i32) -> ProfileEntry {
        ProfileEntry {
            id: id.into(),
            builtin: true,
            profile: Profile {
                device: DeviceInfo {
                    name: id.into(),
                    port_match: matches.iter().map(|s| s.to_string()).collect(),
                    priority,
                    ..Default::default()
                },
                binds: vec![],
            },
        }
    }

    #[test]
    fn detection_prefers_the_most_specific_match() {
        let ps = vec![entry("orig", &["Inpulse 200"], 0), entry("mk2", &["Inpulse 200 MK2", "Inpulse 200"], 1)];
        assert_eq!(detect(&ps, "Hercules DJControl Inpulse 200 MK2 MIDI 1").unwrap().id, "mk2");
        // Same-length matches fall back to priority.
        assert_eq!(detect(&ps, "DJControl Inpulse 200 MIDI 1").unwrap().id, "mk2");
        assert!(detect(&ps, "Launchpad").is_none());
    }

    #[test]
    fn explicit_choice_beats_detection() {
        let ps = vec![entry("orig", &["Inpulse 200"], 0), entry("mk2", &["Inpulse 200"], 1)];
        let mut s = MidiSettings::default();
        let port = "DJControl Inpulse 200 MIDI 1 20:0";
        assert_eq!(s.resolve(&ps, port).unwrap().id, "mk2");
        s.ports.insert(normalise_port_name(port), "orig".into());
        assert_eq!(s.resolve(&ps, "DJControl Inpulse 200 MIDI 1 24:0").unwrap().id, "orig");
        s.ports.insert(normalise_port_name(port), OFF.into());
        assert!(s.resolve(&ps, port).is_none());
        s.ports.insert(normalise_port_name(port), "deleted-profile".into());
        assert_eq!(s.resolve(&ps, port).unwrap().id, "mk2", "a missing profile falls back to detection");
    }

    #[test]
    fn every_shipped_profile_is_valid() {
        assert!(!BUILTIN.is_empty());
        let mut seen = std::collections::HashSet::new();
        for (id, text) in BUILTIN {
            assert!(seen.insert(*id), "duplicate profile id {id}");
            let p = Profile::from_toml(text).unwrap_or_else(|e| panic!("{id}: {e}"));
            p.compile().unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!p.device.name.is_empty(), "{id}: no device name");
            assert!(!p.device.port_match.is_empty(), "{id}: no port_match, it could never be auto-detected");
            assert!(p.binds.len() >= 10, "{id}: only {} bindings", p.binds.len());
        }
    }

    #[test]
    fn slugs_are_file_safe() {
        assert_eq!(slug("DJControl Inpulse 200 Mk2"), "djcontrol-inpulse-200-mk2");
        assert_eq!(slug("  A/B  c "), "a-b-c");
    }
}
