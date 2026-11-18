use std::collections::HashMap;

use crate::app::DeckAction;
use crate::midi_profile::{fold_status, index, Compiled, Func, Profile};

/// Commands the MIDI mapper sends to the UI/app layer.
#[derive(Debug, Clone)]
pub enum AppCommand {
    DeckPlayPause(usize),
    /// Cue / hot-cue / loop actions (shared with the GUI buttons).
    Action(usize, DeckAction),
    DeckLoad(usize),
    /// SYNC button.
    DeckSync(usize),
    /// KEY (key lock) button.
    DeckKeyLock(usize),
    /// AUTO (auto gain) button.
    DeckAutoGain(usize),
    /// Browser encoder: rows to move the selection (negative = up).
    Browse(i32),
    /// Headphone cue on / off for a deck.
    ToggleCue(usize),
    /// Main mix in the headphones on / off.
    ToggleMasterMonitor,
    DeckSetBrake(usize, bool), // true = top-surface touched (scratch), false = released
    DeckSetVolume(usize, f32),
    DeckSetGain(usize, f32),
    DeckSetTempo(usize, f32),
    DeckJogBend(usize, f32),
    /// Search: signed jog steps to move the playhead by.
    DeckSearch(usize, i32),
    DeckEqHigh(usize, f32),
    DeckEqMid(usize, f32),
    DeckEqLow(usize, f32),
    DeckSetFilter(usize, f32),
    SetCrossfader(f32),
    /// Mk2 stem pads — toggle mute for stem `stem` (0=drums,1=bass,2=other,3=vocals).
    DeckStemToggle(usize, usize),
}

fn cc_to_01(v: f32) -> f32 {
    v
}

/// Relative encoder value as a signed step: two's complement (1 = +1, 0x7F = -1).
fn relative(value: u8) -> i32 {
    if value < 0x40 {
        value as i32
    } else {
        value as i32 - 128
    }
}

/// Turns incoming MIDI into [`AppCommand`]s according to one controller profile.
pub struct MidiMapper {
    bindings: HashMap<(u8, u8), Compiled>,
    /// (status, lsb cc) -> (status, msb cc) for 14-bit controls.
    lsb_of: HashMap<(u8, u8), (u8, u8)>,
    /// Last LSB seen for each 14-bit control, keyed by its MSB message.
    last_lsb: HashMap<(u8, u8), u8>,
    /// Vinyl mode per deck: on, touching the platter scratches; off, it only nudges.
    vinyl: [bool; 2],
}

impl MidiMapper {
    pub fn new(profile: &Profile) -> Result<Self, String> {
        let compiled = profile.compile()?;
        let mut lsb_of = HashMap::new();
        for c in compiled.iter().filter(|c| c.hires && c.data1 < 0x20) {
            lsb_of.insert((c.status, c.data1 + 0x20), (c.status, c.data1));
        }
        Ok(Self { bindings: index(&compiled), lsb_of, last_lsb: HashMap::new(), vinyl: [true; 2] })
    }

    /// True when `msg` is bound to something (used by the MIDI monitor).
    pub fn describe(&self, msg: &[u8]) -> Option<String> {
        if msg.len() < 2 {
            return None;
        }
        let key = (fold_status(msg[0]), msg[1]);
        if self.lsb_of.contains_key(&key) {
            return Some("fine (LSB)".into());
        }
        let c = self.bindings.get(&key)?;
        let deck = if c.func.is_global() { String::new() } else { format!(" {}", if c.deck == 0 { "A" } else { "B" }) };
        Some(format!("{}{deck}", c.func.name()))
    }

