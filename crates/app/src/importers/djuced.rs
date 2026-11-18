//! Converts a DJUCED `.djm` controller map into a Dubplate [`Profile`].
//!
//! A `.djm` is XML in two halves, linked by control name:
//! * `<midi-device><control name="CUE_A"><input message="91 06 7f" type="toggle"/>…` — the hardware
//!   (the `<output>` rows are LED feedback, which Dubplate does not drive yet);
//! * `<midi-map><map name="CUE_A" action="chann=1 action=cue_stop value=&quot;auto&quot;"/>` — what
//!   DJUCED does with it.
//!
//! Only the DJUCED functions Dubplate has an equivalent for are converted. Everything else
//! (samplers, slicers, macro FX, energy browsing…) is listed in [`Report::skipped`] instead
//! of being guessed at.

use super::{port_matches, Builder, Report};
use crate::midi_profile::{parse_msg, DeviceInfo, Profile};

struct Action {
    chann: String,
    name: String,
    value: String,
    has_condition: bool,
}

/// `chann=1  action=cue_stop value="auto temporary" takeover="1"` -> fields.
fn parse_action(s: &str) -> Action {
    let mut a = Action { chann: String::new(), name: String::new(), value: String::new(), has_condition: false };
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim();
        let after = &rest[eq + 1..];
        let (val, next) = if let Some(q) = after.strip_prefix('"') {
            match q.find('"') {
                Some(end) => (&q[..end], &q[end + 1..]),
                None => (q, ""),
            }
        } else {
            match after.find(char::is_whitespace) {
                Some(end) => (&after[..end], &after[end..]),
                None => (after, ""),
            }
        };
        match key {
            "chann" => a.chann = val.to_owned(),
            "action" => a.name = val.to_owned(),
            "value" => a.value = val.to_owned(),
            k if k.starts_with("condition") && val != "0" => a.has_condition = true,
            _ => {}
        }
        rest = next.trim_start();
    }
    a
}

/// DJUCED action -> (Dubplate function, arg, approximate?)
fn translate(a: &Action, is_button: bool) -> Result<(&'static str, Option<i32>, Option<&'static str>), String> {
    let num = || a.value.split_whitespace().next().and_then(|v| v.parse::<f64>().ok());
    Ok(match a.name.as_str() {
        "play_pause" => ("play_pause", None, None),
        "cue_stop" => ("cue", None, None),
        "stop" => ("to_start", None, Some("DJUCED `stop` becomes jump-to-start (the Pioneer SHIFT+CUE)")),
        "sync" => ("sync", None, None),
        "hot_cue" => ("pad", num().map(|n| n as i32 - 1), None),
        "delete_cue" => ("pad_delete", num().map(|n| n as i32 - 1), None),
        "loop_half" => ("loop_half", None, None),
        "loop_double" => ("loop_double", None, None),
        "loop_in" => ("loop_in", None, None),
        "loop_out" => ("loop_out", None, None),
        "loop_in_move" => ("loop_in", None, Some("`loop_in_move` is mapped to plain loop IN")),
        "loop_out_move" => ("loop_out", None, Some("`loop_out_move` is mapped to plain loop OUT")),
        "loop" => ("loop_beats", num().map(|n| n as i32), None),
        "loop_toggle" => ("loop_toggle", None, None),
        "browser_load_deck" | "browser_load_deck_or_expand" => ("load", None, None),
        "browser_up_down" => ("browse", None, None),
        "browser_step_up_down" => ("browse", Some(8), None),
        "pitch_bend" => ("jog_bend", None, None),
        "scratch" if is_button => ("jog_touch", None, None),
        "scratch" => ("jog_scratch", None, None),
        "level" => ("volume", None, None),
        "gain" => ("gain", None, None),
        "high" => ("eq_high", None, None),
        "medium" => ("eq_mid", None, None),
        "bass" => ("eq_low", None, None),
        "set_pass_filter_amount" => ("filter", None, None),
        "pitch" => ("tempo", None, None),
        "crossfader" => ("crossfader", None, None),
        "toggle_quantization" => ("quantize", None, None),
        "stem_drum" => ("stem", Some(0), None),
        "stem_bass" => ("stem", Some(1), None),
        "stem_melody" | "stem_instru" => {
            ("stem", Some(2), Some("DJUCED melody / instrumental stem is Dubplate's `other` stem"))
        }
        "stem_voice" | "stem_acapp" => ("stem", Some(3), None),
        other => return Err(format!("no Dubplate equivalent for `{other}`")),
    })
}

