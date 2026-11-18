use std::path::Path;

use rustfft::{num_complex::Complex, FftPlanner};

use crate::deck::{DecodedTrack, WAVEFORM_CHUNK_FRAMES};
pub use dubplate_core::decode::{decode_pcm, decode_track, DecodeError, Pcm};

/// Decodes an audio file and analyses it: waveform summary, tempo and grid, key, loudness. On later loads the
/// expensive FFT + BPM analysis is skipped by reading from `~/.cache/dubplate/<hash>.bin`; audio decode always runs
/// (samples are not cached, they're too large and symphonia is fast). Runs on a background thread, never in the audio
/// callback.
pub fn decode_file(path: &Path) -> Result<DecodedTrack, DecodeError> {
    let cached = crate::cache::load(path);
    let Pcm { samples: all_samples, sample_rate, channels } = decode_pcm(path)?;
    let ch = channels.max(1);
    let duration_frames = all_samples.len() / ch;

    let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").to_string();
    let duration_secs = duration_frames as f64 / sample_rate as f64;

    let wave = std::sync::Arc::new(crate::cache::load_wave(path).unwrap_or_else(|| {
        let built = crate::waveform_data::WaveSummary::build(&all_samples, ch, sample_rate);
        if let Err(e) = crate::cache::save_wave(path, &built) {
            log::warn!("waveform cache write failed: {e}");
        }
        built
    }));

    // ── Use analysis cache if valid ──────────────────────────────────────────
    if let Some(c) = cached {
        log::info!("cache hit — skipping FFT/BPM for {}", path.display());
        return Ok(DecodedTrack {
            loudness: c.loudness,
            wave,
            samples: all_samples,
            sample_rate: c.sample_rate,
            channels: c.channels,
            duration_frames: c.duration_frames,
            duration_secs: c.duration_secs,
            title: c.title,
            waveform: c.waveform,
            bpm: c.bpm,
            bpm_coarse: c.bpm_coarse,
            beat_offset: c.beat_offset,
            beat_times_ms: c.beat_times_ms,
            key: c.key,
            cues: c.cues,
        });
    }

    // ── Cache miss — run full FFT + BPM analysis ─────────────────────────────
    // Spectral waveform: [low, mid, high] energy per WAVEFORM_CHUNK_FRAMES block.
    // FFT bin width ≈ 172 Hz at 44100 Hz (256-point FFT).
    //   Low:  bins 1–5   (172–860 Hz)  — kick / bass
    //   Mid:  bins 6–25  (1032–4300 Hz) — snare / vocals
    //   High: bins 26–127 (4472–21888 Hz) — cymbals / air
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WAVEFORM_CHUNK_FRAMES);

    // Pre-allocate once — reused for every chunk to avoid tens-of-thousands of heap allocs.
    let mut fft_buf = vec![Complex::new(0.0f32, 0.0); WAVEFORM_CHUNK_FRAMES];
    let hann: Vec<f32> = (0..WAVEFORM_CHUNK_FRAMES)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (WAVEFORM_CHUNK_FRAMES - 1) as f32).cos()))
        .collect();

    let mut raw_bands: Vec<[f32; 3]> = Vec::with_capacity(all_samples.len() / (WAVEFORM_CHUNK_FRAMES * ch) + 1);

    for chunk in all_samples.chunks(WAVEFORM_CHUNK_FRAMES * ch) {
        for i in 0..WAVEFORM_CHUNK_FRAMES {
            let mono = if ch == 1 {
                chunk.get(i).copied().unwrap_or(0.0)
            } else {
                let l = chunk.get(i * 2).copied().unwrap_or(0.0);
                let r = chunk.get(i * 2 + 1).copied().unwrap_or(0.0);
                (l + r) * 0.5
            };
            fft_buf[i] = Complex::new(mono * hann[i], 0.0);
        }
        fft.process(&mut fft_buf);

        let low = (1usize..6).map(|i| fft_buf[i].norm()).fold(0.0f32, f32::max);
        let mid = (6usize..26).map(|i| fft_buf[i].norm()).fold(0.0f32, f32::max);
        let high = (26usize..WAVEFORM_CHUNK_FRAMES / 2).map(|i| fft_buf[i].norm()).fold(0.0f32, f32::max);

        raw_bands.push([low, mid, high]);
    }

    // Normalise each band independently so the display fills full height.
    let max_low = raw_bands.iter().map(|b| b[0]).fold(0.0f32, f32::max).max(1.0);
    let max_mid = raw_bands.iter().map(|b| b[1]).fold(0.0f32, f32::max).max(1.0);
    let max_high = raw_bands.iter().map(|b| b[2]).fold(0.0f32, f32::max).max(1.0);

    let waveform: Vec<[f32; 3]> = raw_bands.iter().map(|b| [b[0] / max_low, b[1] / max_mid, b[2] / max_high]).collect();

    let key = crate::meta::detect_key(&all_samples, ch, sample_rate);
    // The genre tag, when there is one, says which octave the tempo is in (dubstep 140, not 70).
    let hint = crate::meta::read_tags(path).genre.as_deref().and_then(crate::bpm::tempo_hint_for_genre);
    let analysis = crate::bpm::analyze_beats_hinted(&all_samples, ch, sample_rate, hint);
    // Which beat starts a bar is decided from the kick, the bass, the section boundaries and the start of the music
    // together; the analysis' own kick-only answer stands when there is too little to go on.
    // A track whose stems have been separated before (a Reel is done in the background) also has the drums stem's note.
    let drums = crate::cache::load_drums(path).map(|n| n.sign);
    let (beat_offset, downbeat_confidence) =
        match crate::downbeat::choose_with_drums(&wave, &analysis, sample_rate, drums) {
            Some(d) => {
                log::info!(
                    "downbeat: beat residue {} (confidence {:.1}{})",
                    d.residue,
                    d.confidence,
                    if drums.is_some() { ", with the drums stem" } else { "" }
                );
                (d.anchor_chunks, d.confidence)
            }
            None => (analysis.offset_chunks, 0.0),
        };
    let bpm = analysis.bpm;
    let bpm_coarse = (bpm * 100.0).round() as u32;
    let beat_times = crate::bpm::beat_times_ms(bpm, beat_offset, sample_rate, duration_frames as u64);

    let loudness = crate::loudness::measure(&all_samples, ch, sample_rate);

    // ── Write analysis cache ─────────────────────────────────────────────────
    let entry = crate::cache::CacheEntry {
        version: crate::cache::CACHE_VERSION,
        file_mtime_secs: 0,
        sample_rate,
        channels: ch,
        duration_frames: duration_frames as u64,
        duration_secs,
        title: title.clone(),
        bpm,
        bpm_coarse,
        beat_offset,
        beat_times_ms: beat_times.clone(),
        key,
        waveform: waveform.clone(),
        cues: Vec::new(),
        downbeat_confidence,
        grid_phase_chunks: analysis.phase_chunks,
        loudness,
    };
    if let Err(e) = crate::cache::save(path, &entry) {
        log::warn!("cache write failed: {e}");
    }

    // ── Write ANLZ .DAT for CDJ compatibility ────────────────────────────────
    if let Some(anlz_path) = crate::cache::anlz_dat_path(path) {
        if let Err(e) = crate::anlz::write_dat(&anlz_path, &beat_times, bpm_coarse, &[], &waveform) {
            log::warn!("ANLZ write failed: {e}");
        } else {
            log::info!("ANLZ .DAT written: {}", anlz_path.display());
        }
    }

    Ok(DecodedTrack {
        loudness,
        wave,
        samples: all_samples,
        sample_rate,
        channels: ch,
        duration_frames: duration_frames as u64,
        waveform,
        title,
        duration_secs,
        bpm,
        bpm_coarse,
        beat_offset,
        beat_times_ms: beat_times,
        key,
        cues: Vec::new(),
    })
}

