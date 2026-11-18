//! Reads the beat grid (and the audio path) from a rekordbox analysis file, `ANLZ0000.DAT`: on a USB stick under
//! `PIONEER/USBANLZ/`, in rekordbox's own data folder (`share/PIONEER/USBANLZ`). Format notes:
//! <https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html>. Only what a grid import needs.

use crate::deck::BeatGrid;

/// One beat of rekordbox's grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RbBeat {
    /// 1..=4, the beat's place in its bar (1 = the downbeat).
    pub in_bar: u8,
    /// Tempo at this beat, BPM x 100.
    pub bpm100: u16,
    /// Position in the track, whole milliseconds.
    pub ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RekordboxGrid {
    /// Where rekordbox found the audio (on the machine or stick it was analysed on).
    pub path: String,
    pub beats: Vec<RbBeat>,
}

/// The tempo and downbeat that rekordbox's list of beats works out to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportedTempo {
    pub bpm: f64,
    /// A bar-1 beat on the fitted line, in seconds.
    pub downbeat_secs: f64,
    /// More than one tempo in the file (rekordbox's dynamic grid): only the longest steady stretch is used.
    pub dynamic: bool,
    pub beats_used: usize,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// Parses an `ANLZ0000.DAT`. `None` if it is not one or holds no beat grid.
pub fn read_dat(bytes: &[u8]) -> Option<RekordboxGrid> {
    if bytes.get(..4)? != b"PMAI" {
        return None;
    }
    let mut pos = be32(bytes, 4)? as usize;
    let (mut path, mut beats) = (String::new(), None);
    while pos + 12 <= bytes.len() {
        let tag = &bytes[pos..pos + 4];
        let (header, len) = (be32(bytes, pos + 4)? as usize, be32(bytes, pos + 8)? as usize);
        if len < 12 || pos + len > bytes.len() {
            break;
        }
        let body = &bytes[pos..pos + len];
        match tag {
            b"PPTH" => {
                let n = be32(body, 12)? as usize;
                let raw = body.get(16..16 + n)?;
                let units: Vec<u16> =
                    raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
                path = String::from_utf16_lossy(&units);
            }
            b"PQTZ" => {
                let n = be32(body, 20)? as usize;
                let mut v = Vec::with_capacity(n);
                for i in 0..n {
                    let at = 24 + i * 8;
                    v.push(RbBeat {
                        in_bar: be16(body, at)? as u8,
                        bpm100: be16(body, at + 2)?,
                        ms: be32(body, at + 4)?,
                    });
                }
                beats = Some(v);
            }
            _ => {}
        }
        let _ = header;
        pos += len;
    }
    let beats = beats.filter(|b| b.len() >= 2)?;
    Some(RekordboxGrid { path, beats })
}

