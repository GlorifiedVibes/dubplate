//! HTDemucs stem separation through ONNX Runtime.
//!
//! Nothing here is linked at build time. The runtime (`libonnxruntime`) is opened when the first track
//! is separated, so the app starts, plays and mixes without it. Install both files with
//! `scripts/setup_onnx.sh`:
//!
//! * model   `<data>/dubplate/models/htdemucs-dml.onnx` (about 372 MB; STFT/ISTFT are outside the graph,
//!   which is what the shared pipeline in [`crate::stem_pipeline`] does)
//! * runtime `<data>/dubplate/runtime/` — `libonnxruntime`, and for NVIDIA GPUs the CUDA 13 / cuDNN 9
//!   libraries next to it
//!
//! A GPU is used when it can be (CUDA, DirectML on Windows, CoreML on macOS), otherwise the CPU; `DUBPLATE_DEVICE=cpu` forces the CPU. On Windows the high-performance (discrete) GPU is asked for; `DUBPLATE_GPU=<n>` picks an adapter by number.
//! The session stays loaded between tracks, since opening the model takes several seconds.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, OnceLock};

use ort::environment::Environment;
use ort::ep;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

use crate::stem_pipeline::{separate_chunked, OUTPUT_LEN, SEGMENT_FRAMES};
use crate::stems::{StemError, TrackStems, MODEL_FILE, OLD_MODEL_FILE};

pub fn model_path() -> PathBuf {
    // `DUBPLATE_MODEL=<file>` tries another export of the model (see scripts/export_htdemucs_onnx_dml.py).
    if let Some(p) = std::env::var_os("DUBPLATE_MODEL").map(PathBuf::from).filter(|p| p.is_file()) {
        return p;
    }
    let dir = crate::paths::data_dir().join("models");
    // The compact export runs on DirectML; the older one still works (on the CPU) for anyone who has only that.
    let old = dir.join(OLD_MODEL_FILE);
    if !dir.join(MODEL_FILE).is_file() && old.is_file() {
        return old;
    }
    dir.join(MODEL_FILE)
}

pub fn is_model_present() -> bool {
    model_path().is_file()
}

pub fn runtime_dir() -> PathBuf {
    crate::paths::data_dir().join("runtime")
}

fn library_file_name() -> &'static str {
    crate::paths::onnx_library_name()
}

/// The ONNX Runtime library to open: `ORT_DYLIB_PATH`, else the runtime folder, else whatever the
/// system loader finds under the usual name.
fn runtime_library() -> PathBuf {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH").filter(|p| !p.is_empty()).map(PathBuf::from) {
        return p;
    }
    // Versioned files only (`libonnxruntime.so.1.31.0`) are common, so look for either spelling.
    let dir = runtime_dir();
    let plain = dir.join(library_file_name());
    if plain.exists() {
        return plain;
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut hits: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(library_file_name())))
            .collect();
        hits.sort();
        if let Some(p) = hits.pop() {
            return p;
        }
    }
    PathBuf::from(library_file_name())
}

/// Libraries that live beside the runtime and must be opened first: on Linux the CUDA / cuDNN files
/// (the provider has no run-path), on Windows `DirectML.dll` and the providers library (a DLL loaded by
/// full path doesn't look in its own folder for what it needs).
fn is_companion_library(name: &str) -> bool {
    match crate::paths::Os::current() {
        crate::paths::Os::Linux => {
            name.starts_with("lib") && name.contains(".so") && !name.starts_with("libonnxruntime")
        }
        crate::paths::Os::Windows => {
            let n = name.to_ascii_lowercase();
            n.ends_with(".dll") && n != "onnxruntime.dll"
        }
        crate::paths::Os::MacOs => false,
    }
}

/// Opens those libraries by full path; whatever loads next then resolves to the copies already mapped.
fn preload_companion_libraries(dir: &Path) {
    let mut pending: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(is_companion_library))
        .collect();
    // They depend on one another (cublas → cublasLt), so retry until a pass loads nothing new.
    loop {
        let before = pending.len();
        pending.retain(|p| match unsafe { libloading::Library::new(p) } {
            Ok(lib) => {
                std::mem::forget(lib);
                false
            } // keep it mapped for the life of the process
            Err(_) => true,
        });
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    if !pending.is_empty() {
        log::debug!("ONNX: {} runtime libraries could not be preloaded", pending.len());
    }
}

