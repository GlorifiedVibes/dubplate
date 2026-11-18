use std::path::Path;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::deck::Cue;

/// Bump when analysis output changes so stale entries are re-analysed.
pub const CACHE_VERSION: u8 = 19;

/// Everything derived from a track file that is expensive to recompute.
/// Raw samples are NOT cached (too large; symphonia decodes quickly anyway).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    pub version: u8,
    /// `mtime` seconds of the audio file at analysis time — used for invalidation.
    pub file_mtime_secs: u64,
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_frames: u64,
    pub duration_secs: f64,
    pub title: String,
    pub bpm: f32,
    /// BPM × 100 (ANLZ PQTZ format).
    pub bpm_coarse: u32,
    pub beat_offset: f64,
    pub beat_times_ms: Vec<u32>,
    /// Detected musical key.
    pub key: Option<crate::meta::Key>,
    /// Spectral waveform chunks: [low, mid, high] normalised 0.0–1.0.
    pub waveform: Vec<[f32; 3]>,
    /// User cues/loops — populated from the library DB after loading.
    pub cues: Vec<Cue>,
    /// How sure the bar line is (the margin of the downbeat vote): under 1 the signs did not agree, and a separated
    /// drums stem is worth asking.
    pub downbeat_confidence: f32,
    /// Where the beat grid starts (waveform chunks), the origin the beats of the vote are counted from; the drums
    /// stem's note is counted from the same one.
    pub grid_phase_chunks: f64,
    /// How loud the track is (for auto gain), so a cached load does not measure it again: about a tenth of a
    /// second a track, on every load. `None` for a track too short or too quiet to measure.
    pub loudness: Option<crate::loudness::Loudness>,
}

/// What a separated drums stem said about the track's bar line: [`dubplate_engine`]'s `drums_sign`, four numbers, kept
/// when the stems themselves are not (a track in a Reel is separated in the background and only this is kept).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DrumsNote {
    /// The analysis version the sign was counted for: a different grid makes it meaningless.
    pub version: u8,
    pub sign: [f32; 4],
}

/// `~/.cache/dubplate/<sha256-of-path>.drums`.
pub fn drums_path(audio_path: &Path) -> Option<std::path::PathBuf> {
    Some(cache_dir()?.join(format!("{}.drums", path_hash(audio_path))))
}

/// The drums stem's note for this audio file, if there is a current one.
pub fn load_drums(audio_path: &Path) -> Option<DrumsNote> {
    let file = drums_path(audio_path)?;
    if file_mtime_secs(&file)? < file_mtime_secs(audio_path)? {
        return None;
    }
    let note: DrumsNote = postcard::from_bytes(&std::fs::read(file).ok()?).ok()?;
    (note.version == CACHE_VERSION).then_some(note)
}

pub fn save_drums(audio_path: &Path, sign: [f32; 4]) -> std::io::Result<()> {
    let file =
        drums_path(audio_path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home dir"))?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    let bytes = postcard::to_allocvec(&DrumsNote { version: CACHE_VERSION, sign })
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(file, bytes)
}

/// Forgets the analysis of a track (the entry and its ANLZ file) so the next load analyses it again, with whatever has
/// been learned since (a drums note). The waveform summary and the stems stay.
pub fn forget_analysis(audio_path: &Path) {
    for f in [cache_path(audio_path), anlz_dat_path(audio_path)].into_iter().flatten() {
        let _ = std::fs::remove_file(f);
    }
}

/// Returns `~/.cache/dubplate/<sha256-of-path>.bin`.
pub fn cache_path(audio_path: &Path) -> Option<std::path::PathBuf> {
    let dir = cache_dir()?;
    let hash = path_hash(audio_path);
    Some(dir.join(format!("{hash}.bin")))
}

/// Returns `~/.cache/dubplate/<sha256-of-path>.DAT` for ANLZ export.
pub fn anlz_dat_path(audio_path: &Path) -> Option<std::path::PathBuf> {
    let dir = cache_dir()?;
    let hash = path_hash(audio_path);
    Some(dir.join(format!("{hash}.DAT")))
}

/// Returns `~/.cache/dubplate/<sha256-of-path>.wave`: the summary the waveform display draws from. It is kept apart
/// from the analysis entry so that changing how the waveform looks does not make every track be analysed again.
pub fn wave_path(audio_path: &Path) -> Option<std::path::PathBuf> {
    Some(cache_dir()?.join(format!("{}.wave", path_hash(audio_path))))
}

/// The saved waveform summary, if there is a current one for this audio file.
pub fn load_wave(audio_path: &Path) -> Option<crate::waveform_data::WaveSummary> {
    let file = wave_path(audio_path)?;
    if file_mtime_secs(&file)? < file_mtime_secs(audio_path)? {
        return None;
    }
    let summary: crate::waveform_data::WaveSummary = postcard::from_bytes(&std::fs::read(file).ok()?).ok()?;
    (summary.version == crate::waveform_data::WAVE_VERSION).then_some(summary)
}

pub fn save_wave(audio_path: &Path, summary: &crate::waveform_data::WaveSummary) -> std::io::Result<()> {
    let file = wave_path(audio_path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home dir"))?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    let bytes = postcard::to_allocvec(summary).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(file, bytes)
}

/// Try to load a valid cache entry for `audio_path`.
/// Returns `None` if missing, corrupt, or stale (audio file newer than cache).
pub fn load(audio_path: &Path) -> Option<CacheEntry> {
    let cache = cache_path(audio_path)?;
    let audio_mtime = file_mtime_secs(audio_path)?;
    let cache_mtime = file_mtime_secs(&cache)?;
    if cache_mtime < audio_mtime {
        return None; // stale
    }
    let bytes = std::fs::read(&cache).ok()?;
    let entry: CacheEntry = postcard::from_bytes(&bytes).ok()?;
    if entry.version != CACHE_VERSION {
        return None; // version bump
    }
    log::debug!("cache hit: {}", audio_path.display());
    Some(entry)
}

/// Persist a cache entry to disk, creating the cache directory if needed.
pub fn save(audio_path: &Path, entry: &CacheEntry) -> std::io::Result<()> {
    let cache =
        cache_path(audio_path).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home dir"))?;
    std::fs::create_dir_all(cache.parent().unwrap())?;
    let bytes = postcard::to_allocvec(entry).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(cache, bytes)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn cache_dir() -> Option<std::path::PathBuf> {
    Some(crate::paths::cache_dir())
}

pub fn path_hash_hex(path: &Path) -> String {
    path_hash(path)
}

fn path_hash(path: &Path) -> String {
    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    let result = h.finalize();
    result.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn file_mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}
