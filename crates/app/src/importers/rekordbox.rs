//! Converts a rekordbox `.midi.csv` mapping into a Dubplate [`Profile`].
//!
//! ```text
//! @file,1,DDJ-400
//! #name,function,type,input,deck1,deck2,deck3,deck4,output,deck1,...,option,comment
//! PlayPause,PlayPause,Button,900B,0,1,,,900B,0,1,,,Fast,Play/Pause
//! CrossFader,CrossFader,KnobSliderHiRes,B61F,,,,,,,,,,Fast,Crossfader
//! ```
//!
//! * column 0 is the rekordbox *function* (`PAD1_HotCue`), column 1 the hardware control it
//!   comes from (`PAD1_PadMode1+Shift`);
//! * `input` is status + data byte in hex; the `deckN` columns add a MIDI channel offset
//!   (`900B` with `0,1` = `90 0B` for deck 1 and `91 0B` for deck 2). When `input` is empty the
//!   `deckN` columns hold whole messages instead;
//! * `KnobSliderHiRes` is a 14-bit control (LSB on the CC 0x20 above).
//!
//! Shifted controls are separate rows with their own messages, so they import like any other row.

use super::{hex_byte, port_matches, Builder, Report};
use crate::midi_profile::{DeviceInfo, Profile};

/// Splits one CSV line, honouring double quotes.
fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// `PAD3_HotCue+Shift` -> `PADn_HotCue+Shift`, `CFXParameterCH2` -> `CFXParameterCHn`.
fn normalise(function: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = function.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        out.push(chars[i]);
        let tail: String = out.chars().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
        if (tail == "PAD" || out.ends_with("CH")) && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            out.push('n');
            i += 1;
        }
        i += 1;
    }
    out
}

struct Mapped {
    func: &'static str,
    arg: Option<i32>,
    /// Deck implied by the function name (`CFXParameterCH2`), for rows without deck columns.
    deck: Option<u8>,
    global: bool,
    approx: Option<&'static str>,
}

fn m(func: &'static str) -> Result<Mapped, String> {
    Ok(Mapped { func, arg: None, deck: None, global: false, approx: None })
}

fn translate(function: &str) -> Result<Mapped, String> {
    // Hot-cue pads: PAD3_HotCue = set / trigger, PAD3_HotCue+Shift = delete.
    if let Some(rest) = function.strip_prefix("PAD") {
        let n: Option<i32> = rest.chars().next().and_then(|c| c.to_digit(10)).map(|d| d as i32);
        if let (Some(n), Some(tail)) = (n, rest.get(1..)) {
            return match tail {
                "_HotCue" => Ok(Mapped { arg: Some(n - 1), ..m("pad")? }),
                "_HotCue+Shift" => Ok(Mapped { arg: Some(n - 1), ..m("pad_delete")? }),
                other => Err(format!("pad mode `{}` has no Dubplate equivalent", other.trim_start_matches('_'))),
            };
        }
    }
    if let Some(ch) = function.strip_prefix("CFXParameterCH") {
        return match ch.parse::<u8>() {
            Ok(n @ 1..=2) => Ok(Mapped { deck: Some(n - 1), ..m("filter")? }),
            Ok(n) => Err(format!("deck {n} (Dubplate has two decks)")),
            Err(_) => Err(format!("no Dubplate equivalent for `{function}`")),
        };
    }
    match function {
        "PlayPause" => m("play_pause"),
        "Cue" => m("cue"),
        "JumpToTrackStart" => m("to_start"),
        "Sync" => m("sync"),
        "Load" => m("load"),
        "LoopIn" => m("loop_in"),
        "LoopOut" => m("loop_out"),
        "ReloopExit" => m("loop_toggle"),
        "LoopHalf" => m("loop_half"),
        "LoopDouble" => m("loop_double"),
        "4BeatLoopLongPress" => Ok(Mapped { arg: Some(4), ..m("loop_beats")? }),
        "Quantize" => m("quantize"),
        "TempoSlider" => m("tempo"),
        "ChannelFader" => m("volume"),
        "Gain" => m("gain"),
        "EQHigh" => m("eq_high"),
        "EQMid" => m("eq_mid"),
        "EQLow" => m("eq_low"),
        "JogScratch" => m("jog_scratch"),
        "JogPitchBend" | "WheelPitchBend" => m("jog_bend"),
        "JogTouch" => m("jog_touch"),
        "CrossFader" => Ok(Mapped { global: true, ..m("crossfader")? }),
        "Browse" => Ok(Mapped { global: true, ..m("browse")? }),
        other => Err(format!("no Dubplate equivalent for `{other}`")),
    }
}

/// Channel offset of a `deckN` cell ("0", "1", "7"…), blank = the deck is not wired.
fn offset(cell: &str) -> Option<u8> {
    let c = cell.trim();
    if c.is_empty() {
        None
    } else {
        c.parse::<u8>().ok()
    }
}