    pub fn process(&mut self, msg: &[u8]) -> Option<AppCommand> {
        if msg.len() < 2 {
            return None;
        }
        let key = (fold_status(msg[0]), msg[1]);
        let mut value = msg.get(2).copied().unwrap_or(0);
        // A note-off (8n) is a release whatever its velocity.
        if msg[0] & 0xF0 == 0x80 {
            value = 0;
        }
        let is_cc = key.0 & 0xF0 == 0xB0;

        if let Some(&msb_key) = self.lsb_of.get(&key) {
            self.last_lsb.insert(msb_key, value);
            return None;
        }
        let c = *self.bindings.get(&key)?;
        let d = c.deck;
        let pressed = !is_cc && value > 0;
        let action = |a: DeckAction| Some(AppCommand::Action(d, a));
        let press = |a: DeckAction| if pressed { action(a) } else { None };

        // Absolute controls arrive 7-bit, or 14-bit when the profile says so.
        let unit = || {
            if c.hires {
                let lsb = self.last_lsb.get(&key).copied().unwrap_or(0);
                cc_to_01((((value as u32) << 7) | lsb as u32) as f32 / 16383.0)
            } else {
                cc_to_01(value as f32 / 127.0)
            }
        };

        match c.func {
            Func::PlayPause if pressed => Some(AppCommand::DeckPlayPause(d)),
            Func::Load if pressed => Some(AppCommand::DeckLoad(d)),
            Func::Sync if pressed => Some(AppCommand::DeckSync(d)),
            Func::KeyLock if pressed => Some(AppCommand::DeckKeyLock(d)),
            Func::AutoGain if pressed => Some(AppCommand::DeckAutoGain(d)),
            Func::Cue if !is_cc => action(if value > 0 { DeckAction::CueDown } else { DeckAction::CueUp }),
            Func::ToStart => press(DeckAction::ToStart),
            Func::LoopIn if !is_cc => action(if value > 0 { DeckAction::LoopInDown } else { DeckAction::LoopInUp }),
            Func::LoopOut | Func::LoopToggle => press(DeckAction::LoopOut),
            Func::LoopHalf => press(DeckAction::LoopHalf),
            Func::LoopDouble => press(DeckAction::LoopDouble),
            Func::LoopBeats => press(DeckAction::LoopBeats(c.arg as f32)),
            Func::Quantize => press(DeckAction::ToggleQuantize),
            Func::Pad if !is_cc => {
                let n = c.arg as u8;
                action(if value > 0 { DeckAction::PadDown(n) } else { DeckAction::PadUp(n) })
            }
            Func::PadDelete => press(DeckAction::PadDelete(c.arg as u8)),
            Func::Stem if pressed => Some(AppCommand::DeckStemToggle(d, c.arg as usize)),
            // Jog top-surface touch: held = vinyl scratch mode.
            Func::JogTouch if !is_cc => {
                if self.vinyl[d.min(1)] {
                    Some(AppCommand::DeckSetBrake(d, value > 0))
                } else {
                    None
                }
            }
            Func::HeadphoneCue if pressed => Some(AppCommand::ToggleCue(d)),
            Func::MasterMonitor if pressed => Some(AppCommand::ToggleMasterMonitor),
            Func::Vinyl if pressed => {
                let on = !self.vinyl[d.min(1)];
                self.vinyl[d.min(1)] = on;
                // Switching off while the platter is held must let go of the brake.
                if on {
                    None
                } else {
                    Some(AppCommand::DeckSetBrake(d, false))
                }
            }
            Func::Volume if is_cc => Some(AppCommand::DeckSetVolume(d, unit() * 1.5)),
            Func::Gain if is_cc => Some(AppCommand::DeckSetGain(d, unit() * 2.0)),
            Func::EqHigh if is_cc => Some(AppCommand::DeckEqHigh(d, unit() * 2.0)),
            Func::EqMid if is_cc => Some(AppCommand::DeckEqMid(d, unit() * 2.0)),
            Func::EqLow if is_cc => Some(AppCommand::DeckEqLow(d, unit() * 2.0)),
            // Knob centre = bypass (0.5), left = deep low-pass, right = deep high-pass.
            Func::Filter if is_cc => Some(AppCommand::DeckSetFilter(d, unit())),
            // Tempo fader: 0 = bottom (-8%), centre = 0%, top = +8%.
            Func::Tempo if is_cc => {
                let ratio = 1.0 + (unit() - 0.5) * 2.0 * 0.08;
                Some(AppCommand::DeckSetTempo(d, ratio.clamp(0.5, 2.0)))
            }
            // Jog wheels send relative steps. Magnitudes are calibrated so a 33 RPM platter
            // is about 1x playback: the outer ring nudges gently, the platter scratches.
            Func::JogBend if is_cc => Some(AppCommand::DeckJogBend(d, relative(value) as f32 * 0.015)),
            Func::JogScratch if is_cc => {
                let gain = if self.vinyl[d.min(1)] { 0.60 } else { 0.015 }; // vinyl off: nudge like the ring
                Some(AppCommand::DeckJogBend(d, relative(value) as f32 * gain))
            }
            Func::JogSearch if is_cc => Some(AppCommand::DeckSearch(d, relative(value))),
            Func::Crossfader if is_cc => Some(AppCommand::SetCrossfader(unit())),
            Func::Browse if is_cc => {
                let step = relative(value) * c.arg.max(1);
                if step == 0 {
                    None
                } else {
                    Some(AppCommand::Browse(step))
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_profile::load_profiles;

    fn mapper(id: &str) -> MidiMapper {
        let p = load_profiles().into_iter().find(|p| p.id == id).unwrap_or_else(|| panic!("no profile {id}"));
        MidiMapper::new(&p.profile).unwrap()
    }

    fn act(m: &mut MidiMapper, msg: [u8; 3]) -> Option<(usize, DeckAction)> {
        match m.process(&msg) {
            Some(AppCommand::Action(d, a)) => Some((d, a)),
            _ => None,
        }
    }

    #[test]
    fn cue_button_reports_press_and_release() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert_eq!(act(&mut m, [0x91, 0x06, 0x7F]), Some((0, DeckAction::CueDown)));
        assert_eq!(act(&mut m, [0x91, 0x06, 0x00]), Some((0, DeckAction::CueUp)));
        assert_eq!(act(&mut m, [0x81, 0x06, 0x40]), Some((0, DeckAction::CueUp)), "note-off is a release");
        assert_eq!(act(&mut m, [0x92, 0x06, 0x7F]), Some((1, DeckAction::CueDown)));
    }

    #[test]
    fn original_200_has_loop_in_out_with_halve_double_on_shift() {
        let mut m = mapper("hercules-dj-inpulse-200");
        assert_eq!(act(&mut m, [0x91, 0x09, 0x7F]), Some((0, DeckAction::LoopInDown)));
        assert_eq!(act(&mut m, [0x91, 0x09, 0x00]), Some((0, DeckAction::LoopInUp)));
        assert_eq!(act(&mut m, [0x91, 0x0A, 0x7F]), Some((0, DeckAction::LoopOut)));
        assert_eq!(act(&mut m, [0x94, 0x09, 0x7F]), Some((0, DeckAction::LoopHalf)));
        assert_eq!(act(&mut m, [0x95, 0x0A, 0x7F]), Some((1, DeckAction::LoopDouble)));
        assert_eq!(act(&mut m, [0x94, 0x06, 0x7F]), Some((0, DeckAction::ToStart)));
        assert_eq!(act(&mut m, [0x94, 0x0A, 0x00]), None, "release of a press-only button is ignored");
    }

    #[test]
    fn mk2_puts_halve_double_on_the_plain_loop_buttons() {
        // The Mk2 map from Hercules swaps the layers: ½X / 2X unshifted, IN / OUT on SHIFT.
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert_eq!(act(&mut m, [0x91, 0x09, 0x7F]), Some((0, DeckAction::LoopHalf)));
        assert_eq!(act(&mut m, [0x92, 0x0A, 0x7F]), Some((1, DeckAction::LoopDouble)));
        assert_eq!(act(&mut m, [0x94, 0x09, 0x7F]), Some((0, DeckAction::LoopInDown)));
        assert_eq!(act(&mut m, [0x95, 0x0A, 0x7F]), Some((1, DeckAction::LoopOut)));
        // The ½X button also sends 0B on the real unit (captured on hardware), so 0B must do nothing.
        assert_eq!(act(&mut m, [0x91, 0x0B, 0x7F]), None);
    }

    #[test]
    fn vinyl_off_stops_the_platter_scratching() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        let touch = |m: &mut MidiMapper| m.process(&[0x91, 0x08, 0x7F]);
        assert!(matches!(touch(&mut m), Some(AppCommand::DeckSetBrake(0, true))));
        // SHIFT + loop is the VINYL button (captured on hardware): off releases the brake.
        assert!(matches!(m.process(&[0x94, 0x03, 0x7F]), Some(AppCommand::DeckSetBrake(0, false))));
        assert!(touch(&mut m).is_none());
        // The other deck is unaffected.
        assert!(matches!(m.process(&[0x92, 0x08, 0x7F]), Some(AppCommand::DeckSetBrake(1, true))));
        // Pressing again turns it back on.
        m.process(&[0x94, 0x03, 0x7F]);
        assert!(matches!(touch(&mut m), Some(AppCommand::DeckSetBrake(0, true))));
    }

    #[test]
    fn the_headphone_buttons_toggle_cue_per_deck() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert!(matches!(m.process(&[0x91, 0x0C, 0x7F]), Some(AppCommand::ToggleCue(0))));
        assert!(m.process(&[0x91, 0x0C, 0x00]).is_none(), "release does nothing");
        assert!(matches!(m.process(&[0x92, 0x0C, 0x7F]), Some(AppCommand::ToggleCue(1))));
    }

    #[test]
    fn shift_jog_searches_and_the_hardware_master_button_is_left_alone() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        // Captured on hardware: SHIFT + jog is a relative CC on channel 5 (0xB4); MASTER is note 0x02 on channel 1.
        assert!(matches!(m.process(&[0xB4, 0x09, 0x01]), Some(AppCommand::DeckSearch(0, 1))));
        assert!(matches!(m.process(&[0xB4, 0x09, 0x7F]), Some(AppCommand::DeckSearch(0, -1))));
        // The Mk2's MASTER button blends the main output into the headphones in hardware (captured as 90 02),
        // so Dubplate must leave it alone or the main mix would be added twice.
        assert!(m.process(&[0x90, 0x02, 0x7F]).is_none());
    }

