//! The library search box: free text plus `field:value` terms.
//!
//! ```text
//! bpm:120-128  bpm:124  bpm:~124 (±6%)  bpm:>130  bpm:<90
//! key:8A  key:~8A (8A and its harmonic neighbours 7A 9A 8B)
//! artist:daft  album:  genre:house  title:  folder:techno  year:1999  year:1995-2005
//! added:7d  added:2w  added:3m
//! "exact phrase"   -term (exclude)   anything else = text in title/artist/album/genre/file name
//! ```
//! Terms are ANDed. A term that can't be understood is kept, flagged, and ignored, so typing
//! `bpm:` half-way never empties the list.

use dubplate_engine::Key;

use crate::db::TrackRow;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    Title,
    Artist,
    Album,
    Genre,
    Folder,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Text(String),
    Field(Field, String),
    Bpm(f32, f32),
    Key {
        number: u8,
        letter: char,
        harmonic: bool,
    },
    /// Added within the last N seconds.
    Added(i64),
    Year(u32, u32),
}

#[derive(Clone, Debug)]
struct Term {
    negate: bool,
    kind: Kind,
}

/// One parsed term as shown under the search box.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub label: String,
    /// False when the term could not be understood (and is ignored).
    pub ok: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    terms: Vec<Term>,
    chips: Vec<Chip>,
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query::default();
        for (raw, quoted) in tokens(input) {
            let (negate, body) = match raw.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_owned()),
                _ => (false, raw.clone()),
            };
            let parsed = if quoted { Ok(Kind::Text(body.to_lowercase())) } else { term(&body) };
            match parsed {
                Ok(kind) => {
                    let label = format!("{}{}", if negate { "not " } else { "" }, describe(&kind));
                    q.chips.push(Chip { label, ok: true });
                    q.terms.push(Term { negate, kind });
                }
                Err(why) => q.chips.push(Chip { label: format!("{raw}  ({why})"), ok: false }),
            }
        }
        q
    }

    /// How the terms were read, for the chip row. Plain single words are not worth a chip.
    pub fn chips(&self) -> Vec<Chip> {
        let advanced =
            self.chips.iter().any(|c| !c.ok) || self.terms.iter().any(|t| t.negate || !matches!(t.kind, Kind::Text(_)));
        if advanced {
            self.chips.clone()
        } else {
            Vec::new()
        }
    }

    /// `now` is unix seconds (for `added:`).
    #[cfg(test)]
    pub fn matches(&self, r: &TrackRow, now: i64) -> bool {
        self.terms.iter().all(|t| t.kind.matches(r, now) != t.negate)
    }

    /// [`matches`](Self::matches) with the row's search text already lower-cased (`hay`: title, artist, album, genre
    /// and file name), so a plain word costs one search instead of five lower-cased copies per row per keystroke.
    pub fn matches_in(&self, r: &TrackRow, hay: &str, now: i64) -> bool {
        self.terms.iter().all(|t| {
            let hit = match &t.kind {
                Kind::Text(n) => hay.contains(n.as_str()),
                kind => kind.matches(r, now),
            };
            hit != t.negate
        })
    }
}

impl Kind {
    fn matches(&self, r: &TrackRow, now: i64) -> bool {
        let has = |s: &Option<String>, needle: &str| s.as_deref().is_some_and(|s| s.to_lowercase().contains(needle));
        match self {
            Kind::Text(n) => {
                r.display_title().to_lowercase().contains(n)
                    || has(&r.artist, n)
                    || has(&r.album, n)
                    || has(&r.genre, n)
                    || r.path.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.to_lowercase().contains(n))
            }
            Kind::Field(f, n) => match f {
                Field::Title => r.display_title().to_lowercase().contains(n),
                Field::Artist => has(&r.artist, n),
                Field::Album => has(&r.album, n),
                Field::Genre => has(&r.genre, n),
                Field::Folder => r.path.parent().is_some_and(|p| p.to_string_lossy().to_lowercase().contains(n)),
            },
            Kind::Bpm(lo, hi) => r.bpm.is_some_and(|b| b >= *lo && b <= *hi),
            Kind::Key { number, letter, harmonic } => r.key.is_some_and(|(tonic, minor)| {
                let (n, l) = Key { tonic, minor, confidence: 0.0 }.camelot();
                if !harmonic {
                    return n == *number && l == *letter;
                }
                // Same number either letter, or the same letter one step round the wheel.
                let step = (n as i32 - *number as i32).rem_euclid(12);
                (n == *number) || (l == *letter && (step == 1 || step == 11))
            }),
            Kind::Added(secs) => r.added_at > 0 && now - r.added_at <= *secs,
            Kind::Year(lo, hi) => r.year.is_some_and(|y| y >= *lo && y <= *hi),
        }
    }
}

