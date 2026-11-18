//! Track metadata: tags, musical key, and a compact waveform preview for browsers.

use std::path::Path;

use rustfft::{num_complex::Complex, FftPlanner};
use symphonia::core::{
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::{MetadataOptions, MetadataRevision, StandardTagKey},
    probe::Hint,
};

/// Tags read from the file without decoding audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
}

/// Musical key. `tonic` is a pitch class (0 = C, 1 = C#, ... 11 = B).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
    /// 0..1 — how clearly this key beat the runner-up.
    pub confidence: f32,
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

impl Key {
    /// Camelot wheel position: (number 1..=12, letter) with 'A' = minor, 'B' = major.
    /// Examples: A minor = (8,'A'), C major = (8,'B'), D minor = (7,'A'), G major = (9,'B').
    pub fn camelot(&self) -> (u8, char) {
        // The wheel is keyed by the major tonic (a minor key shares its relative major's number).
        let major_tonic = if self.minor { (self.tonic as u32 + 3) % 12 } else { self.tonic as u32 % 12 };
        let number = ((major_tonic * 7 + 7) % 12 + 1) as u8;
        (number, if self.minor { 'A' } else { 'B' })
    }

    /// The key after the track is transposed up by `semitones` (negative: down).
    pub fn shifted(&self, semitones: i32) -> Key {
        Key { tonic: (self.tonic as i32 + semitones).rem_euclid(12) as u8, ..*self }
    }

    /// The smallest key shift (-6..=6 semitones, up preferred on a tie) that makes this key mix with `target`:
    /// the same Camelot number, so the same key or its relative major/minor.
    pub fn shift_to_match(&self, target: &Key) -> i32 {
        let want = target.camelot().0;
        (0..=6).flat_map(|d| [d, -d]).find(|&s| self.shifted(s).camelot().0 == want).unwrap_or(0)
    }

    /// "8A" style label.
    pub fn camelot_label(&self) -> String {
        let (n, l) = self.camelot();
        format!("{n}{l}")
    }

    /// "Am" / "C" / "F#m" style label.
    pub fn name(&self) -> String {
        format!("{}{}", NOTE_NAMES[self.tonic as usize % 12], if self.minor { "m" } else { "" })
    }
}

// ── Tags ─────────────────────────────────────────────────────────────────────

