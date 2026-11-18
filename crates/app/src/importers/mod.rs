//! Importers: convert another DJ program's controller map into a Dubplate [`Profile`].
//!
//! Each importer reads one source format and fills a [`Builder`], which owns the rules every
//! importer shares (one function per message, validation, hi-res detection) and records, for
//! every control it sees, what the other program calls the function and whether Dubplate has an
//! equivalent. [`survey`] turns those records into reports (kept out of git: they describe other programs' files).

pub mod compare;
pub mod djuced;
pub mod mixxx;
pub mod rekordbox;
pub mod survey;

use std::collections::HashMap;
use std::path::Path;

use crate::midi_profile::{Bind, DeviceInfo, Func, Profile};

/// One control in the source map, and what became of it.
#[derive(Clone, Debug)]
pub struct Seen {
    /// The source program's name for the function, normalised (`hotcue_N_activate`).
    pub function: String,
    /// Dubplate function it maps to, with its argument (`pad 2`), if any.
    pub dubplate: Option<String>,
    /// Why it was left out, when `dubplate` is None.
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub converted: usize,
    /// (control name, reason)
    pub skipped: Vec<(String, String)>,
    /// Conversions that are close but not identical to the original behaviour.
    pub approximations: Vec<String>,
    /// Every function-bearing control, mapped or not (feeds the docs survey).
    pub seen: Vec<Seen>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Djuced,
    Mixxx,
    Rekordbox,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Djuced => "DJUCED",
            Source::Mixxx => "Mixxx",
            Source::Rekordbox => "rekordbox",
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            Source::Djuced => "djuced",
            Source::Mixxx => "mixxx",
            Source::Rekordbox => "rekordbox",
        }
    }
    pub fn from_slug(s: &str) -> Option<Source> {
        [Source::Djuced, Source::Mixxx, Source::Rekordbox].into_iter().find(|x| x.slug() == s)
    }
    /// File-name ending of this program's MIDI maps (Mixxx also has `.hid.xml`; rekordbox `.pad.csv`).
    pub fn suffix(self) -> &'static str {
        match self {
            Source::Djuced => ".djm",
            Source::Mixxx => ".midi.xml",
            Source::Rekordbox => ".midi.csv",
        }
    }
}

/// Reads a map file. The formats are ASCII / Latin-1, so a stray byte must not fail the import.
pub fn read_text(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    };
    // Some files start with a UTF-8 byte-order mark, which would hide `@file` / `<?xml`.
    Ok(text.trim_start_matches('\u{feff}').to_owned())
}

/// Guesses the source program from the file name and contents.
pub fn detect(path: &Path, text: &str) -> Option<Source> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let head: String = text.chars().take(4000).collect();
    match ext.as_str() {
        "djm" => Some(Source::Djuced),
        "csv" if head.trim_start().starts_with("@file") => Some(Source::Rekordbox),
        "xml" if head.contains("MixxxControllerPreset") || head.contains("MixxxMIDIPreset") => Some(Source::Mixxx),
        _ => None,
    }
}

/// `file_stem` names the device when the format has no name of its own (rekordbox CSV).
pub fn import(source: Source, text: &str, file_stem: &str) -> Result<(Profile, Report), String> {
    match source {
        Source::Djuced => djuced::import(text),
        Source::Mixxx => mixxx::import(text),
        Source::Rekordbox => rekordbox::import(text, file_stem),
    }
}

/// Port-name fragments that should select a profile for `device`: the full name, the name without the
/// maker, and without a trailing generation / "MIDI" word.
pub fn port_matches(device: &str) -> Vec<String> {
    const MAKERS: &[&str] = &[
        "Pioneer DJ ",
        "PIONEER ",
        "Pioneer ",
        "Hercules ",
        "Numark ",
        "Denon DJ ",
        "Denon ",
        "Reloop ",
        "Native Instruments ",
    ];
    let mut out = vec![device.trim().to_owned()];
    let mut short = device.trim();
    for m in MAKERS {
        if let Some(rest) = short.strip_prefix(m) {
            short = rest.trim();
            break;
        }
    }
    let mut push = |s: &str| {
        let s = s.trim();
        if s.len() >= 4 && !out.iter().any(|o| o.eq_ignore_ascii_case(s)) {
            out.push(s.to_owned());
        }
    };
    push(short);
    let mut words: Vec<&str> = short.split_whitespace().collect();
    while words.last().is_some_and(|w| {
        let w = w.to_ascii_lowercase();
        w == "midi" || w == "mk2" || w == "mkii" || w == "mk3" || w == "mk1"
    }) {
        words.pop();
    }
    push(&words.join(" "));
    out
}

/// Tie-break when a port name fits several profiles: the newer hardware revision wins.
pub fn default_priority(device: &str) -> i32 {
    let d = device.to_ascii_lowercase();
    i32::from(d.contains("mk2") || d.contains("mkii") || d.contains("mk3"))
}

/// Collects binds while enforcing the rules every importer shares.
pub struct Builder {
    binds: Vec<Bind>,
    taken: HashMap<(u8, u8), String>,
    pub report: Report,
}