struct Engine {
    _env: Environment,
    session: Session,
    model: PathBuf,
    device: &'static str,
    input: String,
    output: String,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
static RUNTIME: OnceLock<Result<Environment, String>> = OnceLock::new();

/// Opens the ONNX Runtime library once. Cached, so a missing runtime is reported cheaply every time.
fn environment() -> Result<Environment, StemError> {
    RUNTIME
        .get_or_init(|| {
            let lib = runtime_library();
            preload_companion_libraries(&runtime_dir());
            ort::init_from(&lib)
                .map_err(|e| format!("could not load ONNX Runtime ({e})"))?
                .with_name("dubplate")
                .build()
                .map_err(|e| format!("ONNX Runtime environment: {e}"))
        })
        .clone()
        .map_err(StemError)
}

/// True when the ONNX Runtime library can be opened (the model file is checked separately).
pub fn is_runtime_available() -> bool {
    match environment() {
        Ok(_) => true,
        Err(e) => {
            log::info!("ONNX engine unavailable: {e}");
            false
        }
    }
}

/// `"CUDA"`, `"DirectML"`, `"CoreML"` or `"CPU"` once a model has been opened.
pub fn active_device() -> Option<&'static str> {
    ENGINE.lock().ok()?.as_ref().map(|e| e.device)
}

/// GPU providers worth trying on this platform, best first. Each is asked to fail loudly, so a missing
/// driver or library moves on to the next one (and finally the CPU) instead of silently running slowly.
fn gpu_providers() -> Vec<(&'static str, ep::ExecutionProviderDispatch)> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.push(("CoreML", ep::CoreML::default().build().error_on_failure()));
    } else {
        v.push(("CUDA", ep::CUDA::default().build().error_on_failure()));
        if cfg!(target_os = "windows") {
            // DirectML's own default is the first adapter Windows lists, which on a PC with a small integrated
            // GPU (a Radeon with 486 MB) is that one, and it runs out of memory. So: the adapter with the most
            // memory of its own. `DUBPLATE_GPU=<n>` picks one by its number.
            let list = crate::gpus::adapters();
            log::info!("graphics adapters:\n{}", crate::gpus::describe(&list));
            let chosen = std::env::var("DUBPLATE_GPU")
                .ok()
                .and_then(|v| v.trim().parse::<i32>().ok())
                .or_else(|| crate::gpus::biggest(&list).map(|a| a.index));
            let dml = match chosen {
                Some(n) => {
                    log::info!("DirectML: adapter {n}");
                    ep::DirectML::default().with_device_id(n)
                }
                None => ep::DirectML::default()
                    .with_performance_preference(ep::directml::PerformancePreference::HighPerformance)
                    .with_device_filter(ep::directml::DeviceFilter::Gpu),
            };
            v.push(("DirectML", dml.build().error_on_failure()));
        }
    }
    v
}

/// Set when a GPU run failed part-way (a driver or an operator the GPU can't do), so the retry uses the CPU.
static GPU_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `DUBPLATE_DML_FUSION=off` runs DirectML one operator at a time (slower, needs less memory): a switch for
/// graphics cards that struggle with the model.
fn dml_unfused() -> bool {
    std::env::var("DUBPLATE_DML_FUSION").is_ok_and(|v| v.eq_ignore_ascii_case("off"))
}

fn open_session(env: &Environment, model: &Path) -> Result<(Session, &'static str), StemError> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let build = |gpu: Option<ep::ExecutionProviderDispatch>, unfused: bool| -> ort::Result<Session> {
        let mut b = Session::builder(env)?.with_optimization_level(GraphOptimizationLevel::All)?;
        if let Some(p) = gpu {
            // DirectML needs one operator at a time and no planned memory reuse (Microsoft's rule for its provider).
            b = b.with_memory_pattern(false)?.with_parallel_execution(false)?;
            if unfused {
                b = b.with_config_entry("ep.dml.disable_graph_fusion", "1")?;
            }
            b = b.with_execution_providers([p])?;
        }
        b.with_intra_threads(threads)?.commit_from_file(model)
    };
    let cpu_only = GPU_FAILED.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("DUBPLATE_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
    if !cpu_only {
        for (name, provider) in gpu_providers() {
            match build(Some(provider), name == "DirectML" && dml_unfused()) {
                Ok(s) => return Ok((s, name)),
                Err(e) => log::info!("ONNX: {name} not available ({e})"),
            }
        }
        log::info!("ONNX: no GPU available; using the CPU");
    }
    build(None, false)
        .map(|s| (s, "CPU"))
        .map_err(|e| StemError(format!("ONNX: could not open '{}': {e}", model.display())))
}

