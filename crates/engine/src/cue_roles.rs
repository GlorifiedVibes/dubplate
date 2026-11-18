//! Hot-cue roles: the places a DJ usually wants a pad on, found from the sections of a track.
//!
//! A start (the first bar), the build that leads into the first drop, the first drop, the first break after it, the
//! second drop and the outro. A role is left out when the track has no such part, and positions come straight from
//! [`Structure`], so they are on bar lines.

use crate::structure::{Kind, Structure};

/// One suggested hot cue.
#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    /// Hot-cue pad, 1-8 (A-H).
    pub pad: u8,
    /// Position in seconds into the track.
    pub secs: f64,
    /// What it marks, also used as the cue's label.
    pub name: &'static str,
}

/// A drop is a lead-in when the drop right after it is at least this many times as energetic.
const LOUDER: f32 = 1.1;

/// Cues closer than this are one place, not two.
const MIN_APART_SECS: f64 = 1.0;

/// Suggested cues for a track whose first bar line is at `first_bar_secs`. Pads: A start, B build, C first drop,
/// D first break after it, E second drop, F outro. Fewer when the track lacks a part.
pub fn suggest(s: &Structure, first_bar_secs: f64) -> Vec<Role> {
    let secs = &s.sections;
    let mut out: Vec<Role> = Vec::new();
    let mut put = |pad: u8, at: f64, name: &'static str| {
        if out.iter().all(|r| (r.secs - at).abs() >= MIN_APART_SECS) {
            out.push(Role { pad, secs: at, name });
        }
    };
    put(1, first_bar_secs.max(0.0), "Start");
    // A drop that runs straight into a clearly louder drop is the lead-in to it (Bangarang's bar 5 part), not the drop;
    // a quieter drop after a break is a drop of its own (Sandstorm's first)
    let lead_in = |i: usize| {
        secs[i].kind == Kind::Drop
            && secs.get(i + 1).is_some_and(|n| n.kind == Kind::Drop && secs[i].energy * LOUDER <= n.energy)
    };
    let first_drop = (0..secs.len()).find(|&i| secs[i].kind == Kind::Drop && !lead_in(i));
    if let Some(d) = first_drop {
        // the build is the last rising stretch before the drop, whichever kind of section came right before it
        if let Some(b) = secs[..d].iter().rposition(|x| x.kind == Kind::Build) {
            put(2, secs[b].start_secs, "Build");
        }
        put(3, secs[d].start_secs, "Drop");
        if let Some(k) = secs[d + 1..].iter().position(|x| x.kind == Kind::Break).map(|k| k + d + 1) {
            put(4, secs[k].start_secs, "Break");
            // the second drop is the first drop that comes back after a break
            if let Some(e) = secs[k + 1..].iter().position(|x| x.kind == Kind::Drop).map(|e| e + k + 1) {
                put(5, secs[e].start_secs, "Drop 2");
            }
        }
    }
    if let Some(o) = secs.iter().find(|x| x.kind == Kind::Outro) {
        put(6, o.start_secs, "Outro");
    }
    out.sort_by_key(|r| r.pad);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::Section;

    fn sec(start: f64, end: f64, kind: Kind) -> Section {
        Section { start_secs: start, end_secs: end, beats: 32, kind, energy: 1.0 }
    }
    fn song(kinds: &[Kind]) -> Structure {
        Structure {
            sections: kinds.iter().enumerate().map(|(i, &k)| sec(i as f64 * 20.0, i as f64 * 20.0 + 20.0, k)).collect(),
            phrase: None,
        }
    }

    #[test]
    fn a_full_song_gets_all_six() {
        let s = song(&[Kind::Intro, Kind::Build, Kind::Drop, Kind::Break, Kind::Build, Kind::Drop, Kind::Outro]);
        let r = suggest(&s, 0.5);
        let names: Vec<_> = r.iter().map(|r| (r.pad, r.name, r.secs)).collect();
        assert_eq!(
            names,
            vec![
                (1, "Start", 0.5),
                (2, "Build", 20.0),
                (3, "Drop", 40.0),
                (4, "Break", 60.0),
                (5, "Drop 2", 100.0),
                (6, "Outro", 120.0)
            ]
        );
    }

    #[test]
    fn missing_parts_are_left_out() {
        // no build, no break: start, drop, outro only, still on their own pads
        let r = suggest(&song(&[Kind::Intro, Kind::Drop, Kind::Outro]), 0.0);
        assert_eq!(r.iter().map(|r| r.pad).collect::<Vec<_>>(), vec![1, 3, 6]);
        // nothing found at all: just the start
        assert_eq!(suggest(&Structure::default(), 1.2).len(), 1);
    }

    #[test]
    fn a_drop_with_no_break_after_it_has_no_second_drop() {
        let r = suggest(&song(&[Kind::Intro, Kind::Drop, Kind::Drop, Kind::Outro]), 0.0);
        assert!(r.iter().all(|r| r.pad != 5 && r.pad != 4));
    }

    #[test]
    fn a_drop_running_into_a_louder_one_is_a_lead_in_not_the_drop() {
        let mut s = song(&[Kind::Intro, Kind::Drop, Kind::Drop, Kind::Break, Kind::Drop]);
        s.sections[1].energy = 0.8;
        s.sections[2].energy = 0.96;
        s.sections[4].energy = 0.94;
        let r = suggest(&s, 0.0);
        assert_eq!(r.iter().find(|r| r.name == "Drop").map(|r| r.secs), Some(40.0));
        assert_eq!(r.iter().find(|r| r.name == "Drop 2").map(|r| r.secs), Some(80.0));
    }

    #[test]
    fn a_quieter_first_drop_after_a_break_is_still_a_drop() {
        let mut s = song(&[Kind::Intro, Kind::Drop, Kind::Break, Kind::Drop]);
        s.sections[1].energy = 0.7;
        s.sections[3].energy = 0.95;
        let r = suggest(&s, 0.0);
        assert_eq!(r.iter().find(|r| r.name == "Drop").map(|r| r.secs), Some(20.0));
        assert_eq!(r.iter().find(|r| r.name == "Break").map(|r| r.secs), Some(40.0));
    }

    #[test]
    fn two_cues_on_one_spot_are_one() {
        // the track starts straight in on a drop: Start and Drop are the same place, Start wins
        let r = suggest(&song(&[Kind::Drop, Kind::Break, Kind::Drop]), 0.0);
        assert_eq!(r.iter().map(|r| r.name).collect::<Vec<_>>(), vec!["Start", "Break", "Drop 2"]);
    }
}
