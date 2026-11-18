//! Flux: how well a track would mix after the one that is playing.
//!
//! Three ingredients, each 0..1:
//! * **tempo** — within the deck's +-8 % tempo range (also via x2 / x0.5);
//! * **key** — Camelot-wheel distance (same, neighbour and relative keys mix cleanly);
//! * **genre** — same genre tag.
//!
//! Unknown ingredients are neutral and drop out of the weighting, so a track with
//! no key tag is not punished for it.

/// What we know about a track for scoring.
#[derive(Clone, Debug, Default)]
pub struct FluxInfo {
    pub bpm: Option<f32>,
    /// Camelot (number 1..=12, letter 'A' minor / 'B' major).
    pub camelot: Option<(u8, char)>,
    pub genre: Option<String>,
}

/// Result with the parts, so the UI can explain it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flux {
    /// 0..=100
    pub score: u8,
    pub tempo: Option<f32>,
    pub key: Option<f32>,
    pub genre: Option<f32>,
}

const W_TEMPO: f32 = 0.5;
const W_KEY: f32 = 0.3;
const W_GENRE: f32 = 0.2;
/// Playable tempo window (matches the +-8 % tempo fader).
const TEMPO_RANGE: f32 = 0.08;
/// Differences below this are treated as identical (sync makes them equal).
const TEMPO_FREE: f32 = 0.02;

/// Relative tempo gap after trying x2 and x0.5 of `candidate`.
fn tempo_gap(reference: f32, candidate: f32) -> f32 {
    [candidate, candidate * 2.0, candidate * 0.5]
        .iter()
        .map(|c| (c - reference).abs() / reference)
        .fold(f32::MAX, f32::min)
}