pub fn import(text: &str, file_stem: &str) -> Result<(Profile, Report), String> {
    let mut lines = text.trim_start_matches('\u{feff}').lines().map(|l| l.trim_end_matches('\r'));
    let first = lines.next().unwrap_or("");
    if !first.starts_with("@file") {
        return Err("not a rekordbox MIDI mapping (no @file header)".into());
    }
    let name = split_csv(first)
        .get(2)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| file_stem.trim_end_matches(".midi").to_owned());
    // The header often repeats the file name; prefer the plain file stem when it is the same thing.
    let name = if name.ends_with(".midi.csv") || name.is_empty() {
        file_stem.trim_end_matches(".midi").to_owned()
    } else {
        name
    };

    let priority = super::default_priority(&name);
    let device = DeviceInfo {
        port_match: port_matches(&name),
        name,
        priority,
        source: "rekordbox MIDI mapping".into(),
        notes: String::new(),
    };

    let mut b = Builder::new();
    for line in lines {
        let f = split_csv(line);
        if f.len() < 9 || f[0].trim().is_empty() || f[0].starts_with('#') {
            continue;
        }
        let (function, control, kind, input) = (f[0].trim(), f[1].trim(), f[2].trim(), f[3].trim());
        if function == "Shift"
            || kind == "Indicator"
            || kind == "IndicatorHiRes"
            || kind == "Value"
            || kind == "JogIndicator"
            || kind == "JogIndicatorHiRes"
            || kind == "RawValue"
            || kind == "Parameter"
        {
            continue; // the Shift key itself and output-only rows
        }
        let label = if control.is_empty() { function.to_owned() } else { format!("{function} ({control})") };
        let seen_name = normalise(function);
        let deck_cells = &f[4..8];
        let has_input = !input.is_empty();
        let any_deck = deck_cells.iter().any(|c| !c.trim().is_empty());
        if !has_input && !any_deck {
            continue;
        } // output-only

        let mapped = match translate(function) {
            Ok(x) => x,
            Err(why) => {
                b.skip(&label, &seen_name, why);
                continue;
            }
        };
        let hires = kind == "KnobSliderHiRes";

        // Messages to bind: (status, data1, deck)
        let mut msgs: Vec<(u8, u8, Option<u8>)> = Vec::new();
        let parse_msg = |s: &str| -> Option<(u8, u8)> {
            let s = s.trim();
            if s.len() != 4 {
                return None;
            }
            Some((hex_byte(&s[..2])?, hex_byte(&s[2..])?))
        };
        if has_input {
            let Some((status, data1)) = parse_msg(input) else {
                b.skip(&label, &seen_name, format!("unreadable message `{input}`"));
                continue;
            };
            if mapped.global || !any_deck {
                // One message for the whole mixer (crossfader, browse) or a deck named in the function.
                msgs.push((status, data1, if mapped.global { None } else { mapped.deck }));
            } else {
                for (i, cell) in deck_cells.iter().take(2).enumerate() {
                    if let Some(off) = offset(cell) {
                        msgs.push((status.wrapping_add(off), data1, Some(i as u8)));
                    }
                }
            }
        } else {
            // No `input`: the deck cells are whole messages.
            for (i, cell) in deck_cells.iter().take(2).enumerate() {
                if cell.trim().is_empty() {
                    continue;
                }
                match parse_msg(cell) {
                    Some((s, d)) => msgs.push((s, d, Some(i as u8))),
                    None => b.skip(&label, &seen_name, format!("unreadable message `{}`", cell.trim())),
                }
            }
        }
        if msgs.is_empty() {
            b.skip(&label, &seen_name, "only wired for decks 3 / 4 (Dubplate has two decks)");
            continue;
        }
        for (status, data1, deck) in msgs {
            if deck.is_none() && !mapped.global {
                b.skip(&label, &seen_name, "no deck for this control");
                continue;
            }
            b.add(&label, &seen_name, status, data1, mapped.func, deck, mapped.arg, hires, mapped.approx);
        }
    }
    b.finish(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_profile::Bind;

    const SAMPLE: &str = "@file,1,DDJ-TEST\r\n\
#name,function,type,input,deck1,deck2,deck3,deck4,output,deck1,deck2,deck3,deck4,option,comment\r\n\
,,,,,,,,,,,,,,\r\n\
# Deck,,,,,,,,,,,,,,\r\n\
Shift,Shift,Button,903F,0,1,,,,,,,,RO,Shift\r\n\
PlayPause,PlayPause,Button,900B,0,1,,,900B,0,1,,,Fast,Play/Pause\r\n\
JumpToTrackStart,Cue+Shift,Button,9048,0,1,,,9048,0,1,,,Fast,Jump\r\n\
Load,Load,Button,,9646,9647,,,,,,,,,Load\r\n\
CrossFader,CrossFader,KnobSliderHiRes,B61F,,,,,,,,,,Fast,Crossfader\r\n\
ChannelFader,ChannelFader,KnobSliderHiRes,B013,0,1,,,,,,,,,CH Fader\r\n\
CFXParameterCH2,CFXParameterCH2,KnobSliderHiRes,B618,,,,,,,,,,Fast,CH2 CFX\r\n\
Browse,Browse,Rotary,B640,,,,,,,,,,,Browse\r\n\
ChannelLevel,ChannelLevel,Indicator,,,,,,B002,0,1,,,RO,Level\r\n\
PAD1_HotCue,PAD1_PadMode1,Pad,9000,7,9,,,9000,7,9,,,Fast,HOT CUE1\r\n\
PAD3_HotCue+Shift,PAD3_PadMode1+Shift,Pad,9002,8,10,,,9002,8,10,,,Fast,Delete\r\n\
PAD1_BeatJump,PAD1_PadMode3,Pad,9020,7,9,,,9020,7,9,,,Fast,jump\r\n\
FX1-1On,FX1-1On,Button,9447,,,,,9447,,,,,,FX\r\n\
Cue,Cue,Button,9054,,,2,3,,,,,,,only decks 3 and 4\r\n";

    fn find<'a>(p: &'a Profile, msg: &str) -> &'a Bind {
        p.binds.iter().find(|b| b.msg == msg).unwrap_or_else(|| panic!("no bind for {msg}"))
    }

    #[test]
    fn deck_columns_are_channel_offsets() {
        let (p, _) = import(SAMPLE, "DDJ-TEST.midi").unwrap();
        assert_eq!(p.device.name, "DDJ-TEST");
        assert_eq!((find(&p, "90 0B").func.as_str(), find(&p, "90 0B").deck), ("play_pause", Some(0)));
        assert_eq!(find(&p, "91 0B").deck, Some(1));
        assert_eq!((find(&p, "90 48").func.as_str(), find(&p, "91 48").deck), ("to_start", Some(1)));
    }

    #[test]
    fn blank_input_means_whole_messages_per_deck() {
        let (p, _) = import(SAMPLE, "x").unwrap();
        assert_eq!((find(&p, "96 46").func.as_str(), find(&p, "96 46").deck), ("load", Some(0)));
        assert_eq!(find(&p, "96 47").deck, Some(1));
    }

    #[test]
    fn global_and_hires_controls() {
        let (p, _) = import(SAMPLE, "x").unwrap();
        let cf = find(&p, "B6 1F");
        assert_eq!((cf.func.as_str(), cf.deck, cf.hires), ("crossfader", None, true));
        assert_eq!(find(&p, "B0 13").func, "volume");
        assert!(find(&p, "B1 13").hires);
        assert_eq!(find(&p, "B6 40").func, "browse");
        assert!(!find(&p, "B6 40").hires, "a rotary encoder is not 14-bit");
    }

    #[test]
    fn channel_in_the_function_name_picks_the_deck() {
        let (p, _) = import(SAMPLE, "x").unwrap();
        let f = find(&p, "B6 18");
        assert_eq!((f.func.as_str(), f.deck), ("filter", Some(1)), "CFXParameterCH2 is deck B's filter");
    }

    #[test]
    fn hot_cue_pads_use_the_pad_channel_and_index() {
        let (p, _) = import(SAMPLE, "x").unwrap();
        let pad = find(&p, "97 00");
        assert_eq!((pad.func.as_str(), pad.arg, pad.deck), ("pad", Some(0), Some(0)));
        assert_eq!(find(&p, "99 00").deck, Some(1));
        let del = find(&p, "98 02");
        assert_eq!((del.func.as_str(), del.arg), ("pad_delete", Some(2)), "PAD3 +Shift deletes pad index 2");
    }

    #[test]
    fn skips_are_explained_and_output_rows_ignored() {
        let (p, r) = import(SAMPLE, "x").unwrap();
        let why =
            |k: &str| r.skipped.iter().find(|(c, _)| c.starts_with(k)).map(|(_, w)| w.clone()).unwrap_or_default();
        assert!(why("PAD1_BeatJump").contains("BeatJump"));
        assert!(why("FX1-1On").contains("no Dubplate equivalent"));
        assert!(why("Cue").contains("decks 3"));
        assert!(r.skipped.iter().all(|(c, _)| !c.starts_with("ChannelLevel") && !c.starts_with("Shift")));
        assert_eq!(r.converted, p.binds.len());
    }

    #[test]
    fn survey_names_are_normalised() {
        assert_eq!(normalise("PAD3_HotCue+Shift"), "PADn_HotCue+Shift");
        assert_eq!(normalise("CFXParameterCH2"), "CFXParameterCHn");
        assert_eq!(normalise("FX1-1On"), "FX1-1On");
        assert_eq!(normalise("PlayPause"), "PlayPause");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_header() {
        let with_bom = format!("\u{feff}{SAMPLE}");
        assert!(import(&with_bom, "x").is_ok());
    }

    #[test]
    fn csv_splitter_handles_quotes() {
        assert_eq!(split_csv(r#"a,"b,c",d"#), vec!["a", "b,c", "d"]);
    }

    #[test]
    fn rejects_other_files() {
        assert!(import("a,b,c", "x").is_err());
    }
}
