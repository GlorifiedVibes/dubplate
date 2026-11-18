//! Pioneer ANLZ analysis files (`ANLZ0000.DAT` / `.EXT`): what a CDJ reads, besides `export.pdb`, to
//! show a track's beat grid, waveform and cues.
//!
//! Layout (all big-endian): a 28-byte `PMAI` file header, then tags. Every tag starts with its four
//! letter name, the length of its header and the length of the whole tag; the tag-specific header
//! fields and the body follow. Format reference:
//! <https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html>
//!
//! `.DAT` holds what every player understands: path, beat grid, the 400-column overview waveform, the
//! 100-column tiny waveform and the cues. `.EXT` adds what newer players use: the scrolling detail
//! waveform and cues with colours and comments.
//!
//! Not written (players cope without them): `PVBR` (a seek index for variable-bit-rate MP3), the colour
//! waveforms `PWV4`/`PWV5`, and `PSSI` song structure.

use std::io;
use std::path::Path;

use crate::deck::Cue;

/// Everything the files need to know about one track.
pub struct AnlzTrack<'a> {
    /// Path of the audio file on the stick, e.g. "/Contents/Artist/Album/Song.mp3".
    pub audio_path: &'a str,
    /// Time of every beat in milliseconds, starting with the first beat of the grid.
    pub beat_times_ms: &'a [u32],
    pub bpm: f32,
    pub cues: &'a [Cue],
    /// Spectral waveform columns [low, mid, high], each 0.0–1.0, covering the whole track.
    pub waveform: &'a [[f32; 3]],
    pub duration_ms: u32,
}

// ── byte helpers ─────────────────────────────────────────────────────────────

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// A tag: name, header length, total length, then `header` (the tag-specific header bytes) and `body`.
fn tag(name: &[u8; 4], header: &[u8], body: &[u8]) -> Vec<u8> {
    let len_header = 12 + header.len();
    let mut t = Vec::with_capacity(len_header + body.len());
    t.extend_from_slice(name);
    be32(&mut t, len_header as u32);
    be32(&mut t, (len_header + body.len()) as u32);
    t.extend_from_slice(header);
    t.extend_from_slice(body);
    t
}

fn file(tags: Vec<Vec<u8>>) -> Vec<u8> {
    let total: usize = 28 + tags.iter().map(Vec::len).sum::<usize>();
    let mut f = Vec::with_capacity(total);
    f.extend_from_slice(b"PMAI");
    be32(&mut f, 28);
    be32(&mut f, total as u32);
    be32(&mut f, 1);
    be32(&mut f, 0x0001_0000);
    be32(&mut f, 0x0001_0000);
    be32(&mut f, 0);
    for t in tags {
        f.extend(t);
    }
    f
}

fn utf16_be_nul(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() * 2 + 2);
    for u in s.encode_utf16() {
        be16(&mut v, u);
    }
    be16(&mut v, 0);
    v
}

// ── tags ─────────────────────────────────────────────────────────────────────

/// PPTH: where the audio file is, UTF-16BE with a terminator.
fn path_tag(path: &str) -> Vec<u8> {
    let body = utf16_be_nul(path);
    let mut h = Vec::new();
    be32(&mut h, body.len() as u32);
    tag(b"PPTH", &h, &body)
}

/// PQTZ: the beat grid. Each entry: beat in bar (1–4), tempo ×100, time in ms.
fn beat_grid_tag(beats_ms: &[u32], bpm: f32) -> Vec<u8> {
    let tempo = (bpm * 100.0).round().clamp(0.0, 65535.0) as u16;
    let mut h = Vec::new();
    be32(&mut h, 0);
    be32(&mut h, 0x0008_0000);
    be32(&mut h, beats_ms.len() as u32);
    let mut body = Vec::with_capacity(beats_ms.len() * 8);
    for (i, &ms) in beats_ms.iter().enumerate() {
        be16(&mut body, (i % 4) as u16 + 1);
        be16(&mut body, tempo);
        be32(&mut body, ms);
    }
    tag(b"PQTZ", &h, &body)
}