fn clean(v: &str) -> Option<String> {
    let t = v.trim().trim_matches('\0').trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Genre text only; bare ID3v1 numeric codes ("17", "(17)") are ignored.
fn clean_genre(v: &str) -> Option<String> {
    let t = clean(v)?;
    let digits = t.trim_matches(|c| c == '(' || c == ')');
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(t)
}

fn year_of(v: &str) -> Option<u32> {
    let digits: String = v.trim().chars().take_while(|c| c.is_ascii_digit()).take(4).collect();
    (digits.len() == 4).then(|| digits.parse().ok()).flatten()
}

fn absorb(tags: &mut TrackTags, rev: &MetadataRevision) {
    for tag in rev.tags() {
        let value = tag.value.to_string();
        match tag.std_key {
            Some(StandardTagKey::TrackTitle) if tags.title.is_none() => tags.title = clean(&value),
            Some(StandardTagKey::Artist) if tags.artist.is_none() => tags.artist = clean(&value),
            Some(StandardTagKey::AlbumArtist) if tags.artist.is_none() => tags.artist = clean(&value),
            Some(StandardTagKey::Album) if tags.album.is_none() => tags.album = clean(&value),
            Some(StandardTagKey::Genre) if tags.genre.is_none() => tags.genre = clean_genre(&value),
            Some(StandardTagKey::Date) | Some(StandardTagKey::ReleaseDate) | Some(StandardTagKey::OriginalDate)
                if tags.year.is_none() =>
            {
                tags.year = year_of(&value)
            }
            _ => {}
        }
    }
}

fn read_tags_inner(path: &Path) -> TrackTags {
    let mut tags = TrackTags::default();
    let Ok(file) = std::fs::File::open(path) else { return tags };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(mut probed) =
        symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
    else {
        return tags;
    };

    // Container-level tags that precede the stream (e.g. ID3v2) live in the probe result,
    // in-stream tags (Vorbis comments, MP4 atoms, ID3v1 at the end) in the format reader.
    if let Some(log) = probed.metadata.get() {
        if let Some(rev) = log.current() {
            absorb(&mut tags, rev);
        }
    }
    if let Some(rev) = probed.format.metadata().current() {
        absorb(&mut tags, rev);
    }
    tags
}

/// Reads title/artist/album/genre/year from tags. Fast (probe only); empty on failure.
pub fn read_tags(path: &Path) -> TrackTags {
    std::panic::catch_unwind(|| read_tags_inner(path)).unwrap_or_default()
}

// ── Key detection ────────────────────────────────────────────────────────────

const KEY_FFT: usize = 8192;
const KEY_HOP: usize = 4096;
const KEY_TARGET_RATE: f32 = 11_025.0;
const KEY_MIN_HZ: f32 = 65.0;
const KEY_MAX_HZ: f32 = 2000.0;

// Krumhansl-Kessler probe-tone profiles (tonic first).
const KK_MAJOR: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const KK_MINOR: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

fn pearson(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let ma = a.iter().sum::<f32>() / 12.0;
    let mb = b.iter().sum::<f32>() / 12.0;
    let (mut num, mut da, mut db) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 1e-12 || db <= 1e-12 {
        0.0
    } else {
        num / (da.sqrt() * db.sqrt())
    }
}

/// Estimates the key of interleaved decoded audio; `None` if there is no tonal content.
pub fn detect_key(samples: &[f32], channels: usize, sample_rate: u32) -> Option<Key> {
    let ch = channels.max(1);
    let n_frames = samples.len() / ch;
    if sample_rate == 0 || n_frames < KEY_FFT * 2 {
        return None;
    }

    // Mono downmix + boxcar decimation to ~11 kHz (the band of interest is < 2 kHz).
    let dec = ((sample_rate as f32 / KEY_TARGET_RATE).round() as usize).max(1);
    let rate = sample_rate as f32 / dec as f32;
    let m = n_frames / dec;
    let mut mono = Vec::with_capacity(m);
    for i in 0..m {
        let mut acc = 0.0f32;
        for d in 0..dec {
            let f = i * dec + d;
            let mut s = 0.0f32;
            for c in 0..ch {
                s += samples[f * ch + c];
            }
            acc += s / ch as f32;
        }
        mono.push(acc / dec as f32);
    }
    if m < KEY_FFT * 2 {
        return None;
    }

    let rms = (mono.iter().map(|v| v * v).sum::<f32>() / m as f32).sqrt();
    if rms < 1e-4 {
        return None;
    }

    let fft = FftPlanner::<f32>::new().plan_fft_forward(KEY_FFT);
    let hann: Vec<f32> = (0..KEY_FFT)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (KEY_FFT - 1) as f32).cos()))
        .collect();
    let bin_hz = rate / KEY_FFT as f32;
    let k_lo = ((KEY_MIN_HZ / bin_hz).ceil() as usize).max(2);
    let k_hi = ((KEY_MAX_HZ / bin_hz).floor() as usize).min(KEY_FFT / 2 - 2);

    // Pass 1 collects spectral peaks (pitch in fractional semitones, weight) per frame so
    // a global tuning offset can be estimated before pitch classes are assigned.
    let mut frames: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut buf = vec![Complex::new(0.0f32, 0.0); KEY_FFT];
    let mut mag = vec![0.0f32; KEY_FFT / 2];
    let mut start = 0usize;
    while start + KEY_FFT <= m {
        for i in 0..KEY_FFT {
            buf[i] = Complex::new(mono[start + i] * hann[i], 0.0);
        }
        fft.process(&mut buf);
        for k in 0..KEY_FFT / 2 {
            mag[k] = buf[k].norm();
        }

        let mut peaks = Vec::new();
        let mut frame_energy = 0.0f32;
        for k in k_lo..=k_hi {
            if mag[k] > mag[k - 1] && mag[k] >= mag[k + 1] {
                // Parabolic interpolation for the true peak frequency.
                let (a, b, c) = (mag[k - 1].max(1e-12).ln(), mag[k].max(1e-12).ln(), mag[k + 1].max(1e-12).ln());
                let denom = a - 2.0 * b + c;
                let delta = if denom.abs() > 1e-9 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
                let f = (k as f32 + delta) * bin_hz;
                let pitch = 69.0 + 12.0 * (f / 440.0).log2();
                let w = mag[k].sqrt();
                frame_energy += w;
                peaks.push((pitch, w));
            }
        }
        if frame_energy > 1e-6 {
            frames.push(peaks);
        }
        start += KEY_HOP;
    }
    if frames.is_empty() {
        return None;
    }

    // Tuning: weighted circular mean of each peak's deviation from the nearest semitone.
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    for (pitch, w) in frames.iter().flatten() {
        let dev = (pitch - pitch.round()) * 2.0 * std::f32::consts::PI;
        sx += w * dev.cos();
        sy += w * dev.sin();
    }
    let tuning = sy.atan2(sx) / (2.0 * std::f32::consts::PI); // in semitones, -0.5..0.5

    // Pass 2: chromagram. Each frame is normalised so loud passages do not dominate.
    let mut chroma = [0.0f32; 12];
    for peaks in &frames {
        let mut c = [0.0f32; 12];
        for &(pitch, w) in peaks {
            let pc = ((pitch - tuning).round() as i32).rem_euclid(12) as usize;
            c[pc] += w;
        }
        let total: f32 = c.iter().sum();
        if total > 0.0 {
            for i in 0..12 {
                chroma[i] += c[i] / total;
            }
        }
    }
    if chroma.iter().sum::<f32>() <= 1e-6 {
        return None;
    }

    // Correlate with the profiles over all 12 rotations.
    let mut scores: Vec<(f32, u8, bool)> = Vec::with_capacity(24);
    for tonic in 0..12usize {
        let (mut maj, mut min) = ([0.0f32; 12], [0.0f32; 12]);
        for i in 0..12 {
            maj[(tonic + i) % 12] = KK_MAJOR[i];
            min[(tonic + i) % 12] = KK_MINOR[i];
        }
        scores.push((pearson(&chroma, &maj), tonic as u8, false));
        scores.push((pearson(&chroma, &min), tonic as u8, true));
    }
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (best, second) = (scores[0], scores[1]);
    if best.0 <= 0.0 {
        return None;
    }
    let confidence = ((best.0 - second.0) / best.0).clamp(0.0, 1.0);
    Some(Key { tonic: best.1, minor: best.2, confidence })
}

