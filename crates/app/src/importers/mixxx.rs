//! Converts a Mixxx `.midi.xml` controller preset into a Dubplate [`Profile`].
//!
//! ```xml
//! <control>
//!   <group>[Channel1]</group><key>play</key>
//!   <status>0x91</status><midino>0x07</midino>
//!   <options><normal/></options>
//! </control>
//! ```
//!
//! Mixxx presets often hand the interesting controls (jog wheels, shift layers, pads, LEDs) to a
//! JavaScript file. A row with `<script-binding/>` only names the JS function, so it is listed as
//! skipped instead of guessed. Plain rows are converted through the table in [`translate`].

use super::{hex_byte, port_matches, Builder, Report};
use crate::midi_profile::{DeviceInfo, Profile};

struct Row {
    group: String,
    key: String,
    status: u8,
    midino: u8,
    script: bool,
    hires: bool,
    /// The low half of a 14-bit pair; the MSB row carries the function.
    lsb: bool,
}

/// Deck 0 / 1 for `[Channel1]`, `[EqualizerRack1_[Channel2]_Effect1]` and friends.
fn channel_of(group: &str) -> Option<usize> {
    let i = group.find("[Channel")?;
    let digits: String = group[i + "[Channel".len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// `[Channel2]` -> `[ChannelN]`, so the survey counts one function, not one per deck.
fn group_kind(group: &str) -> String {
    match group.find("[Channel") {
        Some(i) => {
            let end = group[i..].find(']').map_or(group.len(), |e| i + e);
            format!("{}[ChannelN{}", &group[..i], &group[end..])
        }
        None => {
            // [Sampler12] -> [SamplerN]
            let t: String = group.chars().map(|c| if c.is_ascii_digit() { 'N' } else { c }).collect();
            let mut out = String::new();
            for c in t.chars() {
                if !(c == 'N' && out.ends_with('N')) {
                    out.push(c);
                }
            }
            out
        }
    }
}

/// `hotcue_3_activate` -> (`hotcue_N_activate`, Some(3)); `beatloop_0.5_toggle` -> (…X…, Some(0.5)).
fn numbered(key: &str) -> (String, Option<f64>) {
    let mut n = None;
    let parts: Vec<String> = key
        .split('_')
        .map(|p| match p.parse::<f64>() {
            Ok(v) if n.is_none() => {
                n = Some(v);
                "N".to_owned()
            }
            _ => p.to_owned(),
        })
        .collect();
    (parts.join("_"), n)
}

struct Mapped {
    func: &'static str,
    arg: Option<i32>,
    approx: Option<&'static str>,
    global: bool,
}

fn m(func: &'static str) -> Result<Mapped, String> {
    Ok(Mapped { func, arg: None, approx: None, global: false })
}

/// (group kind, normalised key, number from the key) -> Dubplate function.
fn translate(group: &str, key: &str, n: Option<f64>) -> Result<Mapped, String> {
    let nothing = || Err(format!("no Dubplate equivalent for `{group} {key}`"));
    if group.starts_with("[EqualizerRack") {
        return match key {
            "parameter3" => m("eq_high"),
            "parameter2" => m("eq_mid"),
            "parameter1" => m("eq_low"),
            _ => nothing(),
        };
    }
    if group.starts_with("[QuickEffectRack") {
        return if key == "super1" { m("filter") } else { nothing() };
    }
    match (group, key) {
        ("[Master]", "crossfader") => Ok(Mapped { global: true, ..m("crossfader")? }),
        ("[Library]", "MoveVertical") | ("[Library]", "ScrollVertical") => Ok(Mapped { global: true, ..m("browse")? }),
        ("[ChannelN]", k) => match k {
            "play" => m("play_pause"),
            "cue_default" | "cue_simple" => m("cue"),
            "cue_preview" => Ok(Mapped { approx: Some("`cue_preview` is mapped to CUE"), ..m("cue")? }),
            "start" => m("to_start"),
            "start_stop" => Ok(Mapped { approx: Some("`start_stop` is mapped to jump-to-start"), ..m("to_start")? }),
            "sync_enabled" => m("sync"),
            "beatsync" => Ok(Mapped {
                approx: Some("`beatsync` is a one-shot sync; Dubplate's SYNC stays on until released"),
                ..m("sync")?
            }),
            "filterLow" => m("eq_low"),
            "filterMid" => m("eq_mid"),
            "filterHigh" => m("eq_high"),
            "LoadSelectedTrack" => m("load"),
            "LoadSelectedTrackAndPlay" => Ok(Mapped { approx: Some("loads but does not start playing"), ..m("load")? }),
            "loop_in" => m("loop_in"),
            "loop_out" => m("loop_out"),
            "loop_halve" => m("loop_half"),
            "loop_double" => m("loop_double"),
            "reloop_toggle" => m("loop_toggle"),
            "reloop_andstop" => {
                Ok(Mapped { approx: Some("`reloop_andstop` is mapped to loop on/off"), ..m("loop_toggle")? })
            }
            "beatloop_N_toggle" | "beatloop_N_activate" => match n {
                Some(beats) if beats >= 0.25 && beats.fract() == 0.0 => {
                    Ok(Mapped { arg: Some(beats as i32), ..m("loop_beats")? })
                }
                Some(_) => Err("fractional loop length (Dubplate loops are whole beats from the controller)".into()),
                None => nothing(),
            },
            "hotcue_N_activate" => {
                n.map_or_else(nothing, |v| Ok(Mapped { arg: Some(v as i32 - 1), ..m("pad").unwrap() }))
            }
            "hotcue_N_gotoandplay" => n.map_or_else(nothing, |v| {
                Ok(Mapped {
                    arg: Some(v as i32 - 1),
                    approx: Some("`gotoandplay` is mapped to the normal hot-cue pad"),
                    ..m("pad").unwrap()
                })
            }),
            "hotcue_N_clear" => {
                n.map_or_else(nothing, |v| Ok(Mapped { arg: Some(v as i32 - 1), ..m("pad_delete").unwrap() }))
            }
            "quantize" => m("quantize"),
            "volume" => m("volume"),
            "pregain" => m("gain"),
            "rate" => m("tempo"),
            "jog" => m("jog_bend"),
            "scratch2" => m("jog_scratch"),
            "scratch2_enable" => m("jog_touch"),
            _ => nothing(),
        },
        _ => nothing(),
    }
}

fn rows(doc: &roxmltree::Document) -> Vec<Row> {
    let mut out = Vec::new();
    let Some(controls) = doc.descendants().find(|n| n.has_tag_name("controls")) else { return out };
    for c in controls.children().filter(|n| n.has_tag_name("control")) {
        let text = |tag: &str| c.children().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(str::trim);
        let (Some(group), Some(key), Some(status), Some(midino)) =
            (text("group"), text("key"), text("status"), text("midino"))
        else {
            continue;
        };
        let (Some(status), Some(midino)) = (hex_byte(status), hex_byte(midino)) else { continue };
        let options: Vec<String> = c
            .children()
            .filter(|n| n.has_tag_name("options"))
            .flat_map(|o| o.children().filter(|n| n.is_element()).map(|n| n.tag_name().name().to_ascii_lowercase()))
            .collect();
        out.push(Row {
            group: group.to_owned(),
            key: key.to_owned(),
            status,
            midino,
            script: options.iter().any(|o| o == "script-binding"),
            hires: options.iter().any(|o| o == "fourteen-bit-msb"),
            lsb: options.iter().any(|o| o == "fourteen-bit-lsb"),
        });
    }
    out
}

pub fn import(text: &str) -> Result<(Profile, Report), String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("not a valid Mixxx preset: {e}"))?;
    let root = doc.root_element();
    if !root.tag_name().name().starts_with("Mixxx") {
        return Err("not a Mixxx controller preset".into());
    }

    let info = |tag: &str| {
        root.descendants().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(|t| t.trim().to_owned())
    };
    let name = info("name").unwrap_or_else(|| "Imported controller".into());
    let priority = super::default_priority(&name);
    let device = DeviceInfo {
        port_match: port_matches(&name),
        priority,
        source: format!("Mixxx {}", root.attribute("mixxxVersion").unwrap_or("")).trim().to_owned(),
        notes: info("description").unwrap_or_default(),
        name,
    };

    let mut b = Builder::new();
    for row in rows(&doc) {
        let label = format!("{} {}", row.group, row.key);
        if row.lsb {
            continue;
        } // companion of a 14-bit MSB row, not a control of its own
        if row.script {
            b.skip(&label, "(script-driven)", format!("handled by the preset's JavaScript (`{}`)", row.key));
            continue;
        }
        let (kind, deck) = (group_kind(&row.group), channel_of(&row.group));
        let (key, n) = numbered(&row.key);
        let function = format!("{kind} {key}");
        if row.status & 0xF0 == 0xE0 || row.status & 0xF0 == 0xC0 || row.status >= 0xF0 {
            b.skip(&label, &function, "pitch-bend / program-change / system messages are not supported");
            continue;
        }
        let mapped = match translate(&kind, &key, n) {
            Ok(x) => x,
            Err(why) => {
                b.skip(&label, &function, why);
                continue;
            }
        };
        let deck = if mapped.global {
            None
        } else {
            match deck {
                Some(d @ 1..=2) => Some(d as u8 - 1),
                Some(d) => {
                    b.skip(&label, &function, format!("deck {d} (Dubplate has two decks)"));
                    continue;
                }
                None => {
                    b.skip(&label, &function, "no deck in the control group");
                    continue;
                }
            }
        };
        b.add(&label, &function, row.status, row.midino, mapped.func, deck, mapped.arg, row.hires, mapped.approx);
    }
    b.finish(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MixxxControllerPreset mixxxVersion="2.2" schemaVersion="1">
 <info><name>Test Pad</name><description>demo</description></info>
 <controller id="Test">
  <controls>
   <control><group>[Channel1]</group><key>play</key><status>0x91</status><midino>0x07</midino><options><normal/></options></control>
   <control><group>[Channel2]</group><key>hotcue_3_activate</key><status>0x92</status><midino>0x02</midino><options><normal/></options></control>
   <control><group>[Channel2]</group><key>hotcue_3_clear</key><status>0x92</status><midino>0x0A</midino><options><normal/></options></control>
   <control><group>[Channel1]</group><key>beatloop_4_toggle</key><status>0x91</status><midino>0x0B</midino><options><normal/></options></control>
   <control><group>[Channel1]</group><key>volume</key><status>0xB1</status><midino>0x00</midino><options><fourteen-bit-msb/></options></control>
   <control><group>[EqualizerRack1_[Channel1]_Effect1]</group><key>parameter3</key><status>0xB1</status><midino>0x04</midino><options><normal/></options></control>
   <control><group>[QuickEffectRack1_[Channel2]]</group><key>super1</key><status>0xB2</status><midino>0x01</midino><options><normal/></options></control>
   <control><group>[Master]</group><key>crossfader</key><status>0xB0</status><midino>0x00</midino><options><normal/></options></control>
   <control><group>[Library]</group><key>MoveVertical</key><status>0xB0</status><midino>0x01</midino><options><selectknob/></options></control>
   <control><group>[Channel1]</group><key>jog</key><status>0xB1</status><midino>0x09</midino><options><Script-Binding/></options></control>
   <control><group>[Channel3]</group><key>play</key><status>0x93</status><midino>0x07</midino><options><normal/></options></control>
   <control><group>[Channel1]</group><key>keylock</key><status>0x91</status><midino>0x20</midino><options><normal/></options></control>
   <control><group>[Channel1]</group><key>play</key><status>0x81</status><midino>0x07</midino><options><normal/></options></control>
   <control><group>[Channel1]</group><key>pitch</key><status>0xE1</status><midino>0x00</midino><options><normal/></options></control>
  </controls>
  <outputs><output><group>[Channel1]</group><key>play_indicator</key><status>0x91</status><midino>0x07</midino></output></outputs>
 </controller>
</MixxxControllerPreset>"#;

    fn find<'a>(p: &'a Profile, msg: &str) -> &'a crate::midi_profile::Bind {
        p.binds.iter().find(|b| b.msg == msg).unwrap_or_else(|| panic!("no bind for {msg}"))
    }

    #[test]
    fn converts_plain_rows() {
        let (p, r) = import(SAMPLE).unwrap();
        assert_eq!(p.device.name, "Test Pad");
        let play = find(&p, "91 07");
        assert_eq!((play.func.as_str(), play.deck), ("play_pause", Some(0)));
        let pad = find(&p, "92 02");
        assert_eq!((pad.func.as_str(), pad.arg, pad.deck), ("pad", Some(2), Some(1)), "hot cue 3 is pad index 2");
        assert_eq!(find(&p, "92 0A").func, "pad_delete");
        assert_eq!(find(&p, "91 0B").arg, Some(4));
        assert_eq!(find(&p, "B1 04").func, "eq_high");
        assert_eq!((find(&p, "B2 01").func.as_str(), find(&p, "B2 01").deck), ("filter", Some(1)));
        assert_eq!((find(&p, "B0 00").func.as_str(), find(&p, "B0 00").deck), ("crossfader", None));
        assert_eq!(find(&p, "B0 01").func, "browse");
        assert_eq!(r.converted, p.binds.len());
    }

    #[test]
    fn hires_comes_from_the_msb_option() {
        let (p, _) = import(SAMPLE).unwrap();
        assert!(find(&p, "B1 00").hires);
        assert!(!find(&p, "B1 04").hires);
    }

    #[test]
    fn script_rows_third_decks_and_unknowns_are_reported() {
        let (_, r) = import(SAMPLE).unwrap();
        let why = |k: &str| r.skipped.iter().find(|(c, _)| c.ends_with(k)).map(|(_, w)| w.clone()).unwrap_or_default();
        assert!(why("jog").contains("JavaScript"));
        assert!(r.skipped.iter().any(|(_, w)| w.contains("deck 3")));
        assert!(why("keylock").contains("no Dubplate equivalent"));
        assert!(r.skipped.iter().any(|(_, w)| w.contains("not supported")), "pitch-bend message");
        assert!(r.skipped.iter().any(|(_, w)| w.contains("already has")), "note-off duplicate of play");
    }

    #[test]
    fn the_lsb_row_of_a_14_bit_pair_is_not_a_second_control() {
        let xml = SAMPLE.replace("</controls>", r#"<control><group>[Channel1]</group><key>volume</key><status>0xB1</status><midino>0x20</midino><options><fourteen-bit-lsb/></options></control></controls>"#);
        let (p, r) = import(&xml).unwrap();
        assert!(p.binds.iter().all(|b| b.msg != "B1 20"));
        assert!(r.skipped.iter().all(|(c, _)| !c.contains("volume")), "not reported as skipped either");
    }

    #[test]
    fn outputs_are_ignored() {
        let (p, _) = import(SAMPLE).unwrap();
        assert_eq!(p.binds.iter().filter(|b| b.msg == "91 07").count(), 1);
    }

    #[test]
    fn names_are_normalised_for_the_survey() {
        assert_eq!(group_kind("[Channel2]"), "[ChannelN]");
        assert_eq!(group_kind("[EqualizerRack1_[Channel2]_Effect1]"), "[EqualizerRack1_[ChannelN]_Effect1]");
        assert_eq!(group_kind("[Sampler12]"), "[SamplerN]");
        assert_eq!(numbered("hotcue_12_activate"), ("hotcue_N_activate".into(), Some(12.0)));
        assert_eq!(numbered("beatloop_0.5_toggle").1, Some(0.5));
        assert_eq!(numbered("play"), ("play".into(), None));
        assert_eq!(channel_of("[EqualizerRack1_[Channel2]_Effect1]"), Some(2));
        assert_eq!(channel_of("[Master]"), None);
    }

    #[test]
    fn fractional_loops_are_not_forced_into_whole_beats() {
        let xml = SAMPLE.replace("beatloop_4_toggle", "beatloop_0.5_toggle");
        let (p, r) = import(&xml).unwrap();
        assert!(p.binds.iter().all(|b| b.msg != "91 0B"));
        assert!(r.skipped.iter().any(|(_, w)| w.contains("fractional")));
    }

    #[test]
    fn rejects_other_xml() {
        assert!(import("<midi/>").is_err());
        assert!(import("nonsense").is_err());
    }
}