/// What [`note_drums`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteOutcome {
    /// The track has no analysis yet (decode it first); nothing was kept.
    NotAnalysed,
    /// The note was kept, and the bar line was sure without it: the track stays as it is.
    Confident,
    /// The note was kept and the bar line was in doubt: the analysis was forgotten, so the next load counts the drums
    /// stem's vote (and may move bar 1).
    Reanalyse,
}

/// Keeps what the drums stem of a separated track says about its bar line (a few bytes, the stems themselves can go),
/// and has the track analysed again at the next load when the bar line was in doubt.
pub fn note_drums(path: &Path, stems: &crate::stems::TrackStems) -> NoteOutcome {
    let Some(entry) = crate::cache::load(path) else { return NoteOutcome::NotAnalysed };
    if entry.bpm <= 0.0 || entry.sample_rate == 0 {
        return NoteOutcome::NotAnalysed;
    }
    let phase_secs = entry.grid_phase_chunks * WAVEFORM_CHUNK_FRAMES as f64 / entry.sample_rate as f64;
    let sign = crate::downbeat::drums_sign(&stems.drums, stems.sample_rate, phase_secs, 60.0 / entry.bpm as f64);
    if let Err(e) = crate::cache::save_drums(path, sign) {
        log::warn!("drums note write failed: {e}");
    }
    if entry.downbeat_confidence < crate::downbeat::DRUMS_ONLY_BELOW {
        crate::cache::forget_analysis(path);
        NoteOutcome::Reanalyse
    } else {
        NoteOutcome::Confident
    }
}

/// Whether a track in a Reel is worth separating in the background: its bar line is in doubt (the signs did not agree)
/// and it has not been asked already. `None` when it has no analysis yet.
pub fn wants_stem_note(path: &Path) -> Option<bool> {
    let entry = crate::cache::load(path)?;
    Some(entry.downbeat_confidence < crate::downbeat::DRUMS_ONLY_BELOW && crate::cache::load_drums(path).is_none())
}