fn describe(k: &Kind) -> String {
    match k {
        Kind::Text(s) => format!("text “{s}”"),
        Kind::Field(f, s) => format!("{} “{s}”", format!("{f:?}").to_lowercase()),
        Kind::Bpm(lo, hi) => format!("BPM {lo:.1}–{hi:.1}"),
        Kind::Key { number, letter, harmonic } => {
            format!("key {number}{letter}{}", if *harmonic { " + neighbours" } else { "" })
        }
        Kind::Added(s) => format!("added in the last {}", span(*s)),
        Kind::Year(lo, hi) => {
            if lo == hi {
                format!("year {lo}")
            } else {
                format!("year {lo}–{hi}")
            }
        }
    }
}

fn span(secs: i64) -> String {
    let day = 86_400;
    if secs % (30 * day) == 0 {
        format!("{} month(s)", secs / (30 * day))
    } else if secs % (7 * day) == 0 {
        format!("{} week(s)", secs / (7 * day))
    } else {
        format!("{} day(s)", secs / day)
    }
}

/// Splits on whitespace, keeping "quoted phrases" together. Returns (text, was_quoted).
fn tokens(input: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let (mut cur, mut quoted, mut in_quote) = (String::new(), false, false);
    for c in input.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                quoted = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push((std::mem::take(&mut cur), quoted));
                }
                quoted = false;
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push((cur, quoted));
    }
    out
}

fn term(s: &str) -> Result<Kind, String> {
    let lower = s.to_lowercase();
    let Some((field, value)) = lower.split_once(':') else { return Ok(Kind::Text(lower)) };
    if value.is_empty() {
        // "bpm:" while still typing; a bare "word:" is not a filter yet.
        return if is_field(field) { Err("waiting for a value".into()) } else { Ok(Kind::Text(lower)) };
    }
    match field {
        "title" => Ok(Kind::Field(Field::Title, value.to_owned())),
        "artist" => Ok(Kind::Field(Field::Artist, value.to_owned())),
        "album" => Ok(Kind::Field(Field::Album, value.to_owned())),
        "genre" => Ok(Kind::Field(Field::Genre, value.to_owned())),
        "folder" | "path" => Ok(Kind::Field(Field::Folder, value.to_owned())),
        "bpm" => bpm(value),
        "key" => key(value),
        "added" => added(value),
        "year" => year(value),
        _ => Ok(Kind::Text(lower)), // "AC:DC" is just text
    }
}

fn is_field(f: &str) -> bool {
    matches!(f, "title" | "artist" | "album" | "genre" | "folder" | "path" | "bpm" | "key" | "added" | "year")
}

fn num(s: &str) -> Result<f32, String> {
    s.parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| format!("“{s}” is not a number"))
}

fn bpm(v: &str) -> Result<Kind, String> {
    if let Some(r) = v.strip_prefix('~') {
        let b = num(r)?;
        return Ok(Kind::Bpm(b * 0.94, b * 1.06));
    }
    if let Some(r) = v.strip_prefix('>') {
        return Ok(Kind::Bpm(num(r)?, f32::MAX));
    }
    if let Some(r) = v.strip_prefix('<') {
        return Ok(Kind::Bpm(0.0, num(r)?));
    }
    if let Some((a, b)) = v.split_once('-') {
        let (a, b) = (num(a)?, num(b)?);
        return Ok(Kind::Bpm(a.min(b), a.max(b)));
    }
    let b = num(v)?;
    Ok(Kind::Bpm(b - 0.5, b + 0.5)) // a whole-number BPM covers the rounding in the table
}

fn key(v: &str) -> Result<Kind, String> {
    let (harmonic, v) = match v.strip_prefix('~') {
        Some(r) => (true, r),
        None => (false, v),
    };
    let Some((at, last)) = v.char_indices().last() else { return Err("use a Camelot key such as 8A".into()) };
    let letter = Some(last.to_ascii_uppercase()).filter(|c| *c == 'A' || *c == 'B');
    let number = v[..at].parse::<u8>().ok().filter(|n| (1..=12).contains(n));
    match (number, letter) {
        (Some(number), Some(letter)) => Ok(Kind::Key { number, letter, harmonic }),
        _ => Err("use a Camelot key such as 8A".into()),
    }
}