/// Some maps write an attribute as `takeover=&quot;1&quot;` (entities instead of quotes) outside
/// any quoted value, which is not well-formed XML. Turns those into real quotes.
fn fix_entity_quotes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut in_tag, mut in_quote, mut fixing) = (false, false, false);
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if in_tag && !in_quote && rest.starts_with("=&quot;") {
            out.push_str("=\"");
            rest = &rest["=&quot;".len()..];
            fixing = true;
            continue;
        }
        if fixing && rest.starts_with("&quot;") {
            out.push('"');
            rest = &rest["&quot;".len()..];
            fixing = false;
            continue;
        }
        match c {
            '<' if !in_quote => in_tag = true,
            '>' if !in_quote && !fixing => in_tag = false,
            '"' if in_tag && !fixing => in_quote = !in_quote,
            _ => {}
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn strip_unparsed(text: &str) -> String {
    // The embedded controller picture is a huge base64 blob and the DOCTYPE has no DTD.
    let mut s = text.replace("<!DOCTYPE MidiML>", "");
    while let Some(a) = s.find("<picture>") {
        match s[a..].find("</picture>") {
            Some(b) => s.replace_range(a..a + b + "</picture>".len(), ""),
            None => break,
        }
    }
    fix_entity_quotes(&s)
}

pub fn import(text: &str) -> Result<(Profile, Report), String> {
    let cleaned = strip_unparsed(text);
    let doc = roxmltree::Document::parse(&cleaned).map_err(|e| format!("not a valid .djm file: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "midi" {
        return Err("not a DJUCED midi map (no <midi> element)".into());
    }

    let mut actions = std::collections::HashMap::new();
    for m in root.descendants().filter(|n| n.has_tag_name("map")) {
        if let (Some(name), Some(action)) = (m.attribute("name"), m.attribute("action")) {
            actions.insert(name.to_owned(), action.to_owned());
        }
    }

    let name = root.attribute("name").unwrap_or("Imported controller").trim().to_owned();
    let priority = super::default_priority(&name);
    let device = DeviceInfo {
        port_match: port_matches(&name),
        name,
        priority,
        source: format!("DJUCED .djm — {}", root.attribute("map-name").unwrap_or("").trim()),
        notes: root.attribute("description").unwrap_or("").trim().to_owned(),
    };

    let mut b = Builder::new();
    let Some(dev) = root.children().find(|n| n.has_tag_name("midi-device")) else {
        return Err("the map has no <midi-device> section".into());
    };
    for c in dev.children().filter(|n| n.has_tag_name("control")) {
        let name = c.attribute("name").unwrap_or("?").to_owned();
        let Some(input) = c.children().find(|n| n.has_tag_name("input")) else { continue }; // LED-only
        let Some(raw) = actions.get(&name) else {
            b.skip(&name, "(unmapped control)", "not mapped to any DJUCED function");
            continue;
        };
        let act = parse_action(raw);
        let function = act.name.clone();
        if act.has_condition {
            b.skip(&name, &function, "only active in a DJUCED condition (alternate mode)");
            continue;
        }
        let deck = match act.chann.as_str() {
            "1" => Some(0u8),
            "2" => Some(1u8),
            "default" => None,
            other => {
                b.skip(&name, &function, format!("DJUCED channel `{other}` (sampler / virtual deck)"));
                continue;
            }
        };

        let msg_raw = input.attribute("message").unwrap_or("");
        // Some maps glue the placeholder on (`b0 14{value}`) or drop the spaces (`9001{value}`);
        // the status and note bytes are always the first four hex digits.
        let digits: String = msg_raw.split('{').next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        if digits.len() < 4 {
            b.skip(&name, &function, format!("unreadable message `{msg_raw}`"));
            continue;
        }
        let Ok((status, data1)) = parse_msg(&format!("{} {}", &digits[..2], &digits[2..4])) else {
            b.skip(&name, &function, format!("unreadable message `{msg_raw}`"));
            continue;
        };

        let is_button = input.attribute("type") == Some("toggle");
        let (func, arg, approx) = match translate(&act, is_button) {
            Ok(t) => t,
            Err(why) => {
                b.skip(&name, &function, why);
                continue;
            }
        };
        let hires = c.children().any(|n| n.has_tag_name("lsbit-input"));
        // A global function has no deck; a per-deck one needs `chann` 1 or 2.
        let deck = if crate::midi_profile::Func::from_name(func).is_some_and(|f| f.is_global()) { None } else { deck };
        b.add(&name, &function, status, data1, func, deck, arg, hires, approx);
    }
    b.finish(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<!DOCTYPE MidiML>
<midi name="Test Pad" map-name="Default" description="demo" version="04">
 <picture>QUJD</picture>
 <midi-device>
  <control name="PLAY_A"><input message="91 07 7f" type="toggle"/><output message="91 07 7f" type="toggle"/></control>
  <control name="PLAY_B"><input message="92 07 7f" type="toggle"/></control>
  <control name="VOL_A"><input message="b1 00 47" type="interval"/><lsbit-input message="b1 20 47" type="toggle"/></control>
  <control name="XFADER"><input message="b0 00 33" type="interval"/></control>
  <control name="HC3_B"><input message="97 02 7f" type="toggle"/></control>
  <control name="SAMPLE1_A"><input message="96 30 7f" type="toggle"/></control>
  <control name="DUP"><input message="91 07 7f" type="toggle"/></control>
  <control name="LED_ONLY"><output message="91 09 7f" type="toggle"/></control>
  <control name="TOUCH_A"><input message="91 08 7f" type="toggle"/></control>
  <control name="SCRATCH_A"><input message="b1 0a 01" type="interval" incremental="yes"/></control>
  <control name="LOOP_IN_A"><input message="94 09 7f" type="toggle"/></control>
  <control name="STEM_A"><input message="96 10 {value}" type="toggle"/></control>
  <control name="ALT"><input message="b0 01 01" type="interval"/></control>
 </midi-device>
 <midi-map>
  <map name="PLAY_A" action="chann=1  action=play_pause value=&quot;0&quot;"/>
  <map name="PLAY_B" action="chann=2  action=play_pause value=&quot;0&quot;"/>
  <map name="VOL_A" action="chann=1  action=level value=&quot;auto&quot; takeover=&quot;1&quot;"/>
  <map name="XFADER" action="chann=default  action=crossfader value=&quot;auto&quot;"/>
  <map name="HC3_B" action="chann=2  action=hot_cue value=&quot;3&quot;"/>
  <map name="SAMPLE1_A" action="chann=3001  action=sample_load_play value=&quot;1 temporary&quot;"/>
  <map name="DUP" action="chann=1  action=cue_stop value=&quot;auto&quot;"/>
  <map name="TOUCH_A" action="chann=1  action=scratch value=&quot;1 temporary&quot;"/>
  <map name="SCRATCH_A" action="chann=1  action=scratch value=&quot;auto&quot;"/>
  <map name="LOOP_IN_A" action="chann=1  action=loop_in_move value=&quot;auto&quot;"/>
  <map name="STEM_A" action="chann=1  action=stem_drum value=&quot;auto&quot;"/>
  <map name="ALT" action="chann=default  action=set_energy_target value=&quot;auto&quot; condition2=&quot;1&quot;"/>
 </midi-map>
</midi>"#;

    fn find<'a>(p: &'a Profile, msg: &str) -> &'a crate::midi_profile::Bind {
        p.binds.iter().find(|b| b.msg == msg).unwrap_or_else(|| panic!("no bind for {msg}"))
    }

    #[test]
    fn converts_the_functions_dubplate_has() {
        let (p, r) = import(SAMPLE).unwrap();
        assert_eq!(p.device.name, "Test Pad");
        let play = find(&p, "91 07");
        assert_eq!((play.func.as_str(), play.deck), ("play_pause", Some(0)));
        assert_eq!(find(&p, "92 07").deck, Some(1));
        let pad = find(&p, "97 02");
        assert_eq!((pad.func.as_str(), pad.arg, pad.deck), ("pad", Some(2), Some(1)), "hot cue 3 is pad index 2");
        assert_eq!(find(&p, "B0 00").func, "crossfader");
        assert_eq!(find(&p, "B0 00").deck, None);
        assert_eq!(r.converted, p.binds.len());
    }

    #[test]
    fn templated_pad_messages_still_import() {
        let (p, _) = import(SAMPLE).unwrap();
        let b = find(&p, "96 10");
        assert_eq!((b.func.as_str(), b.arg), ("stem", Some(0)));
    }

    #[test]
    fn spaceless_messages_are_read() {
        let xml = r#"<midi name="G"><midi-device>
          <control name="PLAY_A"><input message="9001{value}" type="toggle"/></control>
          <control name="PLAY_B"><input message="b0 14 7f" type="toggle"/></control>
         </midi-device><midi-map>
          <map name="PLAY_A" action="chann=1 action=play_pause value=&quot;0&quot;"/>
          <map name="PLAY_B" action="chann=2 action=play_pause value=&quot;0&quot;"/></midi-map></midi>"#;
        let (p, _) = import(xml).unwrap();
        assert_eq!(find(&p, "90 01").deck, Some(0));
        assert_eq!(find(&p, "B0 14").deck, Some(1));
    }

    #[test]
    fn detects_14_bit_controls() {
        let (p, _) = import(SAMPLE).unwrap();
        assert!(find(&p, "B1 00").hires);
        assert!(!find(&p, "B0 00").hires);
    }

    #[test]
    fn touch_and_scratch_are_told_apart_by_input_type() {
        let (p, _) = import(SAMPLE).unwrap();
        assert_eq!(find(&p, "91 08").func, "jog_touch");
        assert_eq!(find(&p, "B1 0A").func, "jog_scratch");
    }

    #[test]
    fn unsupported_and_duplicate_controls_are_reported_not_guessed() {
        let (p, r) = import(SAMPLE).unwrap();
        let why = |n: &str| r.skipped.iter().find(|(c, _)| c == n).map(|(_, w)| w.clone());
        assert!(why("SAMPLE1_A").unwrap().contains("sampler"));
        assert!(why("DUP").unwrap().contains("already"));
        assert!(why("ALT").unwrap().contains("condition"));
        assert!(why("LED_ONLY").is_none(), "LED-only controls are not input and not reported");
        assert!(p.binds.iter().all(|b| b.label != "DUP"));
    }

    #[test]
    fn approximations_are_called_out() {
        let (p, r) = import(SAMPLE).unwrap();
        assert_eq!(find(&p, "94 09").func, "loop_in");
        assert!(r.approximations.iter().any(|a| a.starts_with("LOOP_IN_A")));
    }

    #[test]
    fn output_round_trips_through_toml() {
        let (p, _) = import(SAMPLE).unwrap();
        let back = Profile::from_toml(&p.to_toml()).unwrap();
        assert_eq!(back.binds.len(), p.binds.len());
        back.compile().unwrap();
    }

    #[test]
    fn repairs_entity_quoted_attributes() {
        let bad = r#"<map action="chann=1  action=pitch value=&quot;auto&quot;" name="P" takeover=&quot;1&quot;/>"#;
        let fixed = fix_entity_quotes(bad);
        assert_eq!(fixed, r#"<map action="chann=1  action=pitch value=&quot;auto&quot;" name="P" takeover="1"/>"#);
        let ok = r#"<map action="a=&quot;b&quot;" name="P"/>"#;
        assert_eq!(fix_entity_quotes(ok), ok, "entities inside quoted values are left alone");
        assert!(roxmltree::Document::parse(&fixed).is_ok());
    }

    #[test]
    fn glued_value_placeholder_is_understood() {
        let xml = r#"<midi name="G"><midi-device>
          <control name="CUE_A"><input message="90 07{value}" type="toggle"/></control>
         </midi-device><midi-map><map name="CUE_A" action="chann=1 action=cue_stop value=&quot;auto&quot;"/></midi-map></midi>"#;
        let (p, _) = import(xml).unwrap();
        assert_eq!(p.binds[0].msg, "90 07");
    }

    #[test]
    fn rejects_non_djm_input() {
        assert!(import("<html></html>").is_err());
        assert!(import("garbage").is_err());
    }

    #[test]
    fn action_parser_handles_quotes_and_conditions() {
        let a = parse_action(r#"chann=1  action=loop value="4 temporary" condition2="1""#);
        assert_eq!((a.chann.as_str(), a.name.as_str(), a.value.as_str()), ("1", "loop", "4 temporary"));
        assert!(a.has_condition);
        let b = parse_action(r#"chann=default action=x value="auto" condition2="0""#);
        assert!(!b.has_condition, "condition2=0 is the default mode, not an alternate one");
    }
}
