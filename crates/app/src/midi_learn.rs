//! MIDI learn: build or amend a controller profile by moving the controls.
//!
//! The MIDI callback hands every message to [`observe`] while learning is on, and the Preferences
//! window picks the result up with [`take_settled`]. Everything here is plain data so it can be tested
//! without a controller or a window.
//!
//! A control that sends 14-bit values sends its MSB on CC n and its LSB on CC n+0x20, so after the
//! first message arrives we keep listening for a moment ([`SETTLE`]) to see whether its partner follows.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::midi_profile::{fold_status, parse_msg, Bind, DeviceInfo, Func, Profile, ProfileEntry};

/// How long after the first message we wait for a 14-bit partner before accepting the capture.
pub const SETTLE: Duration = Duration::from_millis(350);

/// What sort of control a function expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A note (button or pad).
    Button,
    /// A fader or knob sending a CC with an absolute position.
    Absolute,
    /// An encoder sending a CC with signed steps.
    Relative,
}

impl Kind {
    pub fn of(func: Func) -> Kind {
        match func {
            Func::Volume
            | Func::Gain
            | Func::EqHigh
            | Func::EqMid
            | Func::EqLow
            | Func::Filter
            | Func::Tempo
            | Func::Crossfader => Kind::Absolute,
            Func::JogBend | Func::JogScratch | Func::JogSearch | Func::Browse => Kind::Relative,
            _ => Kind::Button,
        }
    }

    fn wants(self) -> &'static str {
        match self {
            Kind::Button => "a button or pad",
            Kind::Absolute => "a fader or knob",
            Kind::Relative => "an endless encoder or jog wheel",
        }
    }
}

// ── capture ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Capture {
    /// Normalised name of the port the message came from.
    pub port: String,
    /// Status byte with note-off folded into note-on.
    pub status: u8,
    pub data1: u8,
    /// A matching LSB controller was seen, so the control is 14-bit.
    pub hires: bool,
    pub at: Instant,
}

impl Capture {
    pub fn is_note(&self) -> bool {
        self.status & 0xF0 == 0x90
    }
    pub fn is_cc(&self) -> bool {
        self.status & 0xF0 == 0xB0
    }
    pub fn msg(&self) -> String {
        format!("{:02X} {:02X}", self.status, self.data1)
    }

    /// Whether this control suits `kind`; the error says what to move instead.
    pub fn suits(&self, kind: Kind) -> Result<(), String> {
        let ok = match kind {
            Kind::Button => self.is_note(),
            Kind::Absolute | Kind::Relative => self.is_cc(),
        };
        if ok {
            return Ok(());
        }
        let got = if self.is_note() { "a button" } else { "a fader or knob" };
        Err(format!("That control is {got}; this function needs {}.", kind.wants()))
    }
}

#[derive(Clone, Debug, Default)]
pub enum LearnState {
    #[default]
    Off,
    /// Waiting for the user to move something.
    Waiting,
    Got(Capture),
}

/// Shared between the MIDI callbacks and the window.
pub type Learn = Arc<Mutex<LearnState>>;

pub fn begin(learn: &Learn) {
    if let Ok(mut s) = learn.lock() {
        *s = LearnState::Waiting;
    }
}

pub fn cancel(learn: &Learn) {
    if let Ok(mut s) = learn.lock() {
        *s = LearnState::Off;
    }
}

/// True while messages should be captured rather than acted on.
#[cfg(test)]
pub fn is_active(learn: &Learn) -> bool {
    learn.lock().is_ok_and(|s| !matches!(*s, LearnState::Off))
}

/// Feeds one incoming message to the learner. System messages, releases and clock are ignored.
pub fn observe(state: &mut LearnState, port: &str, msg: &[u8], now: Instant) {
    if msg.len() < 2 {
        return;
    }
    let status = fold_status(msg[0]);
    let value = msg.get(2).copied().unwrap_or(0);
    let is_note = status & 0xF0 == 0x90;
    let is_cc = status & 0xF0 == 0xB0;
    if !is_note && !is_cc {
        return;
    }

    match state {
        LearnState::Off => {}
        LearnState::Waiting => {
            // The release of a button pressed before learning started is not a choice.
            if is_note && value == 0 {
                return;
            }
            *state = LearnState::Got(Capture { port: port.to_owned(), status, data1: msg[1], hires: false, at: now });
        }
        LearnState::Got(c) => {
            if c.port != port || !is_cc || c.status != status {
                return;
            }
            if c.data1 < 0x20 && msg[1] == c.data1 + 0x20 {
                c.hires = true;
            } else if (0x20..0x40).contains(&c.data1) && msg[1] == c.data1 - 0x20 {
                // The LSB came first: the control is named by its MSB.
                c.data1 = msg[1];
                c.hires = true;
            }
        }
    }
}

