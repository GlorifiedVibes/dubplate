use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicU32, Arc};
use std::time::UNIX_EPOCH;

use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions,
    probe::Hint,
};

/// 4-stem separation matching rekordbox's stem categories.
/// Each stem is interleaved stereo f32 at the original track sample rate.
pub struct TrackStems {
    pub drums: Vec<f32>,
    pub bass: Vec<f32>,
    pub other: Vec<f32>,
    pub vocals: Vec<f32>,
    pub sample_rate: u32,
}

/// A waveform summary of each stem, in the order of [`STEM_NAMES`], for drawing a lane per stem.
#[derive(Clone, Debug, Default)]
pub struct StemWaves(pub [crate::waveform_data::WaveSummary; 4]);

impl StemWaves {
    /// Summarises the four stems, one after the other, envelope only (the lanes are one colour each, so the bass /
    /// mid / high split that the track's own summary makes is left out). That is a few hundred milliseconds of one
    /// thread for a track, where the full summary of each stem on four threads was seconds of CPU.
    pub fn build(stems: &TrackStems) -> Self {
        let sr = stems.sample_rate;
        let all = [&stems.drums, &stems.bass, &stems.other, &stems.vocals];
        // a pause between the stems, so whatever is playing is never starved for long by this background work
        StemWaves(all.map(|buf| {
            let w = crate::waveform_data::WaveSummary::build_envelope(buf, 2, sr);
            std::thread::sleep(std::time::Duration::from_millis(15));
            w
        }))
    }
}

pub const STEM_NAMES: [&str; 4] = ["Drums", "Bass", "Other", "Vocals"];
pub const STEM_SHORTS: [&str; 4] = ["D", "B", "O", "V"];

impl TrackStems {
    /// Get interpolated (L, R) for a stem at a fractional frame position.
    #[inline]
    pub fn frame_interp(&self, stem: usize, frac_pos: f64) -> (f32, f32) {
        let buf = match stem {
            0 => &self.drums,
            1 => &self.bass,
            2 => &self.other,
            _ => &self.vocals,
        };
        let frame_i = frac_pos as usize;
        let t = (frac_pos - frame_i as f64) as f32;
        let i0 = frame_i * 2;
        let s0l = buf.get(i0).copied().unwrap_or(0.0);
        let s0r = buf.get(i0 + 1).copied().unwrap_or(s0l);
        let s1l = buf.get(i0 + 2).copied().unwrap_or(s0l);
        let s1r = buf.get(i0 + 3).copied().unwrap_or(s0r);
        (s0l + (s1l - s0l) * t, s0r + (s1r - s0r) * t)
    }
}

// ── Stem cache ────────────────────────────────────────────────────────────────
// Binary format: magic(4) + version(4) + sample_rate(4) + n_frames(8) + [4 stems × raw f32 LE]
// n_frames = number of stereo frames = stem Vec length / 2.
//
// Version history — bump it whenever the separation maths changes, so stale stems are recomputed:
//   1  per-segment RMS scaling, DC offset left in (loud tracks lost vocals and misplaced bass)
//   2  demucs-style whole-track mean/std normalisation, DC removed (see stem_pipeline)

const STEMS_MAGIC: &[u8; 4] = b"FSTM";
const STEMS_VERSION: u32 = 2;

/// `<cache dir>/<sha256>.stems` (`~/.cache/dubplate` on Linux)
pub fn stems_cache_path(audio_path: &Path) -> Option<PathBuf> {
    let dir = crate::paths::cache_dir();
    let hash = crate::cache::path_hash_hex(audio_path);
    Some(dir.join(format!("{hash}.stems")))
}

/// `DUBPLATE_NO_CACHE=1` neither reads nor writes cached stems (for timing and diagnostics).
fn cache_disabled() -> bool {
    std::env::var_os("DUBPLATE_NO_CACHE").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn save_stems_cache(audio_path: &Path, stems: &TrackStems) {
    if cache_disabled() {
        return;
    }
    let Some(path) = stems_cache_path(audio_path) else { return };
    let n_frames = (stems.drums.len() / 2) as u64;
    let write = || -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = BufWriter::new(std::fs::File::create(&path)?);
        w.write_all(STEMS_MAGIC)?;
        w.write_all(&STEMS_VERSION.to_le_bytes())?;
        w.write_all(&stems.sample_rate.to_le_bytes())?;
        w.write_all(&n_frames.to_le_bytes())?;
        for stem in [&stems.drums, &stems.bass, &stems.other, &stems.vocals] {
            for &v in stem {
                w.write_all(&v.to_le_bytes())?;
            }
        }
        Ok(())
    };
    match write() {
        Ok(()) => log::info!("stems cached → {}", path.display()),
        Err(e) => log::warn!("stems cache write failed: {e}"),
    }
}

