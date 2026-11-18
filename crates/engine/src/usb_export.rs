//! Exports reels to a USB stick (or any folder) that Pioneer CDJ / XDJ players can browse.
//!
//! The stick gets what rekordbox's "Device Library" export makes:
//!
//! ```text
//! Contents/<artist>/<album>/<file>              the audio, copied
//! PIONEER/rekordbox/export.pdb                   the database: tracks, artists, keys, playlists
//! PIONEER/USBANLZ/Pxxx/xxxxxxxx/ANLZ0000.DAT    beat grid, waveform, cues (all players)
//! PIONEER/USBANLZ/Pxxx/xxxxxxxx/ANLZ0000.EXT    detail waveform, coloured cues (newer players)
//! ```
//!
//! Players take MP3, AAC (M4A), WAV, AIFF and FLAC; other formats are left off and reported.
//! The writers are checked against rekordcrate, but no player has read their output yet, so treat
//! this as unproven on hardware.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::anlz::{self, AnlzTrack};
use crate::deck::Cue;
use crate::pdb::{self, ExportPlaylist, ExportTrack};

/// File types players can play.
pub const SUPPORTED: &[&str] = &["mp3", "m4a", "aac", "wav", "aif", "aiff", "flac"];

/// Left in `PIONEER/` so a later export knows the database is ours to replace.
const MARKER: &str = "PIONEER/dubplate-export.txt";

/// One track to export, with everything Dubplate knows about it.
#[derive(Clone, Debug)]
pub struct ExportItem {
    pub source: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    /// (tonic pitch class 0 = C, minor?)
    pub key: Option<(u8, bool)>,
    pub bpm: f32,
    pub year: u16,
    pub duration_secs: f64,
    pub sample_rate: u32,
    pub beat_times_ms: Vec<u32>,
    pub waveform: Vec<[f32; 3]>,
    pub cues: Vec<Cue>,
}

/// A reel and its tracks, in order.
#[derive(Clone, Debug)]
pub struct ExportReel {
    pub name: String,
    pub tracks: Vec<ExportItem>,
}

#[derive(Debug, Default)]
pub struct ExportReport {
    pub tracks: usize,
    pub copied: usize,
    /// Already on the stick with the same size.
    pub reused: usize,
    pub bytes_copied: u64,
    /// (file, why)
    pub skipped: Vec<(PathBuf, String)>,
}

#[derive(Debug)]
pub enum ExportError {
    /// The stick already has a library that Dubplate didn't write; the caller must confirm.
    WouldReplaceForeignLibrary,
    Io(io::Error),
}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        ExportError::Io(e)
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::WouldReplaceForeignLibrary => {
                write!(f, "this drive already holds a rekordbox library; exporting would replace it")
            }
            ExportError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// True when `dest` already carries a rekordbox library that wasn't written by Dubplate.
pub fn has_foreign_library(dest: &Path) -> bool {
    dest.join("PIONEER/rekordbox/export.pdb").exists() && !dest.join(MARKER).exists()
}

fn is_supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| SUPPORTED.contains(&e.to_ascii_lowercase().as_str()))
}

/// FAT32 and exFAT refuse `<>:"/\|?*`, control characters and trailing dots or spaces.
pub fn safe_name(s: &str) -> String {
    let cleaned: String =
        s.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim();
    let cleaned: String = cleaned.chars().take(60).collect();
    if cleaned.is_empty() {
        "Unknown".into()
    } else {
        cleaned
    }
}