/// The capture, once it has had [`SETTLE`] to pick up a 14-bit partner. Switches learning off.
pub fn take_settled(learn: &Learn, now: Instant) -> Option<Capture> {
    let mut s = learn.lock().ok()?;
    let LearnState::Got(c) = &*s else { return None };
    if now.duration_since(c.at) < SETTLE {
        return None;
    }
    let c = c.clone();
    *s = LearnState::Off;
    Some(c)
}

// ── what can be learned ──────────────────────────────────────────────────────

/// One learnable function: which deck (None for global ones) and which pad / stem / loop length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub func: Func,
    pub deck: Option<u8>,
    pub arg: Option<i32>,
}

const STEM_NAMES: [&str; 4] = ["drums", "bass", "other", "vocals"];
const LOOP_BEATS: [i32; 5] = [1, 2, 4, 8, 16];

impl Slot {
    pub fn label(&self) -> String {
        let deck = match self.deck {
            Some(0) => " · A",
            Some(_) => " · B",
            None => "",
        };
        let a = self.arg.unwrap_or(0);
        let name = match self.func {
            Func::Pad => format!("Hot cue {}", (b'A' + a as u8) as char),
            Func::PadDelete => format!("Delete hot cue {}", (b'A' + a as u8) as char),
            Func::Stem => format!("Mute {}", STEM_NAMES.get(a as usize).copied().unwrap_or("stem")),
            Func::LoopBeats => format!("{a}-beat loop"),
            f => crate::midi_profile::FUNCS.iter().find(|x| x.0 == f).map_or("?", |x| x.3).to_owned(),
        };
        format!("{name}{deck}")
    }

    /// Does this bind belong to the slot? Functions without an argument match whatever it says.
    pub fn owns(&self, b: &Bind) -> bool {
        if b.func != self.func.name() {
            return false;
        }
        if !self.func.is_global() && b.deck != self.deck {
            return false;
        }
        match self.arg {
            Some(a) => b.arg == Some(a),
            None => true,
        }
    }
}

pub struct Group {
    pub title: &'static str,
    pub slots: Vec<Slot>,
}

fn per_deck(funcs: &[Func]) -> Vec<Slot> {
    funcs
        .iter()
        .flat_map(|&func| {
            let decks: &[Option<u8>] = if func.is_global() { &[None] } else { &[Some(0), Some(1)] };
            decks.iter().map(move |&deck| Slot { func, deck, arg: None })
        })
        .collect()
}

fn with_args(func: Func, args: impl Iterator<Item = i32>) -> Vec<Slot> {
    let args: Vec<i32> = args.collect();
    [Some(0u8), Some(1)]
        .into_iter()
        .flat_map(|deck| args.iter().map(move |&a| Slot { func, deck, arg: Some(a) }))
        .collect()
}

/// Every learnable slot, in the order the editor lists them.
pub fn groups() -> Vec<Group> {
    use Func::*;
    vec![
        Group {
            title: "Transport",
            slots: per_deck(&[PlayPause, Cue, ToStart, Sync, Load, Quantize, KeyLock, AutoGain]),
        },
        Group {
            title: "Mixer",
            slots: per_deck(&[
                Volume,
                Gain,
                EqHigh,
                EqMid,
                EqLow,
                Filter,
                Tempo,
                Crossfader,
                HeadphoneCue,
                MasterMonitor,
            ]),
        },
        Group {
            title: "Loops",
            slots: {
                let mut v = per_deck(&[LoopIn, LoopOut, LoopToggle, LoopHalf, LoopDouble]);
                v.extend(with_args(LoopBeats, LOOP_BEATS.into_iter()));
                v
            },
        },
        Group { title: "Hot cues", slots: with_args(Pad, 0..8) },
        Group { title: "Delete hot cues", slots: with_args(PadDelete, 0..8) },
        Group { title: "Stems", slots: with_args(Stem, 0..4) },
        Group {
            title: "Jog wheels and browser",
            slots: per_deck(&[JogBend, JogScratch, JogSearch, JogTouch, Vinyl, Browse]),
        },
    ]
}