pub fn load_stems_cache(audio_path: &Path) -> Option<Arc<TrackStems>> {
    if cache_disabled() {
        return None;
    }
    let path = stems_cache_path(audio_path)?;
    let audio_mtime = file_mtime_secs(audio_path)?;
    let cache_mtime = file_mtime_secs(&path)?;
    if cache_mtime < audio_mtime {
        return None;
    }

    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() < 24 {
        return None;
    }
    if &bytes[0..4] != STEMS_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    if version != STEMS_VERSION {
        return None;
    }
    let sample_rate = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let n_frames = u64::from_le_bytes(bytes[12..20].try_into().ok()?) as usize;

    let expected = 20 + 4 * n_frames * 2 * 4; // 4 stems × 2ch × n_frames × f32
    if bytes.len() < expected {
        return None;
    }

    let mut offset = 20usize;
    let mut read_stem = || -> Vec<f32> {
        let len = n_frames * 2;
        let v: Vec<f32> = bytes[offset..offset + len * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        offset += len * 4;
        v
    };
    let drums = read_stem();
    let bass = read_stem();
    let other = read_stem();
    let vocals = read_stem();

    log::info!("stems cache hit: {}", audio_path.display());
    Some(Arc::new(TrackStems { drums, bass, other, vocals, sample_rate }))
}

fn file_mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Returns `true` if a usable `demucs` CLI is found on PATH.
pub fn is_demucs_available() -> bool {
    std::process::Command::new("demucs").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// The ONNX model the apps download (`scripts/export_htdemucs_onnx_dml.py`), and the earlier third-party export.
pub const MODEL_FILE: &str = "htdemucs-dml.onnx";
pub const OLD_MODEL_FILE: &str = "htdemucs.onnx";

/// Path of the model the preferred stem engine uses
/// (`htdemucs.onnx` for ONNX Runtime, `htdemucs.pt` for libtorch).
pub fn model_path() -> PathBuf {
    #[cfg(feature = "onnx-inference")]
    return crate::ort_stems::model_path();
    #[cfg(all(not(feature = "onnx-inference"), feature = "torch-inference"))]
    return crate::tch_stems::model_path();
    #[cfg(not(any(feature = "onnx-inference", feature = "torch-inference")))]
    {
        crate::paths::data_dir().join("models").join(MODEL_FILE)
    }
}

/// Returns `true` if a model file for a compiled-in neural engine is present.
pub fn is_model_present() -> bool {
    #[cfg(feature = "onnx-inference")]
    if crate::ort_stems::is_model_present() {
        return true;
    }
    #[cfg(feature = "torch-inference")]
    if crate::tch_stems::is_model_present() {
        return true;
    }
    false
}

/// Returns `true` if any stem engine looks usable (ONNX model + runtime, TorchScript model, or demucs CLI).
pub fn is_stem_engine_available() -> bool {
    stem_engine_description().is_some()
}

/// One line saying which engine will separate stems, or `None` when there isn't one.
pub fn stem_engine_description() -> Option<String> {
    #[cfg(feature = "onnx-inference")]
    if crate::ort_stems::is_model_present() && crate::ort_stems::is_runtime_available() {
        return Some(format!("ONNX Runtime ({})", crate::ort_stems::model_path().display()));
    }
    #[cfg(feature = "torch-inference")]
    if crate::tch_stems::is_model_present() {
        return Some(format!("TorchScript ({})", crate::tch_stems::model_path().display()));
    }
    if is_demucs_available() {
        return Some("demucs command-line tool".into());
    }
    None
}

/// Why no stem engine is available, in words for the user.
pub fn stem_engine_help() -> String {
    #[cfg(feature = "onnx-inference")]
    {
        let model = crate::ort_stems::is_model_present();
        let runtime = crate::ort_stems::is_runtime_available();
        let fix = if cfg!(any(target_os = "windows", target_os = "macos")) {
            "Run scripts\\setup_onnx.ps1 (Windows) or scripts/setup_onnx.sh (macOS)"
        } else {
            "Run scripts/setup_onnx.sh"
        };
        match (model, runtime) {
            (false, false) => format!("Stem separation isn't set up. {fix} to install the model and ONNX Runtime."),
            (false, true) => {
                format!("The stem model is missing ({}). {fix}.", crate::ort_stems::model_path().display())
            }
            (true, false) => format!(
                "ONNX Runtime isn't installed (looked in {}). {fix}.",
                crate::ort_stems::runtime_dir().display()
            ),
            (true, true) => "Stem separation is ready.".into(),
        }
    }
    #[cfg(not(feature = "onnx-inference"))]
    "Stem separation needs the htdemucs model (scripts/setup_model.sh) or the demucs command-line tool.".into()
}

/// What the last separation in this process ran on: "CUDA", "DirectML", "CoreML" or "CPU" (None if it hasn't run one,
/// or the ONNX engine isn't built in).
pub fn stem_device() -> Option<&'static str> {
    #[cfg(feature = "onnx-inference")]
    {
        crate::ort_stems::active_device()
    }
    #[cfg(not(feature = "onnx-inference"))]
    {
        None
    }
}

/// Separate stems using the best available engine.
///
/// Order: ONNX Runtime, then libtorch (if built in), then the `demucs` CLI. Results are cached on disk.
pub fn separate(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    audio_path: &Path,
    progress: Option<&AtomicU32>,
) -> Result<Arc<TrackStems>, StemError> {
    separate_cancellable(samples, channels, sample_rate, audio_path, progress, None)
}

/// [`separate_cancellable`] for a background job that keeps only a note of what the stems say: the finished stems are
/// not written to the cache (each is 200-600 MB), but ones that are already there are used.
pub fn separate_uncached(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    audio_path: &Path,
    progress: Option<&AtomicU32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Arc<TrackStems>, StemError> {
    if let Some(cached) = load_stems_cache(audio_path) {
        return Ok(cached);
    }
    infer_stems(samples, channels, sample_rate, audio_path, progress, cancel)
}

/// True for the error [`separate_cancellable`] returns when `cancel` was set: nothing went wrong, it was asked to stop.
pub fn is_cancelled(e: &StemError) -> bool {
    e.0 == crate::stem_pipeline::CANCELLED
}

/// [`separate`], stopping with a cancelled error (see [`is_cancelled`]) between two segments of the track once
/// `cancel` is set. The ONNX and libtorch engines honour it; the `demucs` command-line fallback runs to the end.
pub fn separate_cancellable(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    audio_path: &Path,
    progress: Option<&AtomicU32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Arc<TrackStems>, StemError> {
    // Fast path: return cached stems if present and fresh.
    if let Some(cached) = load_stems_cache(audio_path) {
        if let Some(p) = progress {
            p.store(100, std::sync::atomic::Ordering::Relaxed);
        }
        return Ok(cached);
    }

    let result = infer_stems(samples, channels, sample_rate, audio_path, progress, cancel)?;

    // Persist for next time.  Failure is non-fatal.
    save_stems_cache(audio_path, &result);

    Ok(result)
}

#[cfg_attr(not(any(feature = "onnx-inference", feature = "torch-inference")), allow(unused_variables))]
fn infer_stems(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    audio_path: &Path,
    progress: Option<&AtomicU32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Arc<TrackStems>, StemError> {
    #[cfg(feature = "onnx-inference")]
    {
        let model = crate::ort_stems::model_path();
        if model.is_file() && crate::ort_stems::is_runtime_available() {
            log::info!("ONNX inference: {}", model.display());
            return crate::ort_stems::separate_onnx(samples, sample_rate, channels, &model, progress, cancel);
        }
    }
    #[cfg(feature = "torch-inference")]
    {
        let model = crate::tch_stems::model_path();
        if model.exists() {
            log::info!("TorchScript inference: {}", model.display());
            return crate::tch_stems::separate_tch(samples, sample_rate, channels, &model, progress, cancel);
        }
    }
    if !is_demucs_available() {
        return Err(StemError(stem_engine_help()));
    }
    log::info!("falling back to the demucs command-line tool");
    separate_stems(audio_path)
}

#[derive(Debug)]
pub struct StemError(pub String);

impl std::fmt::Display for StemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for StemError {}

/// Run `demucs -n htdemucs` on the given audio file and return separated stems.
/// Blocking — call from a background thread, never from the audio callback.
pub fn separate_stems(audio_path: &Path) -> Result<Arc<TrackStems>, StemError> {
    let tmp = std::env::temp_dir().join(format!("dubplate_stems_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| StemError(format!("tmpdir: {e}")))?;

    let result = run_separation(audio_path, &tmp);

    let _ = std::fs::remove_dir_all(&tmp); // always clean up
    result
}

fn run_separation(audio_path: &Path, tmp: &Path) -> Result<Arc<TrackStems>, StemError> {
    let status = std::process::Command::new("demucs")
        .args(["-n", "htdemucs", "--out"])
        .arg(tmp)
        .arg(audio_path)
        .status()
        .map_err(|e| StemError(format!("demucs launch: {e}")))?;

    if !status.success() {
        return Err(StemError(format!("demucs exited with status {status}")));
    }

    // Output layout: tmp/htdemucs/<track_stem_name>/drums.wav etc.
    let track_name = audio_path.file_stem().ok_or_else(|| StemError("no file stem".into()))?.to_string_lossy();
    let stem_dir = tmp.join("htdemucs").join(track_name.as_ref());

    log::info!("loading stem WAVs from {}", stem_dir.display());

    let (drums, sr1) = load_wav_stereo(&stem_dir.join("drums.wav"))?;
    let (bass, sr2) = load_wav_stereo(&stem_dir.join("bass.wav"))?;
    let (other, _) = load_wav_stereo(&stem_dir.join("other.wav"))?;
    let (vocals, _) = load_wav_stereo(&stem_dir.join("vocals.wav"))?;

    if sr1 != sr2 {
        log::warn!("stem sample rate mismatch ({sr1} vs {sr2}) — using {sr1}");
    }

    Ok(Arc::new(TrackStems { drums, bass, other, vocals, sample_rate: sr1 }))
}

fn load_wav_stereo(path: &Path) -> Result<(Vec<f32>, u32), StemError> {
    let file = std::fs::File::open(path).map_err(|e| StemError(format!("open {}: {e}", path.display())))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("wav");

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| StemError(format!("probe {}: {e}", path.display())))?;

    let mut format = probed.format;
    let track = format.tracks().first().ok_or_else(|| StemError("no audio track".into()))?.clone();

    let sr = track.codec_params.sample_rate.unwrap_or(44100);
    let ch = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| StemError(format!("decoder: {e}")))?;

    let mut samples: Vec<f32> = Vec::new();

    loop {
        use symphonia::core::errors::Error as SymphError;
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                let src = buf.samples();
                if ch == 1 {
                    // Mono → duplicate to stereo
                    for &s in src {
                        samples.push(s);
                        samples.push(s);
                    }
                } else {
                    samples.extend_from_slice(src);
                }
            }
            Err(_) => continue,
        }
    }

    Ok((samples, sr))
}

#[cfg(test)]
mod wave_tests {
    use super::*;

    #[test]
    fn each_stem_gets_its_own_summary() {
        let sr = 44100u32;
        let n = sr as usize * 3;
        // the bass stem is a loud 60 Hz tone, the vocals a quiet 1 kHz one, the others silent
        let tone = |hz: f32, amp: f32| -> Vec<f32> {
            (0..n)
                .flat_map(|i| {
                    let v = amp * (i as f32 * hz * std::f32::consts::TAU / sr as f32).sin();
                    [v, v]
                })
                .collect()
        };
        let stems = TrackStems {
            drums: vec![0.0; n * 2],
            bass: tone(60.0, 0.8),
            other: vec![0.0; n * 2],
            vocals: tone(1000.0, 0.1),
            sample_rate: sr,
        };
        let w = StemWaves::build(&stems);
        let peak = |i: usize| w.0[i].levels[0].cols.iter().map(|c| c.decode().peak()).fold(0.0f32, f32::max);
        assert!(peak(0) < 0.01 && peak(2) < 0.01, "silent stems stay flat");
        assert!(peak(1) > 0.6, "bass {}", peak(1));
        assert!((0.05..0.15).contains(&peak(3)), "vocals {}", peak(3));
        assert!(w.0.iter().all(|s| s.total_frames == n as u64));
    }
}