pub fn tempo_score(reference: f32, candidate: f32) -> f32 {
    if reference <= 0.0 || candidate <= 0.0 {
        return 0.0;
    }
    let gap = tempo_gap(reference, candidate);
    (1.0 - ((gap - TEMPO_FREE) / (TEMPO_RANGE - TEMPO_FREE)).clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// Steps between two Camelot numbers around the 12-position wheel.
fn wheel_distance(a: u8, b: u8) -> u8 {
    let d = (a as i16 - b as i16).rem_euclid(12) as u8;
    d.min(12 - d)
}

/// Harmonic compatibility on the Camelot wheel.
pub fn key_score(a: (u8, char), b: (u8, char)) -> f32 {
    let dist = wheel_distance(a.0, b.0);
    let same_letter = a.1 == b.1;
    match (dist, same_letter) {
        (0, true) => 1.0,   // same key
        (0, false) => 0.9,  // relative major/minor
        (1, true) => 0.9,   // neighbour: perfect fifth / fourth
        (1, false) => 0.45, // diagonal: sometimes works
        (2, true) => 0.6,   // two steps: energy change
        (2, false) => 0.25,
        (_, true) => 0.2,
        (_, false) => 0.1,
    }
}

fn norm_genre(g: &str) -> String {
    g.to_lowercase()
        .replace('&', " and ")
        .replace(['-', '_', '/'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn genre_score(a: &str, b: &str) -> f32 {
    let (a, b) = (norm_genre(a), norm_genre(b));
    if a.is_empty() || b.is_empty() {
        return 0.5;
    }
    if a == b {
        return 1.0;
    }
    if a.contains(&b) || b.contains(&a) {
        return 0.7;
    } // "drum and bass" vs "drum and bass / jungle"
    let aw: Vec<&str> = a.split_whitespace().collect();
    if b.split_whitespace().any(|w| w.len() > 2 && aw.contains(&w)) {
        return 0.5;
    }
    0.15
}

/// Scores `candidate` as a follow-up to `reference`. `None` when there is no tempo for
/// either track (nothing meaningful to say).
pub fn flux(reference: &FluxInfo, candidate: &FluxInfo) -> Option<Flux> {
    let tempo = match (reference.bpm, candidate.bpm) {
        (Some(r), Some(c)) => Some(tempo_score(r, c)),
        _ => None,
    };
    let key = match (reference.camelot, candidate.camelot) {
        (Some(r), Some(c)) => Some(key_score(r, c)),
        _ => None,
    };
    let genre = match (&reference.genre, &candidate.genre) {
        (Some(r), Some(c)) => Some(genre_score(r, c)),
        _ => None,
    };
    tempo?; // tempo is the backbone: without it we can't rank
    let parts = [(tempo, W_TEMPO), (key, W_KEY), (genre, W_GENRE)];
    let (sum, weight) =
        parts.iter().filter_map(|(v, w)| v.map(|v| (v * w, *w))).fold((0.0, 0.0), |(s, w), (vs, ww)| (s + vs, w + ww));
    Some(Flux { score: (sum / weight * 100.0).round() as u8, tempo, key, genre })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(bpm: f32, cam: Option<(u8, char)>, genre: Option<&str>) -> FluxInfo {
        FluxInfo { bpm: Some(bpm), camelot: cam, genre: genre.map(String::from) }
    }

    #[test]
    fn tempo_within_two_percent_is_perfect_and_falls_to_zero_at_eight() {
        assert_eq!(tempo_score(128.0, 128.0), 1.0);
        assert_eq!(tempo_score(128.0, 130.0), 1.0, "1.6 % apart sync-matches");
        let mid = tempo_score(128.0, 128.0 * 1.05);
        assert!(mid > 0.3 && mid < 0.7, "{mid}");
        assert_eq!(tempo_score(128.0, 128.0 * 1.10), 0.0);
    }

    #[test]
    fn half_and_double_tempo_count_as_matches() {
        assert_eq!(tempo_score(174.0, 87.0), 1.0);
        assert_eq!(tempo_score(87.0, 174.0), 1.0);
        assert_eq!(tempo_score(170.0, 85.0), 1.0);
    }

    #[test]
    fn camelot_wheel_distances() {
        assert_eq!(key_score((8, 'A'), (8, 'A')), 1.0);
        assert_eq!(key_score((8, 'A'), (9, 'A')), 0.9);
        assert_eq!(key_score((8, 'A'), (7, 'A')), 0.9);
        assert_eq!(key_score((8, 'A'), (8, 'B')), 0.9, "relative major");
        assert_eq!(key_score((12, 'A'), (1, 'A')), 0.9, "wheel wraps");
        assert!(key_score((8, 'A'), (2, 'B')) < 0.2);
    }

    #[test]
    fn genre_matching_is_forgiving_about_formatting() {
        assert_eq!(genre_score("Drum & Bass", "drum and bass"), 1.0);
        assert_eq!(genre_score("Hip-Hop", "hip hop"), 1.0);
        assert_eq!(genre_score("Techno", "Trance"), 0.15);
        assert_eq!(genre_score("House", "house"), 1.0);
        assert!(genre_score("Deep House", "House") >= 0.7);
        assert_eq!(genre_score("", "House"), 0.5, "unknown is neutral");
    }

    #[test]
    fn a_perfect_follow_up_scores_100_and_a_clash_scores_low() {
        let r = info(126.0, Some((8, 'A')), Some("House"));
        assert_eq!(flux(&r, &info(126.0, Some((8, 'A')), Some("House"))).unwrap().score, 100);
        let clash = flux(&r, &info(150.0, Some((3, 'B')), Some("Trance"))).unwrap();
        assert!(clash.score < 20, "{clash:?}");
    }

    #[test]
    fn missing_key_or_genre_does_not_punish_the_score() {
        let r = info(126.0, Some((8, 'A')), Some("House"));
        let c = FluxInfo { bpm: Some(126.0), camelot: None, genre: None };
        assert_eq!(flux(&r, &c).unwrap().score, 100);
    }

    #[test]
    fn no_tempo_means_no_flux() {
        let r = info(126.0, None, None);
        assert!(flux(&r, &FluxInfo::default()).is_none());
    }
}
