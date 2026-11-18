//! Compares two profiles of the same controller (typically converted from two different programs).
//!
//! Where two programs agree on what a message does, that is evidence the mapping is right; where
//! they disagree, one of them is wrong, differs by firmware revision, or means something else by
//! the same name — worth a human look.

use std::collections::BTreeMap;

use crate::midi_profile::{parse_msg, Bind, Profile};

#[derive(Debug, Default)]
pub struct Comparison {
    /// Same message, same function.
    pub agree: Vec<(String, String)>,
    /// Same message, different function: (message, A says, B says).
    pub conflicts: Vec<(String, String, String)>,
    /// Same function, different message: (function, A sends, B sends).
    pub moved: Vec<(String, String, String)>,
    pub only_a: Vec<(String, String)>,
    pub only_b: Vec<(String, String)>,
}

fn deck_name(b: &Bind) -> &'static str {
    match b.deck {
        Some(0) => " A",
        Some(1) => " B",
        _ => "",
    }
}

/// Human-readable function: `pad A 2`, `crossfader`.
pub fn describe(b: &Bind) -> String {
    match b.arg {
        Some(a) => format!("{}{} {a}", b.func, deck_name(b)),
        None => format!("{}{}", b.func, deck_name(b)),
    }
}

/// Functions that do the same job in Dubplate's model.
fn canonical(b: &Bind) -> String {
    let func = match b.func.as_str() {
        "loop_toggle" => "loop_out", // OUT is also exit / re-loop
        f => f,
    };
    // `browse`'s arg is only a step multiplier, not a different function.
    let arg = if func == "browse" { None } else { b.arg };
    format!("{func}|{:?}|{:?}", b.deck, arg)
}

fn key(b: &Bind) -> String {
    parse_msg(&b.msg).map(|(s, d)| format!("{s:02X} {d:02X}")).unwrap_or_else(|_| b.msg.clone())
}

pub fn compare(a: &Profile, b: &Profile) -> Comparison {
    fn index(p: &Profile) -> BTreeMap<String, &Bind> {
        p.binds.iter().map(|x| (key(x), x)).collect()
    }
    let (ia, ib) = (index(a), index(b));
    let mut c = Comparison::default();

    for (msg, x) in &ia {
        match ib.get(msg) {
            Some(y) if canonical(x) == canonical(y) => c.agree.push((msg.clone(), describe(x))),
            Some(y) => c.conflicts.push((msg.clone(), describe(x), describe(y))),
            None => c.only_a.push((msg.clone(), describe(x))),
        }
    }
    for (msg, y) in &ib {
        if !ia.contains_key(msg) {
            c.only_b.push((msg.clone(), describe(y)));
        }
    }

    // A function that exists on both sides but on different messages is a probable layer / firmware
    // difference rather than two unrelated leftovers.
    let by_func = |m: &Vec<(String, String)>, p: &Profile| -> Vec<(String, String, String)> {
        m.iter()
            .filter_map(|(msg, d)| {
                let bind = p.binds.iter().find(|x| key(x) == *msg)?;
                Some((canonical(bind), d.clone(), msg.clone()))
            })
            .collect()
    };
    let fa = by_func(&c.only_a, a);
    let fb = by_func(&c.only_b, b);
    let mut moved = Vec::new();
    for (canon, d, ma) in &fa {
        if let Some((_, _, mb)) = fb.iter().find(|(c2, _, _)| c2 == canon) {
            moved.push((d.clone(), ma.clone(), mb.clone()));
        }
    }
    for (d, ma, mb) in &moved {
        c.only_a.retain(|(m, _)| m != ma);
        c.only_b.retain(|(m, _)| m != mb);
        let _ = d;
    }
    c.moved = moved;
    c
}