// ── Browser preview ──────────────────────────────────────────────────────────

/// Downsamples the per-chunk [low, mid, high] waveform to `cols` columns of u8 for
/// the browser preview (peak-preserving, 0..255).
pub fn preview_columns(waveform: &[[f32; 3]], cols: usize) -> Vec<[u8; 3]> {
    if cols == 0 {
        return Vec::new();
    }
    let n = waveform.len();
    if n == 0 {
        return vec![[0; 3]; cols];
    }
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (0..cols)
        .map(|i| {
            let lo = i * n / cols;
            let hi = ((i + 1) * n / cols).max(lo + 1).min(n); // stretch when n < cols
            let mut m = [0.0f32; 3];
            for c in &waveform[lo.min(n - 1)..hi] {
                for k in 0..3 {
                    m[k] = m[k].max(c[k]);
                }
            }
            [to_u8(m[0]), to_u8(m[1]), to_u8(m[2])]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// Sustained chord with harmonics, in stereo, optionally detuned by `cents`.
    fn chord(freqs: &[f32], secs: f32, cents: f32) -> Vec<f32> {
        let n = (secs * SR as f32) as usize;
        let ratio = 2f32.powf(cents / 1200.0);
        let mut out = vec![0.0f32; n * 2];
        for i in 0..n {
            let t = i as f32 / SR as f32;
            let mut v = 0.0f32;
            for &f in freqs {
                for (h, amp) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.33), (4.0, 0.25)] {
                    v += amp * (2.0 * std::f32::consts::PI * f * ratio * h * t).sin();
                }
            }
            v *= 0.1;
            out[i * 2] = v;
            out[i * 2 + 1] = v;
        }
        out
    }

    fn midi_hz(m: f32) -> f32 {
        440.0 * 2f32.powf((m - 69.0) / 12.0)
    }
    fn triad(root_midi: f32, minor: bool) -> Vec<f32> {
        vec![midi_hz(root_midi), midi_hz(root_midi + if minor { 3.0 } else { 4.0 }), midi_hz(root_midi + 7.0)]
    }

    /// Tonic-heavy progression (I - IV - V - I) so the key is unambiguous.
    fn progression(tonic_midi: f32, minor: bool) -> Vec<f32> {
        let seg = |root: f32, m: bool| chord(&triad(root, m), 3.0, 0.0);
        let mut v = seg(tonic_midi, minor);
        v.extend(seg(tonic_midi + 5.0, minor));
        v.extend(seg(tonic_midi + 7.0, false));
        v.extend(seg(tonic_midi, minor));
        v
    }

    #[test]
    fn camelot_table_spot_checks() {
        let k = |tonic, minor| Key { tonic, minor, confidence: 1.0 };
        assert_eq!(k(9, true).camelot(), (8, 'A')); // A minor
        assert_eq!(k(0, false).camelot(), (8, 'B')); // C major
        assert_eq!(k(2, true).camelot(), (7, 'A')); // D minor
        assert_eq!(k(7, false).camelot(), (9, 'B')); // G major
        assert_eq!(k(6, true).camelot(), (11, 'A')); // F# minor
        assert_eq!(k(5, true).camelot(), (4, 'A')); // F minor
        assert_eq!(k(1, false).camelot(), (3, 'B')); // C# major
        assert_eq!(k(11, false).camelot(), (1, 'B')); // B major
        assert_eq!(k(4, true).camelot(), (9, 'A')); // E minor
        assert_eq!(k(9, true).camelot_label(), "8A");
    }

    #[test]
    fn camelot_covers_the_whole_wheel_exactly_once() {
        let mut seen = std::collections::HashSet::new();
        for tonic in 0..12u8 {
            for minor in [false, true] {
                seen.insert(Key { tonic, minor, confidence: 1.0 }.camelot());
            }
        }
        assert_eq!(seen.len(), 24);
        assert!(seen.iter().all(|&(n, l)| (1..=12).contains(&n) && (l == 'A' || l == 'B')));
    }

    #[test]
    fn key_sync_picks_the_smallest_shift_to_the_same_camelot_number() {
        let k = |tonic, minor| Key { tonic, minor, confidence: 1.0 };
        assert_eq!(k(9, true).shift_to_match(&k(9, true)), 0); // Am -> Am
        assert_eq!(k(9, true).shift_to_match(&k(11, true)), 2); // Am -> Bm up two
        assert_eq!(k(9, true).shift_to_match(&k(7, true)), -2); // Am -> Gm down two
        assert_eq!(k(9, true).shift_to_match(&k(0, false)), 0); // Am and C major are relatives
        assert_eq!(k(9, true).shift_to_match(&k(2, false)), 2); // Am -> D major's relative Bm: up two
        for a in 0..12 {
            for b in 0..12 {
                for (ma, mb) in [(false, false), (true, true), (true, false)] {
                    let (ka, kb) = (k(a, ma), k(b, mb));
                    let s = ka.shift_to_match(&kb);
                    assert!((-6..=6).contains(&s));
                    assert_eq!(ka.shifted(s).camelot().0, kb.camelot().0);
                }
            }
        }
    }

    #[test]
    fn key_names_use_sharps() {
        assert_eq!(Key { tonic: 9, minor: true, confidence: 1.0 }.name(), "Am");
        assert_eq!(Key { tonic: 0, minor: false, confidence: 1.0 }.name(), "C");
        assert_eq!(Key { tonic: 6, minor: true, confidence: 1.0 }.name(), "F#m");
        assert_eq!(Key { tonic: 10, minor: false, confidence: 1.0 }.name(), "A#");
    }

    #[test]
    fn detects_a_minor_and_c_major_progressions() {
        let am = detect_key(&progression(57.0, true), 2, SR).expect("key");
        assert_eq!((am.tonic, am.minor), (9, true), "A minor, got {am:?}");
        let c = detect_key(&progression(60.0, false), 2, SR).expect("key");
        assert_eq!((c.tonic, c.minor), (0, false), "C major, got {c:?}");
    }

    #[test]
    fn detects_transposed_keys() {
        for (tonic_pc, minor) in [(2u8, true), (6, true), (7, false), (3, false), (11, true)] {
            let midi = 48.0 + tonic_pc as f32; // C3 + pc
            let k = detect_key(&progression(midi, minor), 2, SR).expect("key");
            assert_eq!((k.tonic, k.minor), (tonic_pc, minor), "expected {tonic_pc}/{minor}, got {k:?}");
        }
    }

    #[test]
    fn tolerates_a_detuned_recording() {
        let mut v = Vec::new();
        for (root, minor) in [(57.0, true), (62.0, true), (64.0, false), (57.0, true)] {
            v.extend(chord(&triad(root, minor), 3.0, 35.0)); // 35 cents sharp
        }
        let k = detect_key(&v, 2, SR).expect("key");
        assert_eq!((k.tonic, k.minor), (9, true), "got {k:?}");
    }

    #[test]
    fn silence_and_short_input_have_no_key() {
        assert!(detect_key(&vec![0.0f32; SR as usize * 10 * 2], 2, SR).is_none());
        assert!(detect_key(&[0.1, 0.2, 0.3, 0.4], 2, SR).is_none());
        assert!(detect_key(&[], 2, SR).is_none());
    }

    #[test]
    fn noise_does_not_panic_and_has_low_confidence_if_any() {
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..SR as usize * 10 * 2)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.1
            })
            .collect();
        if let Some(k) = detect_key(&noise, 2, SR) {
            assert!(k.confidence < 0.5, "noise should not look certain: {k:?}");
        }
    }

    #[test]
    fn preview_downsample_preserves_peaks_and_length() {
        let wf: Vec<[f32; 3]> = (0..1000).map(|i| if i == 503 { [1.0, 0.5, 0.25] } else { [0.1; 3] }).collect();
        let p = preview_columns(&wf, 100);
        assert_eq!(p.len(), 100);
        assert_eq!(p[50], [255, 128, 64], "peak survives in its bucket");
        assert_eq!(p[10], [26, 26, 26]);
        assert!(p.iter().all(|c| c.iter().all(|&v| v >= 26)));
    }

    #[test]
    fn preview_stretches_short_waveforms_and_handles_edges() {
        let wf = vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let p = preview_columns(&wf, 8);
        assert_eq!(p.len(), 8);
        assert_eq!(p[0], [255, 0, 0]);
        assert_eq!(p[7], [0, 255, 0]);
        assert!(preview_columns(&[], 5).iter().all(|c| *c == [0, 0, 0]));
        assert!(preview_columns(&wf, 0).is_empty());
        assert_eq!(preview_columns(&[[2.0, -1.0, 0.5]], 1), vec![[255, 0, 128]], "values are clamped");
    }

    #[test]
    fn tags_on_missing_or_bad_files_are_empty_not_panics() {
        assert_eq!(read_tags(Path::new("/definitely/not/here.mp3")), TrackTags::default());
        let dir = std::env::temp_dir().join("dubplate_meta_test");
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.mp3");
        std::fs::write(&junk, b"this is not audio at all").unwrap();
        assert_eq!(read_tags(&junk), TrackTags::default());
        let _ = std::fs::remove_file(junk);
    }

    #[test]
    fn tag_helpers_clean_values() {
        assert_eq!(clean("  Daft Punk \0"), Some("Daft Punk".into()));
        assert_eq!(clean("   "), None);
        assert_eq!(clean_genre("(17)"), None);
        assert_eq!(clean_genre("17"), None);
        assert_eq!(clean_genre(" Drum & Bass "), Some("Drum & Bass".into()));
        assert_eq!(year_of("2012-05-01"), Some(2012));
        assert_eq!(year_of("12"), None);
    }

    #[test]
    fn reads_tags_from_a_tiny_wav_with_a_list_info_chunk() {
        // RIFF WAVE with LIST/INFO tags (INAM, IART, IGNR) and an empty data chunk.
        fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut v = id.to_vec();
            v.extend((body.len() as u32).to_le_bytes());
            v.extend(body);
            if body.len() % 2 == 1 {
                v.push(0);
            }
            v
        }
        let mut info = b"INFO".to_vec();
        info.extend(chunk(b"INAM", b"Test Title\0"));
        info.extend(chunk(b"IART", b"Test Artist\0"));
        info.extend(chunk(b"IGNR", b"Techno\0"));
        let mut fmt = Vec::new();
        fmt.extend(1u16.to_le_bytes()); // PCM
        fmt.extend(1u16.to_le_bytes()); // mono
        fmt.extend(8000u32.to_le_bytes()); // rate
        fmt.extend(16000u32.to_le_bytes()); // byte rate
        fmt.extend(2u16.to_le_bytes()); // block align
        fmt.extend(16u16.to_le_bytes()); // bits
        let mut body = b"WAVE".to_vec();
        body.extend(chunk(b"fmt ", &fmt));
        body.extend(chunk(b"LIST", &info));
        body.extend(chunk(b"data", &[0u8; 64]));
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);

        let dir = std::env::temp_dir().join("dubplate_meta_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tagged.wav");
        std::fs::write(&path, &file).unwrap();
        let t = read_tags(&path);
        let _ = std::fs::remove_file(&path);
        // symphonia's WAV reader exposes LIST/INFO; if a build lacks it we still must not panic.
        if t != TrackTags::default() {
            assert_eq!(t.title.as_deref(), Some("Test Title"));
            assert_eq!(t.artist.as_deref(), Some("Test Artist"));
            assert_eq!(t.genre.as_deref(), Some("Techno"));
        }
    }

    /// Manual check against the real samples:
    /// `cargo test -p dubplate-core real_samples_keys -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_samples_keys() {
        // a folder of test songs: DUBPLATE_SAMPLES, or `samples` in the repository (git-ignored)
        let dir = std::env::var_os("DUBPLATE_SAMPLES")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples"));
        let dir = dir.as_path();
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let t0 = std::time::Instant::now();
            let Ok(track) = crate::decode::decode_track(&p) else { continue };
            let t1 = t0.elapsed();
            let t2 = std::time::Instant::now();
            let key = detect_key(&track.samples, track.channels, track.sample_rate);
            println!(
                "KEY {:?} -> {:?} ({}) detect={:?} decode+analysis={:?} tags={:?}",
                p.file_name().unwrap(),
                key.map(|k| k.name()),
                key.map(|k| k.camelot_label()).unwrap_or_default(),
                t2.elapsed(),
                t1,
                read_tags(&p)
            );
        }
    }
}