// ── editing a profile ────────────────────────────────────────────────────────

/// Index of every bind in `profile` that belongs to `slot`.
pub fn binds_of(profile: &Profile, slot: Slot) -> Vec<usize> {
    profile.binds.iter().enumerate().filter(|(_, b)| slot.owns(b)).map(|(i, _)| i).collect()
}

/// Binds `cap` to `slot`. `replace` drops the slot's earlier controls first; otherwise the new one is
/// added beside them (handy for a shift layer). A control can do only one thing, so anything else it
/// was bound to loses it; the return value names that function.
pub fn apply(profile: &mut Profile, slot: Slot, cap: &Capture, replace: bool) -> Option<String> {
    let key = (cap.status, cap.data1);
    let mut displaced = None;
    profile.binds.retain(|b| {
        if parse_msg(&b.msg).ok() != Some(key) {
            return true;
        }
        if !slot.owns(b) {
            displaced = Some(describe(b));
        }
        false
    });
    if replace {
        profile.binds.retain(|b| !slot.owns(b));
    }
    profile.binds.push(Bind {
        msg: cap.msg(),
        func: slot.func.name().to_owned(),
        deck: if slot.func.is_global() { None } else { slot.deck },
        arg: slot.arg,
        hires: cap.hires && Kind::of(slot.func) == Kind::Absolute && cap.data1 < 0x20,
        label: "learned".to_owned(),
    });
    displaced
}

fn describe(b: &Bind) -> String {
    let slot_deck = b.deck;
    let func = Func::from_name(&b.func);
    match func {
        Some(f) => Slot { func: f, deck: slot_deck, arg: b.arg }.label(),
        None => b.func.clone(),
    }
}

/// A profile being edited in the Preferences window.
pub struct Editor {
    /// The port this edit is for; the saved profile gets assigned to it.
    pub port: String,
    pub profile: Profile,
    /// The slot waiting for a control, and whether the result replaces its earlier controls.
    pub learning: Option<(Slot, bool)>,
    /// Latest outcome: (text, ok?).
    pub note: Option<(String, bool)>,
    pub dirty: bool,
}