pub fn render_markdown(a_name: &str, b_name: &str, c: &Comparison) -> String {
    let mut s = String::new();
    s.push_str(&format!("**{a_name}** vs **{b_name}** — {} messages agree, {} conflict, {} same function on a different message, {} only in the first, {} only in the second.\n\n",
        c.agree.len(), c.conflicts.len(), c.moved.len(), c.only_a.len(), c.only_b.len()));
    if !c.conflicts.is_empty() {
        s.push_str("Conflicts (same message, different function):\n\n| Message | A | B |\n|---|---|---|\n");
        for (m, x, y) in &c.conflicts {
            s.push_str(&format!("| `{m}` | {x} | {y} |\n"));
        }
        s.push('\n');
    }
    if !c.moved.is_empty() {
        s.push_str("Same function, different message:\n\n| Function | A sends | B sends |\n|---|---|---|\n");
        for (d, x, y) in &c.moved {
            s.push_str(&format!("| {d} | `{x}` | `{y}` |\n"));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_profile::DeviceInfo;

    fn bind(msg: &str, func: &str, deck: Option<u8>, arg: Option<i32>) -> Bind {
        Bind { msg: msg.into(), func: func.into(), deck, arg, hires: false, label: String::new() }
    }
    fn prof(binds: Vec<Bind>) -> Profile {
        Profile { device: DeviceInfo::default(), binds }
    }

    #[test]
    fn agreement_conflict_and_missing() {
        let a = prof(vec![
            bind("91 07", "play_pause", Some(0), None),
            bind("91 06", "cue", Some(0), None),
            bind("91 09", "loop_in", Some(0), None),
            bind("91 05", "sync", Some(0), None),
        ]);
        let b = prof(vec![
            bind("91 07", "play_pause", Some(0), None),
            bind("91 06", "to_start", Some(0), None),
            bind("91 0B", "loop_beats", Some(0), Some(4)),
        ]);
        let c = compare(&a, &b);
        assert_eq!(c.agree.len(), 1);
        assert_eq!(c.conflicts, vec![("91 06".to_owned(), "cue A".to_owned(), "to_start A".to_owned())]);
        assert_eq!(c.only_a.len(), 2);
        assert_eq!(c.only_b.len(), 1);
        assert!(c.moved.is_empty());
    }

    #[test]
    fn same_function_on_another_message_is_reported_as_moved() {
        let a = prof(vec![bind("91 09", "loop_half", Some(0), None)]);
        let b = prof(vec![bind("94 09", "loop_half", Some(0), None)]);
        let c = compare(&a, &b);
        assert_eq!(c.moved, vec![("loop_half A".to_owned(), "91 09".to_owned(), "94 09".to_owned())]);
        assert!(c.only_a.is_empty() && c.only_b.is_empty());
    }

    #[test]
    fn loop_out_and_loop_toggle_are_equivalent() {
        let a = prof(vec![bind("91 0A", "loop_out", Some(0), None)]);
        let b = prof(vec![bind("91 0A", "loop_toggle", Some(0), None)]);
        assert_eq!(compare(&a, &b).agree.len(), 1);
    }

    #[test]
    fn browse_step_size_is_not_a_conflict() {
        let a = prof(vec![bind("B3 01", "browse", None, Some(8))]);
        let b = prof(vec![bind("B3 01", "browse", None, Some(1))]);
        assert_eq!(compare(&a, &b).agree.len(), 1);
    }

    #[test]
    fn note_off_and_note_on_compare_equal() {
        let a = prof(vec![bind("91 07", "play_pause", Some(0), None)]);
        let b = prof(vec![bind("81 07", "play_pause", Some(0), None)]);
        assert_eq!(compare(&a, &b).agree.len(), 1);
    }

    #[test]
    fn markdown_mentions_counts() {
        let a = prof(vec![bind("91 06", "cue", Some(0), None)]);
        let b = prof(vec![bind("91 06", "to_start", Some(0), None)]);
        let md = render_markdown("A", "B", &compare(&a, &b));
        assert!(md.contains("1 conflict") && md.contains("`91 06`"));
    }
}