/// Separate `samples` (interleaved, mono or stereo) into four stems.
///
/// Blocking and long-running: call it from a worker thread. `progress` goes 0 → 100.
pub fn separate_onnx(
    samples: &[f32],
    sample_rate: u32,
    channels: usize,
    model_file: &Path,
    progress: Option<&AtomicU32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Arc<TrackStems>, StemError> {
    let first = separate_on_engine(samples, sample_rate, channels, model_file, progress, cancel);
    // A GPU that opened the model but can't run it must not leave the song unseparated: go again on the CPU.
    if first.as_ref().err().is_some_and(|e| e.0 == crate::stem_pipeline::CANCELLED) {
        return first;
    }
    if first.is_err() && active_device().is_some_and(|d| d != "CPU") {
        log::warn!(
            "ONNX: the {} failed ({}); trying again on the CPU",
            active_device().unwrap_or("GPU"),
            first.as_ref().err().map_or(String::new(), |e| e.to_string())
        );
        GPU_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut g) = ENGINE.lock() {
            *g = None;
        }
        return separate_on_engine(samples, sample_rate, channels, model_file, progress, cancel);
    }
    first
}

fn separate_on_engine(
    samples: &[f32],
    sample_rate: u32,
    channels: usize,
    model_file: &Path,
    progress: Option<&AtomicU32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Arc<TrackStems>, StemError> {
    let env = environment()?;
    let mut guard = ENGINE.lock().map_err(|_| StemError("stem engine lock poisoned".into()))?;

    if guard.as_ref().is_none_or(|e| e.model != model_file) {
        let started = std::time::Instant::now();
        let (session, device) = open_session(&env, model_file)?;
        let input = session.inputs().first().map_or_else(|| "mix".to_owned(), |i| i.name().to_owned());
        let output = session.outputs().first().map_or_else(|| "stems".to_owned(), |o| o.name().to_owned());
        log::info!(
            "ONNX model ready on {device} in {:.1}s (input '{input}', output '{output}')",
            started.elapsed().as_secs_f32()
        );
        *guard = Some(Engine { _env: env, session, model: model_file.to_owned(), device, input, output });
    }
    let engine = guard.as_mut().expect("engine just created");
    let (input_name, output_name) = (engine.input.clone(), engine.output.clone());

    separate_chunked(samples, sample_rate, channels, progress, cancel, |segment| {
        let input = Tensor::from_array(([1i64, 2, SEGMENT_FRAMES as i64], segment.to_vec()))
            .map_err(|e| StemError(format!("ONNX input: {e}")))?;
        let outputs = engine
            .session
            .run(ort::inputs![input_name.as_str() => input])
            .map_err(|e| StemError(format!("ONNX inference: {e}")))?;
        let (shape, data) = outputs[output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| StemError(format!("ONNX output: {e}")))?;
        if data.len() != OUTPUT_LEN {
            return Err(StemError(format!(
                "ONNX model returned {} values of shape {shape:?}, expected 4 stems × 2 channels × {SEGMENT_FRAMES} — is this an HTDemucs 4-stem model?",
                data.len())));
        }
        Ok(data.to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_the_dubplate_data_dir() {
        assert!(model_path().to_string_lossy().contains("models"));
        assert!(runtime_dir().ends_with("dubplate/runtime"));
    }

    /// Manual check of the installed runtime and model:
    /// `cargo test -p dubplate-engine installed_engine_runs -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn installed_engine_runs() {
        if !is_model_present() || !is_runtime_available() {
            eprintln!("model or runtime not installed");
            return;
        }
        let tone: Vec<f32> = (0..44100 * 4)
            .flat_map(|i| {
                let x = (i as f32 * 0.05).sin() * 0.3;
                [x, x]
            })
            .collect();
        let stems = separate_onnx(&tone, 44100, 2, &model_path(), None, None).expect("separation");
        eprintln!("separated on {:?}: {} frames per stem", active_device(), stems.drums.len() / 2);
        assert_eq!(stems.drums.len(), tone.len());
    }

    #[test]
    fn companion_libraries_are_recognised_per_platform() {
        // Whatever platform this runs on, the runtime itself is never preloaded as a companion.
        assert!(!is_companion_library(crate::paths::onnx_library_name()));
        assert!(!is_companion_library("readme.txt"));
        #[cfg(target_os = "linux")]
        {
            assert!(is_companion_library("libcudnn.so.9"));
            assert!(!is_companion_library("libonnxruntime_providers_cuda.so"));
        }
    }

    #[test]
    fn a_missing_runtime_is_an_error_not_a_panic() {
        // Whatever the machine has, asking must never abort the process.
        let _ = is_runtime_available();
    }
}
