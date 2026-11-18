//! Dubplate's shared audio engine: decoding, the deck players and mixer, audio output, microphones, key lock and
//! time stretch, pitch tracking, stem separation and the waveform summary. The DJ app is built on it, and other programs can be (it
//! has no DJ analysis in it). Beat analysis and the USB export live in `dubplate-engine`, which re-exports everything here.

pub mod cache;
pub mod click;
pub mod deck;
pub mod decode;
pub mod eq;
pub mod gpus;
pub mod keylock;
pub mod loudness;
pub mod meta;
pub mod mic;
pub mod output;
pub mod paths;
pub mod pitch;
pub mod recorder;
pub mod stem_pipeline;
pub mod stems;
pub mod stretch;
pub mod threads;
#[cfg(feature = "vst3-host")]
pub mod vst3_host;
pub mod waveform_data;

#[cfg(feature = "onnx-inference")]
pub mod ort_stems;
pub mod playhead;
#[cfg(feature = "torch-inference")]
pub mod tch_stems;

pub use click::ClickControls;
pub use deck::{
    create_deck_pair, BeatGrid, Cue, DeckCommand, DeckHandle, DeckPlayer, DeckShared, DecodedTrack, LoopRegion,
    DEFAULT_LOOP_BEATS, LOOP_SIZES, TEMPO_RANGE, WAVEFORM_CHUNK_FRAMES,
};
pub use decode::decode_track;
pub use eq::{DeckEffect, DeckEq, FilterKnob};
pub use meta::{detect_key, preview_columns, read_tags, Key, TrackTags};
pub use mic::{list_input_devices, mic_bus, mic_bus_tapped, MicControls, MicFeed, MicInput, MicProcessor, MicTap};
pub use output::{
    crossfade_gains, default_output_device_name, list_output_devices, list_output_devices_with_channels, monitor_frame,
    start_audio, start_audio_with_mic, start_audio_with_prep, AudioOutput, MonitorControls, MonitorReader,
    MonitorTarget, CF_FAST, CF_SMOOTH,
};
pub use recorder::{default_recording_path, new_sink, start_recording, RecordSession, RecordSink};
pub use stems::{
    is_demucs_available, is_model_present, is_stem_engine_available, model_path, separate, separate_stems, stem_device,
    stem_engine_description, stem_engine_help, StemWaves, TrackStems, MODEL_FILE, OLD_MODEL_FILE, STEM_NAMES,
    STEM_SHORTS,
};
pub use waveform_data::{Column as WaveColumn, Level as WaveLevelValues, WaveLevel, WaveSummary};