fn added(v: &str) -> Result<Kind, String> {
    let (at, unit) = v.char_indices().last().ok_or("use 7d, 2w or 3m")?;
    let n: i64 = v[..at].parse().ok().filter(|n| *n > 0).ok_or("use 7d, 2w or 3m")?;
    let day = 86_400;
    match unit {
        'd' => Ok(Kind::Added(n * day)),
        'w' => Ok(Kind::Added(n * 7 * day)),
        'm' => Ok(Kind::Added(n * 30 * day)),
        _ => Err("use 7d, 2w or 3m".into()),
    }
}

fn year(v: &str) -> Result<Kind, String> {
    let y = |s: &str| s.parse::<u32>().map_err(|_| format!("“{s}” is not a year"));
    match v.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (y(a)?, y(b)?);
            Ok(Kind::Year(a.min(b), a.max(b)))
        }
        None => {
            let a = y(v)?;
            Ok(Kind::Year(a, a))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn row(title: &str, artist: &str, bpm: f32, camelot: (u8, char)) -> TrackRow {
        // Find the (tonic, minor) whose Camelot label is `camelot`.
        let key = (0u8..12)
            .flat_map(|t| [(t, false), (t, true)])
            .find(|&(t, m)| Key { tonic: t, minor: m, confidence: 0.0 }.camelot() == camelot);
        TrackRow {
            id: 1,
            path: PathBuf::from("/music/House/x.mp3"),
            title: Some(title.into()),
            artist: Some(artist.into()),
            album: Some("Album".into()),
            genre: Some("House".into()),
            year: Some(1999),
            bpm: Some(bpm),
            bpm_manual: false,
            key,
            duration_secs: Some(200.0),
            preview: vec![],
            added_at: 1_000_000,
            analysed: true,
        }
    }
    fn hit(q: &str, r: &TrackRow) -> bool {
        Query::parse(q).matches(r, 1_000_000 + 3 * 86_400)
    }

    #[test]
    fn plain_words_are_anded_across_fields() {
        let r = row("One More Time", "Daft Punk", 122.0, (8, 'A'));
        assert!(hit("daft time", &r));
        assert!(hit("", &r));
        assert!(!hit("daft nothing", &r));
        assert!(hit("house", &r), "genre is searched");
    }

    #[test]
    fn bpm_forms() {
        let r = row("t", "a", 124.0, (8, 'A'));
        assert!(hit("bpm:124", &r) && hit("bpm:120-128", &r) && hit("bpm:128-120", &r));
        assert!(hit("bpm:>120", &r) && !hit("bpm:>130", &r) && hit("bpm:<130", &r));
        assert!(hit("bpm:~120", &r), "120 ±6% reaches 124");
        assert!(!hit("bpm:~110", &r));
        assert!(!hit("bpm:124", &row("t", "a", 128.0, (8, 'A'))));
    }

    #[test]
    fn key_exact_and_harmonic() {
        let r = row("t", "a", 124.0, (8, 'A'));
        assert!(hit("key:8a", &r) && !hit("key:9A", &r));
        for k in ["8A", "8B", "7A", "9A"] {
            assert!(hit(&format!("key:~{k}"), &r), "{k}");
        }
        for k in ["7B", "9B", "10A", "2A"] {
            assert!(!hit(&format!("key:~{k}"), &r), "{k}");
        }
        let wrap = row("t", "a", 124.0, (12, 'A'));
        assert!(hit("key:~1A", &wrap), "12A neighbours 1A round the wheel");
    }

    #[test]
    fn negation_quotes_and_dates() {
        let r = row("One More Time", "Daft Punk", 122.0, (8, 'A'));
        assert!(!hit("-daft", &r) && hit("-nobody", &r));
        assert!(hit("\"more time\"", &r) && !hit("\"time more\"", &r));
        assert!(hit("added:7d", &r) && !hit("added:1d", &r));
        assert!(hit("year:1995-2005", &r) && !hit("year:2010", &r));
        assert!(hit("artist:punk", &r) && !hit("artist:house", &r));
        assert!(hit("folder:house", &r));
    }

    #[test]
    fn half_typed_or_bad_terms_are_flagged_and_ignored() {
        let r = row("t", "a", 124.0, (8, 'A'));
        for q in ["bpm:", "bpm:abc", "key:99", "added:soon", "year:x"] {
            let p = Query::parse(q);
            assert!(hit(q, &r), "{q} must not empty the list");
            assert!(p.chips().iter().any(|c| !c.ok), "{q} should be flagged");
        }
        assert!(Query::parse("AC:DC").chips().is_empty(), "AC:DC is just text");
    }

    #[test]
    fn chips_only_appear_for_advanced_terms() {
        assert!(Query::parse("daft punk").chips().is_empty());
        let c = Query::parse("bpm:120-128 -remix").chips();
        assert_eq!(c.len(), 2);
        assert!(c[1].label.starts_with("not "));
    }
}