impl RekordboxGrid {
    /// The audio file's name, from either kind of path (`/Users/me/a.mp3`, `C:\Music\a.mp3`).
    pub fn file_name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }

    /// Time of the last beat, seconds.
    pub fn last_secs(&self) -> f64 {
        self.beats.last().map_or(0.0, |b| b.ms as f64 / 1000.0)
    }

    /// Tempo and downbeat from the beat positions themselves. The tempo is a least-squares line through the beats
    /// (the stored tempo is rounded to 0.01 and the positions to a millisecond, so the line is the more exact of
    /// the two, and a true 124.0116 comes out as 124.0116).
    pub fn tempo(&self) -> Option<ImportedTempo> {
        // longest run of beats that share one tempo
        let (mut best, mut start) = ((0usize, 0usize), 0usize);
        for i in 1..=self.beats.len() {
            if i == self.beats.len() || self.beats[i].bpm100 != self.beats[start].bpm100 {
                if i - start > best.1 - best.0 {
                    best = (start, i);
                }
                start = i;
            }
        }
        let dynamic = best.1 - best.0 < self.beats.len();
        let run = if best.1 - best.0 >= 16 { &self.beats[best.0..best.1] } else { &self.beats[..] };
        let n = run.len() as f64;
        let (sx, sy): (f64, f64) =
            run.iter().enumerate().fold((0.0, 0.0), |a, (i, b)| (a.0 + i as f64, a.1 + b.ms as f64));
        let (mx, my) = (sx / n, sy / n);
        let (mut num, mut den) = (0.0, 0.0);
        for (i, b) in run.iter().enumerate() {
            num += (i as f64 - mx) * (b.ms as f64 - my);
            den += (i as f64 - mx).powi(2);
        }
        if den <= 0.0 || num <= 0.0 {
            return None;
        }
        let slope_ms = num / den;
        let at = |i: usize| (my + slope_ms * (i as f64 - mx)) / 1000.0;
        let first_bar = run.iter().position(|b| b.in_bar == 1).unwrap_or(0);
        Some(ImportedTempo { bpm: 60_000.0 / slope_ms, downbeat_secs: at(first_bar), dynamic, beats_used: run.len() })
    }

    /// A Dubplate grid from this one, moved `shift_secs` later (rekordbox and Dubplate can disagree about where the
    /// start of an MP3 is). The downbeat is kept at or after the start of the track by whole bars.
    pub fn to_grid(&self, shift_secs: f64, sample_rate: u32) -> Option<BeatGrid> {
        let t = self.tempo()?;
        let mut g = BeatGrid {
            bpm: t.bpm as f32,
            offset: (t.downbeat_secs + shift_secs) * sample_rate as f64 / crate::deck::WAVEFORM_CHUNK_FRAMES as f64,
        };
        let bar = 4.0 * g.period_chunks(sample_rate);
        if bar > 0.0 {
            g.offset = g.offset.rem_euclid(bar);
        }
        Some(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anlz::{build_dat, AnlzTrack};

    fn dat(path: &str, beats: &[u32], bpm: f32) -> Vec<u8> {
        build_dat(&AnlzTrack {
            audio_path: path,
            beat_times_ms: beats,
            bpm,
            cues: &[],
            waveform: &[[0.5, 0.5, 0.5]; 64],
            duration_ms: 200_000,
        })
    }

    #[test]
    fn reads_back_what_the_exporter_writes() {
        let beats: Vec<u32> = (0..200).map(|i| (300.0 + i as f64 * 483.9) as u32).collect();
        let g = read_dat(&dat("/Contents/Artist/Album/Song ü.mp3", &beats, 124.0)).unwrap();
        assert_eq!(g.path, "/Contents/Artist/Album/Song ü.mp3");
        assert_eq!(g.file_name(), "Song ü.mp3");
        assert_eq!(g.beats.len(), 200);
        assert_eq!((g.beats[0].in_bar, g.beats[1].in_bar, g.beats[4].in_bar), (1, 2, 1));
        assert_eq!(g.beats[3].ms, beats[3]);
    }

    #[test]
    fn the_line_is_more_exact_than_the_stored_tempo() {
        // true tempo 124.0116, positions rounded to whole ms, tempo stored as 124.01
        let p = 60_000.0 / 124.0116;
        let beats: Vec<u32> = (0..800).map(|i| (315.0 + i as f64 * p).round() as u32).collect();
        let t = read_dat(&dat("C:\\Music\\Hoping.mp3", &beats, 124.0116)).unwrap().tempo().unwrap();
        assert!((t.bpm - 124.0116).abs() < 0.0005, "{}", t.bpm);
        assert!((t.downbeat_secs - 0.315).abs() < 0.001);
        assert!(!t.dynamic);
        assert_eq!(read_dat(&dat("C:\\Music\\Hoping.mp3", &beats, 124.0)).unwrap().file_name(), "Hoping.mp3");
    }

    #[test]
    fn a_grid_with_two_tempos_is_flagged_and_uses_the_longer_one() {
        let mut g = read_dat(&dat("/a.mp3", &(0..100).map(|i| 500 + i * 500).collect::<Vec<u32>>(), 120.0)).unwrap();
        for b in g.beats.iter_mut().skip(70) {
            b.bpm100 = 12100;
        }
        let t = g.tempo().unwrap();
        assert!(t.dynamic);
        assert_eq!(t.beats_used, 70);
    }

    #[test]
    fn rubbish_is_refused() {
        assert!(read_dat(b"").is_none());
        assert!(read_dat(b"PMAI\0\0\0\x1c\0\0\0\0").is_none());
        assert!(read_dat(&[0u8; 100]).is_none());
        let mut ok = dat("/a.mp3", &[0, 500, 1000, 1500], 120.0);
        ok.truncate(60);
        assert!(read_dat(&ok).is_none());
    }

    #[test]
    fn the_grid_keeps_the_downbeat_and_moves_with_the_shift() {
        let beats: Vec<u32> = (0..64).map(|i| 1000 + i * 500).collect();
        let g = read_dat(&dat("/a.mp3", &beats, 120.0)).unwrap();
        let a = g.to_grid(0.0, 44100).unwrap();
        assert!((a.bpm - 120.0).abs() < 1e-3);
        assert!((a.offset * 256.0 / 44100.0 - 1.0).abs() < 0.001);
        let b = g.to_grid(-0.051, 44100).unwrap();
        assert!((b.offset * 256.0 / 44100.0 - 0.949).abs() < 0.001);
    }
}