/// The waveform sampled at `fraction` (0–1) of the way through the track.
fn column(wave: &[[f32; 3]], fraction: f32) -> [f32; 3] {
    if wave.is_empty() {
        return [0.0; 3];
    }
    wave[((fraction * wave.len() as f32) as usize).min(wave.len() - 1)]
}

/// Height 0–1 from the loudest band; whiteness 0–1 from how much of it is treble.
fn height_and_white(c: [f32; 3]) -> (f32, f32) {
    let peak = c[0].max(c[1]).max(c[2]).clamp(0.0, 1.0);
    let white = if peak > 0.0 { (c[2] / peak).clamp(0.0, 1.0) } else { 0.0 };
    (peak, white)
}

/// PWAV: the 400-column overview. One byte per column: height in bits 0–4, whiteness in bits 5–7.
fn overview_tag(wave: &[[f32; 3]]) -> Vec<u8> {
    let cols = 400;
    let body: Vec<u8> = (0..cols)
        .map(|i| {
            let (h, w) = height_and_white(column(wave, (i as f32 + 0.5) / cols as f32));
            ((h * 31.0).round() as u8) | (((w * 7.0).round() as u8) << 5)
        })
        .collect();
    let mut h = Vec::new();
    be32(&mut h, cols as u32);
    be32(&mut h, 0x0001_0000);
    tag(b"PWAV", &h, &body)
}

/// PWV2: the 100-column tiny overview, a height of 0–15 in the low nibble.
fn tiny_tag(wave: &[[f32; 3]]) -> Vec<u8> {
    let cols = 100;
    let body: Vec<u8> = (0..cols)
        .map(|i| {
            let (h, _) = height_and_white(column(wave, (i as f32 + 0.5) / cols as f32));
            (h * 15.0).round() as u8
        })
        .collect();
    let mut h = Vec::new();
    be32(&mut h, cols as u32);
    be32(&mut h, 0x0001_0000);
    tag(b"PWV2", &h, &body)
}

/// PWV3: the detail waveform, 150 columns per second, same byte as the overview.
fn detail_tag(wave: &[[f32; 3]], duration_ms: u32) -> Vec<u8> {
    let cols = (duration_ms as u64 * 150 / 1000) as usize;
    let body: Vec<u8> = (0..cols)
        .map(|i| {
            let (h, w) = height_and_white(column(wave, (i as f32 + 0.5) / cols.max(1) as f32));
            ((h * 31.0).round() as u8) | (((w * 7.0).round() as u8) << 5)
        })
        .collect();
    let mut h = Vec::new();
    be32(&mut h, 1);
    be32(&mut h, cols as u32);
    be32(&mut h, 0x0096_0000);
    tag(b"PWV3", &h, &body)
}

/// Hot cues (pads 1–8) or memory cues, in time order.
fn cues_of(cues: &[Cue], hot: bool) -> Vec<&Cue> {
    let mut v: Vec<&Cue> = cues.iter().filter(|c| c.hot_cue.is_some() == hot).collect();
    v.sort_by_key(|c| (c.hot_cue.unwrap_or(0), c.time_ms));
    v
}

/// PCPT: one cue in a PCOB list (56 bytes).
fn cue_entry(c: &Cue, index: usize, count: usize) -> Vec<u8> {
    let mut e = Vec::with_capacity(56);
    e.extend_from_slice(b"PCPT");
    be32(&mut e, 28);
    be32(&mut e, 56);
    be32(&mut e, c.hot_cue.map_or(0, u32::from));
    be32(&mut e, 0); // status: 0 = ordinary; 4 marks the loop that is active
    be32(&mut e, 0x0010_0000);
    be16(&mut e, if index == 0 { 0xffff } else { (index - 1) as u16 });
    be16(&mut e, if index + 1 == count { 0xffff } else { (index + 1) as u16 });
    e.push(if c.out_time_ms.is_some() { 2 } else { 1 });
    e.push(0);
    be16(&mut e, 1000);
    be32(&mut e, c.time_ms);
    be32(&mut e, c.out_time_ms.unwrap_or(0xffff_ffff));
    while e.len() < 56 {
        e.push(0);
    }
    e
}