fn stable_hash(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// Folder for a track's analysis files, relative to the stick, like `PIONEER/USBANLZ/P016/0000875E`.
fn anlz_dir(source: &Path, id: u32) -> String {
    let h = stable_hash(&source.to_string_lossy()) ^ id.wrapping_mul(0x9e37_79b1);
    format!("PIONEER/USBANLZ/P{:03X}/{:08X}", (h >> 20) & 0xfff, h)
}

fn date_today() -> String {
    // Days since the epoch to a civil date (Howard Hinnant's algorithm).
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", y + (m <= 2) as i64, m, d)
}

/// Copies `src` to `dst` unless a file of the same size is already there. Returns bytes copied.
fn copy_if_needed(src: &Path, dst: &Path) -> io::Result<Option<u64>> {
    let size = std::fs::metadata(src)?.len();
    if std::fs::metadata(dst).is_ok_and(|m| m.len() == size) {
        return Ok(None);
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write beside it first so a pulled stick never holds half a song under the real name.
    let part = dst.with_extension("part");
    std::fs::copy(src, &part)?;
    std::fs::rename(&part, dst)?;
    Ok(Some(size))
}

/// Writes the reels to `dest`. `progress(done, total, what)` is called before each track.
///
/// Fails with [`ExportError::WouldReplaceForeignLibrary`] when `dest` holds a library rekordbox wrote,
/// unless `replace_foreign` is set.
pub fn export_usb(
    dest: &Path,
    reels: &[ExportReel],
    replace_foreign: bool,
    progress: &mut dyn FnMut(usize, usize, &str),
) -> Result<ExportReport, ExportError> {
    if !dest.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a folder", dest.display())).into());
    }
    if has_foreign_library(dest) && !replace_foreign {
        return Err(ExportError::WouldReplaceForeignLibrary);
    }
    let mut report = ExportReport::default();

    // One id per distinct source file; playlists refer to them.
    let mut ids: HashMap<PathBuf, u32> = HashMap::new();
    let mut order: Vec<&ExportItem> = Vec::new();
    for reel in reels {
        for t in &reel.tracks {
            if ids.contains_key(&t.source) {
                continue;
            }
            if !is_supported(&t.source) {
                report.skipped.push((
                    t.source.clone(),
                    "players can't play this file type (use MP3, AAC, WAV, AIFF or FLAC)".into(),
                ));
                continue;
            }
            if !t.source.is_file() {
                report.skipped.push((t.source.clone(), "file not found".into()));
                continue;
            }
            ids.insert(t.source.clone(), order.len() as u32 + 1);
            order.push(t);
        }
    }

    let today = date_today();
    let mut rows: Vec<ExportTrack> = Vec::with_capacity(order.len());
    let mut used_names: HashMap<String, u32> = HashMap::new();
    for (i, t) in order.iter().enumerate() {
        let id = i as u32 + 1;
        progress(i, order.len(), &t.title);

        // Contents/<artist>/<album>/<file>, made unique if two tracks share a name.
        let file = t.source.file_name().and_then(|f| f.to_str()).unwrap_or("track");
        let rel = format!("Contents/{}/{}/{}", safe_name(&t.artist), safe_name(&t.album), safe_name(file));
        let n = used_names.entry(rel.to_lowercase()).or_insert(0);
        *n += 1;
        let rel = if *n > 1 {
            let (stem, ext) = rel.rsplit_once('.').unwrap_or((&rel, ""));
            format!("{stem} ({n}).{ext}")
        } else {
            rel
        };

        match copy_if_needed(&t.source, &dest.join(&rel)) {
            Ok(Some(bytes)) => {
                report.copied += 1;
                report.bytes_copied += bytes;
            }
            Ok(None) => report.reused += 1,
            Err(e) => {
                // Usually a full stick: stop rather than write a database that points at nothing.
                return Err(io::Error::new(e.kind(), format!("copying {}: {e}", t.source.display())).into());
            }
        }

        let file_size = std::fs::metadata(dest.join(&rel)).map_or(0, |m| m.len());
        let audio_path = format!("/{rel}");
        let dir = anlz_dir(&t.source, id);
        let duration_ms = (t.duration_secs * 1000.0) as u32;
        let a = AnlzTrack {
            audio_path: &audio_path,
            beat_times_ms: &t.beat_times_ms,
            bpm: t.bpm,
            cues: &t.cues,
            waveform: &t.waveform,
            duration_ms,
        };
        std::fs::create_dir_all(dest.join(&dir))?;
        std::fs::write(dest.join(&dir).join("ANLZ0000.DAT"), anlz::build_dat(&a))?;
        std::fs::write(dest.join(&dir).join("ANLZ0000.EXT"), anlz::build_ext(&a))?;

        let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let bitrate = match ext.as_str() {
            "wav" | "aif" | "aiff" => t.sample_rate * 16 * 2 / 1000,
            _ if t.duration_secs > 1.0 => (file_size as f64 * 8.0 / t.duration_secs / 1000.0).round() as u32,
            _ => 0,
        };
        rows.push(ExportTrack {
            id,
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            genre: t.genre.clone(),
            key: t.key.map(|(pc, minor)| pdb::key_name(pc, minor)),
            bpm: t.bpm,
            duration_secs: t.duration_secs.round() as u32,
            file_size: file_size.min(u32::MAX as u64) as u32,
            sample_rate: t.sample_rate,
            bitrate_kbps: bitrate,
            bit_depth: 16,
            year: t.year,
            track_number: 0,
            comment: String::new(),
            date_added: today.clone(),
            file_path: audio_path,
            analyze_path: format!("/{dir}/ANLZ0000.DAT"),
            analyze_date: today.clone(),
        });
    }
    report.tracks = rows.len();

    let playlists: Vec<ExportPlaylist> = reels
        .iter()
        .map(|r| ExportPlaylist {
            name: r.name.clone(),
            track_ids: r.tracks.iter().filter_map(|t| ids.get(&t.source).copied()).collect(),
        })
        .collect();

    std::fs::create_dir_all(dest.join("PIONEER/rekordbox"))?;
    std::fs::write(dest.join("PIONEER/rekordbox/export.pdb"), pdb::build_export_pdb(&rows, &playlists))?;
    std::fs::write(dest.join(MARKER), "Written by Dubplate. Exporting again replaces PIONEER/rekordbox/export.pdb.\n")?;
    progress(order.len(), order.len(), "done");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dubplate-usb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn item(dir: &Path, file: &str, title: &str, artist: &str) -> ExportItem {
        let p = dir.join(file);
        std::fs::write(&p, vec![7u8; 5000]).unwrap();
        ExportItem {
            source: p,
            title: title.into(),
            artist: artist.into(),
            album: "Album".into(),
            genre: "House".into(),
            key: Some((9, true)),
            bpm: 124.0,
            year: 2020,
            duration_secs: 200.0,
            sample_rate: 44100,
            beat_times_ms: (0..32).map(|i| i * 484).collect(),
            waveform: vec![[0.5, 0.3, 0.2]; 300],
            cues: vec![Cue {
                time_ms: 1000,
                out_time_ms: None,
                pos_frames: 0.0,
                out_frames: None,
                hot_cue: Some(1),
                label: "A".into(),
                color: [1, 2, 3],
            }],
        }
    }

    #[test]
    fn names_are_safe_on_fat() {
        assert_eq!(safe_name("AC/DC: Live?"), "AC_DC_ Live_");
        assert_eq!(safe_name("  ..  "), "Unknown");
        assert_eq!(safe_name("Song."), "Song");
        assert_eq!(safe_name(&"x".repeat(200)).len(), 60);
    }

    #[test]
    fn dates_are_civil() {
        let d = date_today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"), "{d}");
    }

    #[test]
    fn exports_audio_database_and_analysis() {
        let src = scratch("src");
        let usb = scratch("usb");
        let reels = vec![
            ExportReel {
                name: "Friday".into(),
                tracks: vec![item(&src, "a.mp3", "Song A", "Artist"), item(&src, "b.flac", "Song B", "Artist")],
            },
            ExportReel { name: "Warm".into(), tracks: vec![item(&src, "b.flac", "Song B", "Artist")] },
        ];
        let mut calls = 0;
        let r = export_usb(&usb, &reels, false, &mut |_, _, _| calls += 1).unwrap();
        assert_eq!((r.tracks, r.copied, r.reused), (2, 2, 0), "a track on two reels goes on once");
        assert!(calls >= 2);
        assert!(usb.join("Contents/Artist/Album/a.mp3").is_file());
        assert!(usb.join("PIONEER/rekordbox/export.pdb").is_file());
        assert!(usb.join(MARKER).is_file());

        // The database lists both tracks, and each analysis file the database points at exists.
        let pdb = std::fs::read(usb.join("PIONEER/rekordbox/export.pdb")).unwrap();
        assert_eq!(pdb.len() % 4096, 0);
        let text = String::from_utf8_lossy(&pdb);
        assert!(text.contains("Song A") && text.contains("Song B") && text.contains("Friday"));
        let dat = text.split("/PIONEER/USBANLZ/").nth(1).expect("an analysis path");
        let rel: String = format!("PIONEER/USBANLZ/{}", dat.split("ANLZ0000.DAT").next().unwrap());
        assert!(usb.join(&rel).join("ANLZ0000.DAT").is_file(), "{rel}");
        assert!(usb.join(&rel).join("ANLZ0000.EXT").is_file());

        // Exporting again copies nothing.
        let again = export_usb(&usb, &reels, false, &mut |_, _, _| {}).unwrap();
        assert_eq!((again.copied, again.reused), (0, 2));
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(usb);
    }

    #[test]
    fn unsupported_and_missing_files_are_reported_not_fatal() {
        let src = scratch("src2");
        let usb = scratch("usb2");
        let mut gone = item(&src, "x.mp3", "Gone", "A");
        std::fs::remove_file(&gone.source).unwrap();
        gone.title = "Gone".into();
        let reels = vec![ExportReel {
            name: "R".into(),
            tracks: vec![item(&src, "ok.wav", "Fine", "A"), item(&src, "song.ogg", "Ogg", "A"), gone],
        }];
        let r = export_usb(&usb, &reels, false, &mut |_, _, _| {}).unwrap();
        assert_eq!(r.tracks, 1);
        assert_eq!(r.skipped.len(), 2);
        assert!(r.skipped.iter().any(|(p, why)| p.ends_with("song.ogg") && why.contains("file type")));
        assert!(r.skipped.iter().any(|(_, why)| why.contains("not found")));
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(usb);
    }

    #[test]
    fn colliding_names_are_kept_apart() {
        let src = scratch("src3");
        let src2 = scratch("src3b");
        let usb = scratch("usb3");
        let a = item(&src, "same.mp3", "One", "A");
        let b = item(&src2, "same.mp3", "Two", "A");
        let reels = vec![ExportReel { name: "R".into(), tracks: vec![a, b] }];
        let r = export_usb(&usb, &reels, false, &mut |_, _, _| {}).unwrap();
        assert_eq!(r.copied, 2);
        assert!(usb.join("Contents/A/Album/same.mp3").is_file());
        assert!(usb.join("Contents/A/Album/same (2).mp3").is_file());
        for d in [src, src2, usb] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_rekordbox_library_is_not_replaced_without_consent() {
        let usb = scratch("usb4");
        let src = scratch("src4");
        std::fs::create_dir_all(usb.join("PIONEER/rekordbox")).unwrap();
        std::fs::write(usb.join("PIONEER/rekordbox/export.pdb"), b"theirs").unwrap();
        assert!(has_foreign_library(&usb));
        let reels = vec![ExportReel { name: "R".into(), tracks: vec![item(&src, "a.mp3", "A", "A")] }];
        assert!(matches!(
            export_usb(&usb, &reels, false, &mut |_, _, _| {}),
            Err(ExportError::WouldReplaceForeignLibrary)
        ));
        assert_eq!(std::fs::read(usb.join("PIONEER/rekordbox/export.pdb")).unwrap(), b"theirs", "untouched");
        assert!(!usb.join("Contents").exists(), "nothing was copied either");

        export_usb(&usb, &reels, true, &mut |_, _, _| {}).unwrap();
        assert!(!has_foreign_library(&usb), "now it's ours, so a re-export needs no consent");
        for d in [src, usb] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_missing_destination_is_an_error() {
        let r = export_usb(Path::new("/definitely/not/here"), &[], false, &mut |_, _, _| {});
        assert!(matches!(r, Err(ExportError::Io(_))));
    }
}
