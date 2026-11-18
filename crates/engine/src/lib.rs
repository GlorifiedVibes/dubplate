//! Dubplate's DJ engine: beat and tempo analysis, bar 1 and sections, hot-cue roles, tap tempo, and the Pioneer
//! analysis and USB export formats. The audio engine itself (decode, decks, output, stems, ...) is `dubplate-core`,
//! re-exported here so `dubplate_engine::deck`, `dubplate_engine::DeckPlayer` and the rest keep their paths.

pub use dubplate_core::*;
// modules by name too, so `dubplate_engine::deck::...` paths resolve (a glob re-export carries items, not module paths of
// the same name as local ones)
#[cfg(feature = "onnx-inference")]
pub use dubplate_core::ort_stems;
#[cfg(feature = "torch-inference")]
pub use dubplate_core::tch_stems;
#[cfg(feature = "vst3-host")]
pub use dubplate_core::vst3_host;
pub use dubplate_core::{
    cache, click, deck, eq, gpus, keylock, loudness, meta, mic, output, paths, pitch, playhead, recorder,
    stem_pipeline, stems, stretch, waveform_data,
};

pub mod anlz;
pub mod anlz_read;
pub mod bpm;
pub mod cue_roles;
pub mod decode;
pub mod downbeat;
pub mod pdb;
mod pdb_fixed;
pub mod structure;
pub mod tap;
pub mod usb_export;

pub use decode::decode_file;
pub use structure::{BeatEnergy, Kind as SectionKind, Section, Structure};