/// PCOB: a list of cues; type 1 = hot cues, 0 = memory cues.
fn cue_list_tag(cues: &[Cue], hot: bool) -> Vec<u8> {
    let list = cues_of(cues, hot);
    let mut h = Vec::new();
    be32(&mut h, hot as u32);
    be16(&mut h, 0);
    be16(&mut h, list.len() as u16);
    be32(&mut h, if list.is_empty() { 0xffff_ffff } else { list.len() as u32 });
    let body: Vec<u8> = list.iter().enumerate().flat_map(|(i, c)| cue_entry(c, i, list.len())).collect();
    tag(b"PCOB", &h, &body)
}

/// PCP2: one cue in a PCO2 list, with its colour and comment.
fn ext_cue_entry(c: &Cue) -> Vec<u8> {
    let comment = utf16_be_nul(&c.label);
    let mut e = Vec::new();
    e.extend_from_slice(b"PCP2");
    be32(&mut e, 16);
    be32(&mut e, 0); // total length, patched below
    be32(&mut e, c.hot_cue.map_or(0, u32::from));
    e.push(if c.out_time_ms.is_some() { 2 } else { 1 });
    e.push(0);
    be16(&mut e, 1000);
    be32(&mut e, c.time_ms);
    be32(&mut e, c.out_time_ms.unwrap_or(0xffff_ffff));
    e.push(0); // colour table entry: none
    e.push(0);
    be16(&mut e, 0);
    be32(&mut e, 0);
    be16(&mut e, 0); // loop numerator
    be16(&mut e, 0); // loop denominator
    be32(&mut e, comment.len() as u32);
    e.extend(comment);
    e.push(0); // hot cue colour index
    e.extend_from_slice(&c.color);
    e.extend_from_slice(&[0u8; 20]);
    let len = e.len() as u32;
    e[8..12].copy_from_slice(&len.to_be_bytes());
    e
}

/// PCO2: the extended cue list.
fn ext_cue_list_tag(cues: &[Cue], hot: bool) -> Vec<u8> {
    let list = cues_of(cues, hot);
    let mut h = Vec::new();
    be32(&mut h, hot as u32);
    be16(&mut h, list.len() as u16);
    be16(&mut h, 0);
    let body: Vec<u8> = list.iter().flat_map(|c| ext_cue_entry(c)).collect();
    tag(b"PCO2", &h, &body)
}

// ── files ────────────────────────────────────────────────────────────────────

/// `ANLZ0000.DAT`
pub fn build_dat(t: &AnlzTrack) -> Vec<u8> {
    file(vec![
        path_tag(t.audio_path),
        beat_grid_tag(t.beat_times_ms, t.bpm),
        overview_tag(t.waveform),
        tiny_tag(t.waveform),
        cue_list_tag(t.cues, true),
        cue_list_tag(t.cues, false),
    ])
}

/// `ANLZ0000.EXT`
pub fn build_ext(t: &AnlzTrack) -> Vec<u8> {
    file(vec![
        path_tag(t.audio_path),
        detail_tag(t.waveform, t.duration_ms),
        cue_list_tag(t.cues, true),
        cue_list_tag(t.cues, false),
        ext_cue_list_tag(t.cues, true),
        ext_cue_list_tag(t.cues, false),
    ])
}