    #[test]
    fn play_pause_and_cue_buttons_still_map() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert!(matches!(m.process(&[0x91, 0x07, 0x7F]), Some(AppCommand::DeckPlayPause(0))));
        assert!(matches!(m.process(&[0x92, 0x07, 0x7F]), Some(AppCommand::DeckPlayPause(1))));
        assert!(matches!(m.process(&[0x91, 0x06, 0x7F]), Some(AppCommand::Action(0, DeckAction::CueDown))));
    }

    #[test]
    fn pads_set_trigger_and_delete() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert_eq!(act(&mut m, [0x96, 0x00, 0x7F]), Some((0, DeckAction::PadDown(0))));
        assert_eq!(act(&mut m, [0x96, 0x03, 0x00]), Some((0, DeckAction::PadUp(3))));
        assert_eq!(act(&mut m, [0x97, 0x0B, 0x7F]), Some((1, DeckAction::PadDelete(3))));
    }

    #[test]
    fn mk2_stem_pads_follow_the_hercules_order() {
        // Hercules' own Mk2 map: pad 1 vocals, 2 melody, 3 bass, 4 drums.
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        let stem = |m: &mut MidiMapper, n: u8| match m.process(&[0x96, 0x10 + n, 0x7F]) {
            Some(AppCommand::DeckStemToggle(0, s)) => Some(s),
            _ => None,
        };
        assert_eq!(stem(&mut m, 0), Some(3), "vocals");
        assert_eq!(stem(&mut m, 1), Some(2), "other / melody");
        assert_eq!(stem(&mut m, 2), Some(1), "bass");
        assert_eq!(stem(&mut m, 3), Some(0), "drums");
    }

    #[test]
    fn sync_and_play_are_bound() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert!(matches!(m.process(&[0x92, 0x05, 0x7F]), Some(AppCommand::DeckSync(1))));
        assert!(m.process(&[0x92, 0x05, 0x00]).is_none());
        assert!(matches!(m.process(&[0x91, 0x07, 0x7F]), Some(AppCommand::DeckPlayPause(0))));
    }

    #[test]
    fn fourteen_bit_faders_combine_msb_and_lsb() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        let vol = |c: Option<AppCommand>| match c {
            Some(AppCommand::DeckSetVolume(0, v)) => v,
            o => panic!("{o:?}"),
        };
        assert!(m.process(&[0xB1, 0x20, 0x7F]).is_none(), "the LSB alone emits nothing");
        let full = vol(m.process(&[0xB1, 0x00, 0x7F]));
        assert!((full - 1.5).abs() < 1e-3, "full travel = unity+headroom, got {full}");
        m.process(&[0xB1, 0x20, 0x00]);
        let a = vol(m.process(&[0xB1, 0x00, 0x40]));
        m.process(&[0xB1, 0x20, 0x7F]);
        let b = vol(m.process(&[0xB1, 0x00, 0x40]));
        assert!(b > a, "the LSB refines the value ({a} < {b})");
    }

    #[test]
    fn crossfader_spans_zero_to_one() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        let cf = |m: &mut MidiMapper, v: u8| match m.process(&[0xB0, 0x00, v]) {
            Some(AppCommand::SetCrossfader(x)) => x,
            o => panic!("{o:?}"),
        };
        m.process(&[0xB0, 0x20, 0x00]);
        assert!(cf(&mut m, 0) < 1e-3);
        m.process(&[0xB0, 0x20, 0x7F]);
        assert!((cf(&mut m, 0x7F) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn jog_wheels_are_signed_relative() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        let bend = |m: &mut MidiMapper, v: u8| match m.process(&[0xB1, 0x09, v]) {
            Some(AppCommand::DeckJogBend(0, x)) => x,
            o => panic!("{o:?}"),
        };
        assert!(bend(&mut m, 0x01) > 0.0);
        assert!(bend(&mut m, 0x7F) < 0.0);
        assert!(bend(&mut m, 0x7E) < bend(&mut m, 0x7F), "a faster counter-clockwise spin bends more");
    }

    #[test]
    fn browser_encoder_steps_the_selection() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert!(matches!(m.process(&[0xB0, 0x01, 0x01]), Some(AppCommand::Browse(1))));
        assert!(matches!(m.process(&[0xB0, 0x01, 0x7F]), Some(AppCommand::Browse(-1))));
    }

    #[test]
    fn unbound_messages_are_ignored() {
        let mut m = mapper("djcontrol-inpulse-200-mk2");
        assert!(m.process(&[0xF8]).is_none());
        assert!(m.process(&[0x91, 0x7E, 0x7F]).is_none());
        assert!(m.describe(&[0x91, 0x06, 0x7F]).unwrap().starts_with("cue A"));
        assert!(m.describe(&[0x91, 0x7E, 0x7F]).is_none());
    }
}