impl Editor {
    /// Starts from `base` (the port's current profile) or from nothing. A built-in is copied under a
    /// new name and made to win detection, so the original stays untouched.
    pub fn start(port_key: &str, base: Option<&ProfileEntry>) -> Editor {
        let profile = match base {
            Some(e) if e.builtin => {
                let mut p = e.profile.clone();
                p.device.name = format!("{} (custom)", p.device.name);
                p.device.priority += 10;
                p.device.source = format!("learned from {}", e.profile.device.name);
                p
            }
            Some(e) => e.profile.clone(),
            None => Profile {
                device: DeviceInfo {
                    name: port_key.to_owned(),
                    port_match: vec![port_key.to_owned()],
                    priority: 10,
                    source: "learned".to_owned(),
                    notes: String::new(),
                },
                binds: Vec::new(),
            },
        };
        Editor { port: port_key.to_owned(), profile, learning: None, note: None, dirty: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_mapper::{AppCommand, MidiMapper};

    fn blank() -> Profile {
        Editor::start("Test Pad MIDI 1", None).profile
    }

    fn cap(status: u8, data1: u8) -> Capture {
        Capture { port: "p".into(), status, data1, hires: false, at: Instant::now() }
    }

    fn feed(state: &mut LearnState, msg: &[u8]) {
        observe(state, "p", msg, Instant::now());
    }

    #[test]
    fn captures_a_button_press_but_not_its_release() {
        let mut s = LearnState::Waiting;
        feed(&mut s, &[0x91, 0x06, 0x00]);
        assert!(matches!(s, LearnState::Waiting), "a stray release is not a choice");
        feed(&mut s, &[0xF8]);
        assert!(matches!(s, LearnState::Waiting), "clock is ignored");
        feed(&mut s, &[0x91, 0x06, 0x7F]);
        let LearnState::Got(c) = &s else { panic!("{s:?}") };
        assert_eq!((c.status, c.data1, c.hires), (0x91, 0x06, false));
    }

    #[test]
    fn nothing_is_captured_unless_learning() {
        let mut s = LearnState::Off;
        feed(&mut s, &[0x91, 0x06, 0x7F]);
        assert!(matches!(s, LearnState::Off));
    }

    #[test]
    fn a_fader_with_an_lsb_is_fourteen_bit() {
        let mut s = LearnState::Waiting;
        feed(&mut s, &[0xB1, 0x00, 0x40]);
        feed(&mut s, &[0xB1, 0x20, 0x11]);
        let LearnState::Got(c) = &s else { panic!() };
        assert!(c.hires);
        assert_eq!(c.data1, 0x00);
    }

    #[test]
    fn an_lsb_arriving_first_is_named_by_its_msb() {
        let mut s = LearnState::Waiting;
        feed(&mut s, &[0xB1, 0x20, 0x11]);
        feed(&mut s, &[0xB1, 0x00, 0x40]);
        let LearnState::Got(c) = &s else { panic!() };
        assert!((c.hires, c.data1) == (true, 0x00));
    }

    #[test]
    fn other_controls_do_not_disturb_a_capture() {
        let mut s = LearnState::Waiting;
        feed(&mut s, &[0xB1, 0x00, 0x40]);
        feed(&mut s, &[0xB1, 0x05, 0x10]); // not its partner
        feed(&mut s, &[0xB2, 0x20, 0x10]); // right number, wrong channel
        observe(&mut s, "other port", &[0xB1, 0x20, 0x10], Instant::now());
        let LearnState::Got(c) = &s else { panic!() };
        assert!(!c.hires);
    }

    #[test]
    fn a_capture_settles_after_a_moment() {
        let learn: Learn = Arc::new(Mutex::new(LearnState::Waiting));
        {
            let mut g = learn.lock().unwrap();
            observe(&mut g, "p", &[0x90, 0x01, 0x7F], Instant::now());
        }
        assert!(take_settled(&learn, Instant::now()).is_none(), "too soon");
        assert!(is_active(&learn));
        let c = take_settled(&learn, Instant::now() + SETTLE * 2).expect("settled");
        assert_eq!(c.msg(), "90 01");
        assert!(!is_active(&learn), "learning ends once taken");
    }

    #[test]
    fn controls_must_suit_the_function() {
        assert!(cap(0x91, 6).suits(Kind::Button).is_ok());
        assert!(cap(0xB1, 6).suits(Kind::Absolute).is_ok());
        assert!(cap(0xB1, 6).suits(Kind::Relative).is_ok());
        let e = cap(0xB1, 6).suits(Kind::Button).unwrap_err();
        assert!(e.contains("fader") && e.contains("button"), "{e}");
        assert!(cap(0x91, 6).suits(Kind::Absolute).is_err());
        assert_eq!(Kind::of(Func::Volume), Kind::Absolute);
        assert_eq!(Kind::of(Func::JogScratch), Kind::Relative);
        assert_eq!(Kind::of(Func::Pad), Kind::Button);
    }

    #[test]
    fn every_function_can_be_learned_and_still_compiles() {
        // Every function in the table is reachable from the editor.
        let mut seen = std::collections::HashSet::new();
        let mut p = blank();
        let mut n = 0u8;
        for g in groups() {
            for slot in g.slots {
                seen.insert(slot.func.name());
                // Distinct messages, so nothing displaces anything.
                let (ch, d1) = ((n / 0x20) % 16, n % 0x20);
                let c = match Kind::of(slot.func) {
                    Kind::Button => cap(0x90 | ch, d1),
                    _ => cap(0xB0 | ch, d1),
                };
                n += 1;
                assert_eq!(apply(&mut p, slot, &c, true), None);
            }
        }
        assert_eq!(p.binds.len(), groups().iter().map(|g| g.slots.len()).sum::<usize>());
        for (f, name, _, _) in crate::midi_profile::FUNCS {
            assert!(seen.contains(name), "{name} ({f:?}) is missing from the editor");
        }
        p.compile().expect("a fully learned profile compiles");
    }

    #[test]
    fn learning_replaces_or_adds() {
        let mut p = blank();
        let cue = Slot { func: Func::Cue, deck: Some(0), arg: None };
        apply(&mut p, cue, &cap(0x91, 6), true);
        apply(&mut p, cue, &cap(0x94, 6), true);
        assert_eq!(binds_of(&p, cue).len(), 1, "replace swaps the control");
        assert_eq!(p.binds[0].msg, "94 06");
        apply(&mut p, cue, &cap(0x91, 6), false);
        assert_eq!(binds_of(&p, cue).len(), 2, "add keeps both (a shift layer)");
    }

    #[test]
    fn a_control_can_only_do_one_thing() {
        let mut p = blank();
        let cue = Slot { func: Func::Cue, deck: Some(0), arg: None };
        let play = Slot { func: Func::PlayPause, deck: Some(0), arg: None };
        assert_eq!(apply(&mut p, cue, &cap(0x91, 6), true), None);
        let displaced = apply(&mut p, play, &cap(0x91, 6), true);
        assert_eq!(displaced.as_deref(), Some("CUE (press and release) · A"));
        assert!(binds_of(&p, cue).is_empty());
        assert_eq!(binds_of(&p, play).len(), 1);
    }

    #[test]
    fn pads_and_decks_are_told_apart() {
        let mut p = blank();
        let pad = |deck, n| Slot { func: Func::Pad, deck: Some(deck), arg: Some(n) };
        apply(&mut p, pad(0, 0), &cap(0x96, 0), true);
        apply(&mut p, pad(0, 1), &cap(0x96, 1), true);
        apply(&mut p, pad(1, 0), &cap(0x97, 0), true);
        apply(&mut p, pad(0, 0), &cap(0x96, 9), true);
        assert_eq!(p.binds.len(), 3, "re-learning pad A of deck A touched nothing else");
        assert_eq!(binds_of(&p, pad(0, 1)).len(), 1);
        assert_eq!(binds_of(&p, pad(1, 0)).len(), 1);
    }

    #[test]
    fn a_learned_profile_drives_the_mapper() {
        let mut p = blank();
        let vol = Slot { func: Func::Volume, deck: Some(0), arg: None };
        let cf = Slot { func: Func::Crossfader, deck: None, arg: None };
        let cue = Slot { func: Func::Cue, deck: Some(1), arg: None };
        let mut fader = cap(0xB1, 0x00);
        fader.hires = true;
        apply(&mut p, vol, &fader, true);
        apply(&mut p, cf, &cap(0xB0, 0x07), true);
        apply(&mut p, cue, &cap(0x92, 0x06), true);
        assert!(p.binds[0].hires && p.binds[1].deck.is_none());

        let mut m = MidiMapper::new(&p).unwrap();
        m.process(&[0xB1, 0x20, 0x7F]);
        match m.process(&[0xB1, 0x00, 0x7F]) {
            Some(AppCommand::DeckSetVolume(0, v)) => assert!((v - 1.5).abs() < 1e-3, "{v}"),
            o => panic!("{o:?}"),
        }
        assert!(matches!(m.process(&[0xB0, 0x07, 0x7F]), Some(AppCommand::SetCrossfader(_))));
        assert!(m.process(&[0x92, 0x06, 0x7F]).is_some());
    }

    #[test]
    fn only_absolute_cc_controls_are_fourteen_bit() {
        let mut p = blank();
        let mut c = cap(0xB1, 0x01);
        c.hires = true;
        apply(&mut p, Slot { func: Func::Browse, deck: None, arg: None }, &c, true);
        assert!(!p.binds[0].hires, "an encoder is never 14-bit");
        p.compile().unwrap();
    }

    #[test]
    fn a_built_in_is_copied_not_overwritten() {
        let builtin =
            crate::midi_profile::load_profiles().into_iter().find(|p| p.id == "djcontrol-inpulse-200-mk2").unwrap();
        let ed = Editor::start("Inpulse 200 MIDI 1", Some(&builtin));
        assert!(ed.profile.device.name.ends_with("(custom)"));
        assert_eq!(ed.profile.device.priority, builtin.profile.device.priority + 10, "the copy wins detection");
        assert_eq!(ed.profile.binds.len(), builtin.profile.binds.len());
        assert_ne!(crate::midi_profile::slug(&ed.profile.device.name), builtin.id);
    }

    #[test]
    fn a_new_profile_matches_its_own_port() {
        let ed = Editor::start("Acme Pad MIDI 1", None);
        assert_eq!(ed.profile.device.port_match, vec!["Acme Pad MIDI 1"]);
        assert!(ed.profile.binds.is_empty());
    }

    #[test]
    fn slot_labels_read_naturally() {
        assert_eq!(Slot { func: Func::Pad, deck: Some(1), arg: Some(2) }.label(), "Hot cue C · B");
        assert_eq!(Slot { func: Func::Stem, deck: Some(0), arg: Some(3) }.label(), "Mute vocals · A");
        assert_eq!(Slot { func: Func::LoopBeats, deck: Some(0), arg: Some(8) }.label(), "8-beat loop · A");
        assert_eq!(Slot { func: Func::Crossfader, deck: None, arg: None }.label(), "Crossfader");
    }
}