/// Writes a `.DAT` for the analysis cache (no stick involved, so no path).
pub fn write_dat(
    path: &Path,
    beat_times_ms: &[u32],
    bpm_coarse: u32,
    cues: &[Cue],
    waveform: &[[f32; 3]],
) -> io::Result<()> {
    let bytes = build_dat(&AnlzTrack {
        audio_path: "",
        beat_times_ms,
        bpm: bpm_coarse as f32 / 100.0,
        cues,
        waveform,
        duration_ms: beat_times_ms.last().copied().unwrap_or(0),
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::BinRead;
    use rekordcrate::anlz::{Content, ANLZ};
    use std::io::Cursor;

    fn cue(hot: Option<u8>, t: u32, out: Option<u32>, label: &str) -> Cue {
        Cue {
            time_ms: t,
            out_time_ms: out,
            pos_frames: 0.0,
            out_frames: None,
            hot_cue: hot,
            label: label.into(),
            color: [10, 200, 30],
        }
    }

    fn sample<'a>(beats: &'a [u32], cues: &'a [Cue], wave: &'a [[f32; 3]]) -> AnlzTrack<'a> {
        AnlzTrack {
            audio_path: "/Contents/Artist/Album/Song ü.mp3",
            beat_times_ms: beats,
            bpm: 124.5,
            cues,
            waveform: wave,
            duration_ms: 240_000,
        }
    }

    fn wave() -> Vec<[f32; 3]> {
        (0..2000)
            .map(|i| {
                let x = (i as f32 * 0.01).sin().abs();
                [x, x * 0.6, x * 0.3]
            })
            .collect()
    }

    fn parse(bytes: &[u8]) -> ANLZ {
        ANLZ::read(&mut Cursor::new(bytes)).expect("rekordcrate can read it")
    }

    #[test]
    fn dat_reads_back_with_rekordcrate() {
        let beats: Vec<u32> = (0..64).map(|i| 250 + i * 482).collect();
        let w = wave();
        let bytes = build_dat(&sample(&beats, &[], &w));
        let a = parse(&bytes);
        let kinds: Vec<&str> = a
            .sections
            .iter()
            .map(|s| match &s.content {
                Content::Path(_) => "path",
                Content::BeatGrid(_) => "grid",
                Content::WaveformPreview(_) => "wave",
                Content::TinyWaveformPreview(_) => "tiny",
                Content::CueList(_) => "cues",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["path", "grid", "wave", "tiny", "cues", "cues"]);
        for s in &a.sections {
            match &s.content {
                Content::Path(p) => assert_eq!(p.path.to_string(), "/Contents/Artist/Album/Song ü.mp3"),
                Content::BeatGrid(g) => {
                    assert_eq!(g.beats.len(), 64);
                    assert_eq!((g.beats[0].beat_number, g.beats[4].beat_number), (1, 1));
                    assert_eq!(g.beats[1].beat_number, 2);
                    assert_eq!(g.beats[0].tempo, 12450);
                    assert_eq!(g.beats[3].time, 250 + 3 * 482);
                }
                Content::WaveformPreview(w) => assert_eq!(w.data.len(), 400),
                Content::TinyWaveformPreview(w) => assert_eq!(w.data.len(), 100),
                _ => {}
            }
        }
    }

    #[test]
    fn file_header_and_tag_lengths_are_consistent() {
        let w = wave();
        let bytes = build_dat(&sample(&[500, 1000], &[], &w));
        assert_eq!(&bytes[..4], b"PMAI");
        assert_eq!(u32::from_be_bytes(bytes[4..8].try_into().unwrap()), 28);
        assert_eq!(u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize, bytes.len());
        // Walk the tags: each one ends where the next begins, the last at the end of the file.
        let mut at = 28;
        while at < bytes.len() {
            let total = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
            let head = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            assert!(head >= 12 && head <= total, "{:?}", &bytes[at..at + 4]);
            at += total;
        }
        assert_eq!(at, bytes.len());
    }

    #[test]
    fn cues_are_written_as_hot_and_memory_lists() {
        let cues = vec![
            cue(Some(2), 30_000, None, "drop"),
            cue(Some(1), 1_000, None, ""),
            cue(None, 60_000, Some(64_000), "loop"),
        ];
        let w = wave();
        let bytes = build_dat(&sample(&[500], &cues, &w));
        let find = |name: &[u8; 4], nth: usize| -> usize {
            let mut at = 28;
            let mut seen = 0;
            while at < bytes.len() {
                if &bytes[at..at + 4] == name {
                    if seen == nth {
                        return at;
                    }
                    seen += 1;
                }
                at += u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
            }
            panic!("no {name:?}");
        };
        // First list: hot cues, sorted by pad number.
        let hot = find(b"PCOB", 0);
        assert_eq!(u32::from_be_bytes(bytes[hot + 12..hot + 16].try_into().unwrap()), 1, "type = hot cues");
        assert_eq!(u16::from_be_bytes(bytes[hot + 18..hot + 20].try_into().unwrap()), 2);
        let first = hot + 24;
        assert_eq!(&bytes[first..first + 4], b"PCPT");
        assert_eq!(u32::from_be_bytes(bytes[first + 12..first + 16].try_into().unwrap()), 1, "pad 1 first");
        assert_eq!(u32::from_be_bytes(bytes[first + 32..first + 36].try_into().unwrap()), 1_000);
        assert_eq!(bytes[first + 28], 1, "point cue");
        // Order links: first cue 0xffff/1, last 0/0xffff.
        assert_eq!(u16::from_be_bytes(bytes[first + 24..first + 26].try_into().unwrap()), 0xffff);
        assert_eq!(u16::from_be_bytes(bytes[first + 26..first + 28].try_into().unwrap()), 1);
        let second = first + 56;
        assert_eq!(u16::from_be_bytes(bytes[second + 26..second + 28].try_into().unwrap()), 0xffff);
        // Second list: memory cues, with the loop.
        let mem = find(b"PCOB", 1);
        assert_eq!(u32::from_be_bytes(bytes[mem + 12..mem + 16].try_into().unwrap()), 0);
        let e = mem + 24;
        assert_eq!(u32::from_be_bytes(bytes[e + 12..e + 16].try_into().unwrap()), 0, "memory cue has no pad");
        assert_eq!(bytes[e + 28], 2, "loop");
        assert_eq!(u32::from_be_bytes(bytes[e + 36..e + 40].try_into().unwrap()), 64_000);
    }

    #[test]
    fn ext_reads_back_and_carries_colours_and_comments() {
        // Loops, because rekordcrate reads a plain cue's type byte as 0 where Deep Symmetry's
        // documentation (and our writer) has 1; the point-cue bytes are checked by hand above.
        let cues = vec![cue(Some(1), 1_000, Some(3_000), "Intro ü"), cue(None, 5_000, Some(6_000), "")];
        let w = wave();
        let bytes = build_ext(&sample(&[500], &cues, &w));
        let a = parse(&bytes);
        let mut detail = 0;
        let mut ext_cues = Vec::new();
        for s in &a.sections {
            match &s.content {
                Content::WaveformDetail(d) => detail = d.data.len(),
                Content::ExtendedCueList(l) => ext_cues.push(l.cues.len()),
                _ => {}
            }
        }
        assert_eq!(detail, 240 * 150, "150 columns per second");
        assert_eq!(ext_cues, [1, 1]);
        let ext = a
            .sections
            .iter()
            .find_map(|s| match &s.content {
                Content::ExtendedCueList(l) if !l.cues.is_empty() => Some(l),
                _ => None,
            })
            .unwrap();
        assert_eq!(ext.cues[0].comment.to_string(), "Intro ü");
        assert_eq!(ext.cues[0].hot_cue_color_rgb, (10, 200, 30));
    }

    #[test]
    fn silence_and_missing_waveforms_are_fine() {
        let bytes = build_dat(&sample(&[], &[], &[]));
        parse(&bytes);
        let bytes = build_ext(&AnlzTrack { duration_ms: 0, ..sample(&[], &[], &[]) });
        parse(&bytes);
    }

    #[test]
    fn overview_columns_use_five_bits_of_height_and_three_of_whiteness() {
        let loud_bright = [[1.0, 0.2, 1.0]; 10];
        let bytes = build_dat(&sample(&[], &[], &loud_bright));
        let at = bytes.windows(4).position(|w| w == b"PWAV").unwrap();
        let col = bytes[at + 20];
        assert_eq!(col & 0x1f, 31);
        assert_eq!(col >> 5, 7);
        let quiet = [[0.0f32; 3]; 10];
        let bytes = build_dat(&sample(&[], &[], &quiet));
        let at = bytes.windows(4).position(|w| w == b"PWAV").unwrap();
        assert_eq!(bytes[at + 20], 0);
    }
}