impl Builder {
    pub fn new() -> Self {
        Self { binds: Vec::new(), taken: HashMap::new(), report: Report::default() }
    }

    pub fn skip(&mut self, label: &str, function: &str, why: impl Into<String>) {
        let why = why.into();
        self.report.skipped.push((label.to_owned(), why.clone()));
        self.report.seen.push(Seen { function: function.to_owned(), dubplate: None, reason: Some(why) });
    }

    /// Adds one binding. `function` is the source program's (normalised) name for it.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        label: &str,
        function: &str,
        status: u8,
        data1: u8,
        func: &str,
        deck: Option<u8>,
        arg: Option<i32>,
        hires: bool,
        approx: Option<&str>,
    ) -> bool {
        let Some(def) = Func::from_name(func) else {
            self.skip(label, function, format!("unknown Dubplate function `{func}`"));
            return false;
        };
        if def.is_global() != deck.is_none() {
            self.skip(label, function, format!("`{function}` is bound to the wrong kind of channel"));
            return false;
        }
        let msg = crate::midi_profile::fold_status(status);
        let described = match arg {
            Some(a) => format!("{func} {a}"),
            None => func.to_owned(),
        };
        let bad_pad = matches!(def, Func::Pad | Func::PadDelete) && !arg.is_some_and(|a| (0..8).contains(&a));
        if bad_pad {
            self.skip(label, function, "pad number out of range");
            return false;
        }
        if matches!(def, Func::LoopBeats) && arg.is_none() {
            self.skip(label, function, "loop length unreadable");
            return false;
        }
        if let Some(prev) = self.taken.get(&(msg, data1)) {
            let why = format!("same message as {prev}, which already has a function");
            self.skip(label, function, why);
            return false;
        }
        if let Some(a) = approx {
            self.report.approximations.push(format!("{label}: {a}"));
        }
        self.taken.insert((msg, data1), label.to_owned());
        self.binds.push(Bind {
            msg: format!("{msg:02X} {data1:02X}"),
            func: func.to_owned(),
            deck,
            arg,
            hires: hires && msg & 0xF0 == 0xB0,
            label: label.to_owned(),
        });
        self.report.converted += 1;
        self.report.seen.push(Seen { function: function.to_owned(), dubplate: Some(described), reason: None });
        true
    }

    pub fn finish(mut self, device: DeviceInfo) -> Result<(Profile, Report), String> {
        self.binds.sort_by(|a, b| a.msg.cmp(&b.msg));
        let profile = Profile { device, binds: self.binds };
        profile.compile()?; // never hand back something the loader would reject
        Ok((profile, self.report))
    }
}

/// Parses a hex byte pair like "9641" / "0x91" into the numbers; `None` if malformed.
pub fn hex_byte(s: &str) -> Option<u8> {
    u8::from_str_radix(s.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_match_variants() {
        let m = port_matches("Hercules DJControl Inpulse 300 Mk2");
        assert!(m.contains(&"Hercules DJControl Inpulse 300 Mk2".to_owned()));
        assert!(m.contains(&"DJControl Inpulse 300 Mk2".to_owned()));
        assert!(m.contains(&"DJControl Inpulse 300".to_owned()));
        let m = port_matches("DDJ-400");
        assert_eq!(m, vec!["DDJ-400".to_owned()]);
    }

    #[test]
    fn newer_revisions_win_ties() {
        assert_eq!(default_priority("DJControl Inpulse 200 Mk2"), 1);
        assert_eq!(default_priority("DJControl Inpulse 200"), 0);
    }

    #[test]
    fn builder_enforces_one_function_per_message() {
        let mut b = Builder::new();
        assert!(b.add("PLAY_A", "play_pause", 0x91, 0x07, "play_pause", Some(0), None, false, None));
        assert!(!b.add("DUP", "cue_stop", 0x91, 0x07, "cue", Some(0), None, false, None));
        assert!(!b.add("XF", "crossfader", 0xB0, 0, "crossfader", Some(0), None, false, None), "global needs no deck");
        assert!(!b.add("PAD", "hot_cue", 0x96, 0, "pad", Some(0), Some(9), false, None));
        assert_eq!(b.report.converted, 1);
        assert_eq!(b.report.skipped.len(), 3);
        assert_eq!(b.report.seen.len(), 4);
    }

    #[test]
    fn note_off_and_note_on_are_the_same_message() {
        let mut b = Builder::new();
        assert!(b.add("A", "play", 0x91, 0x07, "play_pause", Some(0), None, false, None));
        assert!(!b.add("B", "play", 0x81, 0x07, "play_pause", Some(0), None, false, None));
    }

    #[test]
    fn detection_uses_extension_and_content() {
        let p = |s: &str| std::path::PathBuf::from(s);
        assert_eq!(detect(&p("a.djm"), "<midi/>"), Some(Source::Djuced));
        assert_eq!(detect(&p("a.csv"), "@file,1,X"), Some(Source::Rekordbox));
        assert_eq!(detect(&p("a.csv"), "a,b"), None);
        assert_eq!(detect(&p("a.xml"), "<MixxxControllerPreset>"), Some(Source::Mixxx));
        assert_eq!(detect(&p("a.xml"), "<other/>"), None);
    }
}
