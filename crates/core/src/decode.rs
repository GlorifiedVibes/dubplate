//! Reading audio files into memory. `decode_pcm` is the plain decode; `decode_track` wraps it as a [`DecodedTrack`]
//! with no analysis (no tempo, key, loudness or waveform summary), which is all a player needs. The DJ app's
//! `dubplate_engine::decode_file` adds the analysis on top.

use std::path::Path;

use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error as SymphoniaError, formats::FormatOptions,
    io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};

use crate::deck::DecodedTrack;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("symphonia: {0}")]
    Symphonia(#[from] SymphoniaError),
    #[error("{0}")]
    Other(&'static str),
}

/// Interleaved samples of a decoded file.
pub struct Pcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// At least 1.
    pub channels: usize,
}

/// Decodes an audio file to interleaved f32 samples in memory. Runs on a background thread, never in the audio callback.
pub fn decode_pcm(path: &Path) -> Result<Pcm, DecodeError> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed =
        symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())?;

    let mut format = probed.format;
    let track = format.default_track().ok_or(DecodeError::Other("no audio track found"))?.clone();

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);

    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut all_samples: Vec<f32> = Vec::with_capacity(44100 * channels * 4); // 4s initial

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                all_samples.extend_from_slice(buf.samples());
            }
            Err(SymphoniaError::DecodeError(e)) => {
                log::warn!("skipping corrupt packet: {e}");
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(Pcm { samples: all_samples, sample_rate, channels: channels.max(1) })
}

/// A file as a [`DecodedTrack`] with nothing analysed: tempo 0, no key, no loudness, an empty waveform and summary.
pub fn decode_track(path: &Path) -> Result<DecodedTrack, DecodeError> {
    let Pcm { samples, sample_rate, channels } = decode_pcm(path)?;
    let duration_frames = samples.len() / channels;
    Ok(DecodedTrack {
        loudness: None,
        title: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").to_string(),
        duration_secs: duration_frames as f64 / sample_rate as f64,
        duration_frames: duration_frames as u64,
        samples,
        sample_rate,
        channels,
        waveform: Vec::new(),
        wave: Default::default(),
        bpm: 0.0,
        bpm_coarse: 0,
        beat_offset: 0.0,
        beat_times_ms: Vec::new(),
        key: None,
        cues: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit stereo WAV of `frames` frames of a 440 Hz tone, written by hand so the test needs no files.
    fn tone_wav(frames: usize, rate: u32) -> Vec<u8> {
        let data_len = (frames * 4) as u32;
        let mut w = Vec::new();
        w.extend(b"RIFF");
        w.extend((36 + data_len).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(rate.to_le_bytes());
        w.extend((rate * 4).to_le_bytes());
        w.extend(4u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend(data_len.to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0) as i16;
            w.extend(v.to_le_bytes());
            w.extend(v.to_le_bytes());
        }
        w
    }

    #[test]
    fn a_plain_decode_has_the_samples_and_nothing_analysed() {
        let dir = std::env::temp_dir().join(format!("dubplate-core-decode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Some Song.wav");
        std::fs::write(&path, tone_wav(44100, 44100)).unwrap();
        let t = decode_track(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!((t.sample_rate, t.channels, t.duration_frames), (44100, 2, 44100));
        assert_eq!(t.samples.len(), 44100 * 2);
        assert!((t.duration_secs - 1.0).abs() < 1e-9);
        assert_eq!(t.title, "Some Song");
        assert!(t.samples.iter().any(|&s| s.abs() > 0.3), "the tone is in there");
        // nothing analysed, so nothing slow was done
        assert!(
            t.bpm == 0.0
                && t.key.is_none()
                && t.loudness.is_none()
                && t.waveform.is_empty()
                && t.wave.is_empty()
                && t.cues.is_empty()
        );
    }
}
