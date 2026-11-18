/// Embedded HTDemucs inference via TorchScript (tch-rs / libtorch).
///
/// Works without Python.  The model is a one-time TorchScript export that
/// lives at:
///   `$XDG_DATA_HOME/dubplate/models/htdemucs.pt`
///   (typically `~/.local/share/dubplate/models/htdemucs.pt`)
///
/// Generate it once with:
///   python scripts/export_htdemucs_onnx.py
///
/// Build requirement — set before `cargo build`:
///   export LIBTORCH=/path/to/torch   # e.g. /usr/lib/python3.xx/site-packages/torch
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use tch::{CModule, Device, Tensor};

use crate::stem_pipeline::{separate_chunked, OUTPUT_LEN, SEGMENT_FRAMES};
use crate::stems::{StemError, TrackStems};

pub fn model_path() -> PathBuf {
    crate::paths::data_dir().join("models/htdemucs.pt")
}

pub fn is_model_present() -> bool {
    model_path().exists()
}

/// Select inference device.
///
/// Priority:
///   1. `DUBPLATE_DEVICE=cpu` / `=cuda` env var override
///   2. `~/.local/share/dubplate/models/htdemucs.device` metadata written by
///      the export script — must match the device the model was traced on
///   3. Auto: CUDA if available, else CPU
///
/// The `.device` file is the important one: a CPU-traced model cannot run on
/// CUDA (STFT window device mismatch) and vice versa.
pub fn select_device() -> Device {
    // Env var override takes highest priority.
    match std::env::var("DUBPLATE_DEVICE").as_deref() {
        Ok("cpu") => {
            log::info!("tch: device=cpu (DUBPLATE_DEVICE)");
            return Device::Cpu;
        }
        Ok("cuda" | "gpu") => {
            if tch::Cuda::is_available() {
                log::info!("tch: device=cuda:0 (DUBPLATE_DEVICE)");
                return Device::Cuda(0);
            } else {
                log::warn!("tch: CUDA requested but not available — falling back to CPU");
                return Device::Cpu;
            }
        }
        _ => {}
    }

    // Read the device the model was traced on from the companion metadata file.
    let device_file = model_path().with_file_name("htdemucs.device");
    if let Ok(traced_on) = std::fs::read_to_string(&device_file) {
        match traced_on.trim() {
            "cuda" => {
                if tch::Cuda::is_available() {
                    log::info!("tch: device=cuda:0 (model traced on CUDA)");
                    return Device::Cuda(0);
                }
                log::warn!("tch: model was traced on CUDA but CUDA is not available — re-export on CPU with scripts/export_htdemucs_onnx.py");
                return Device::Cpu;
            }
            "cpu" => {
                log::info!("tch: device=cpu (model traced on CPU)");
                return Device::Cpu;
            }
            other => log::warn!("tch: unknown device in {}: '{other}'", device_file.display()),
        }
    }

    // Fall back to auto-detect.
    if tch::Cuda::is_available() {
        log::info!("tch: device=cuda:0 (auto)");
        Device::Cuda(0)
    } else {
        log::info!("tch: device=cpu (CUDA not available)");
        Device::Cpu
    }
}

/// Separate `samples` (interleaved, mono or stereo) into 4 stems with the TorchScript HTDemucs model.
///
/// `progress` is updated 0 → 100 as inference proceeds.
pub fn separate_tch(
    samples: &[f32],
    sample_rate: u32,
    channels: usize,
    model_file: &Path,
    progress: Option<&AtomicU32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Arc<TrackStems>, StemError> {
    let device = select_device();
    let model = CModule::load_on_device(model_file, device)
        .map_err(|e| StemError(format!("tch load '{}': {e}", model_file.display())))?;

    separate_chunked(samples, sample_rate, channels, progress, cancel, |segment| {
        let input = Tensor::from_slice(segment).reshape([1, 2, SEGMENT_FRAMES as i64]).to_device(device);
        let output =
            tch::no_grad(|| model.forward_ts(&[input])).map_err(|e| StemError(format!("tch inference: {e}")))?;
        // [1, 4, 2, T] or [4, 2, T], C-contiguous — the pipeline wants [stem][channel][frame].
        let n_el = output.size().iter().product::<i64>() as usize;
        if n_el != OUTPUT_LEN {
            return Err(StemError(format!("tch: model returned {n_el} values, expected {OUTPUT_LEN}")));
        }
        let mut out = vec![0.0f32; n_el];
        output.contiguous().to_device(Device::Cpu).copy_data(&mut out, n_el);
        Ok(out)
    })
}
