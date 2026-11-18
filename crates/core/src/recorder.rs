use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use ringbuf::{
    traits::{Consumer, Split},
    HeapRb,
};

/// Shared between the CPAL audio callback and the app layer.
/// Holds the ring-buffer producer while a recording is in progress, None otherwise.
/// `try_lock()` in the audio callback is non-blocking — if contested (only at start/stop),
/// the frame is silently skipped.
pub type RecordSink = Arc<Mutex<Option<ringbuf::HeapProd<f32>>>>;

pub fn new_sink() -> RecordSink {
    Arc::new(Mutex::new(None))
}

pub struct RecordSession {
    pub path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RecordSession {
    /// Signal writer to flush remaining samples and finalise the WAV.
    /// Returns immediately; the thread finishes in the background.
    pub fn stop(&mut self, sink: &RecordSink) {
        // Remove producer first — no more frames can be pushed after this.
        if let Ok(mut g) = sink.lock() {
            *g = None;
        }
        self.stop_flag.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().map(|t| t.is_finished()).unwrap_or(true)
    }
}

/// Start a new recording session.  Installs a ring-buffer producer into `sink`;
/// spawns a background thread that writes the WAV.
pub fn start_recording(
    path: PathBuf,
    sample_rate: u32,
    sink: &RecordSink,
) -> Result<RecordSession, Box<dyn std::error::Error + Send + Sync>> {
    // 10-second headroom at 44100 Hz stereo
    let rb = HeapRb::<f32>::new(sample_rate as usize * 2 * 10);
    let (prod, cons) = rb.split();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop_flag);

    {
        let mut g = sink.lock().map_err(|e| e.to_string())?;
        *g = Some(prod);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let path_clone = path.clone();
    let thread = std::thread::Builder::new()
        .name("dubplate-recorder".into())
        .spawn(move || writer_thread(path_clone, sample_rate, cons, stop_clone))?;

    Ok(RecordSession { path, stop_flag, thread: Some(thread) })
}

/// Returns a sensible default output path for a new recording.
/// `~/Music/Dubplate Recordings/recording-<unix_secs>.wav`
pub fn default_recording_path() -> PathBuf {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    crate::paths::home_dir().join("Music/Dubplate Recordings").join(format!("recording-{secs}.wav"))
}

// ── Writer thread ─────────────────────────────────────────────────────────────

fn writer_thread(path: PathBuf, sample_rate: u32, mut cons: ringbuf::HeapCons<f32>, stop: Arc<AtomicBool>) {
    let spec =
        hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };

    let mut writer = match hound::WavWriter::create(&path, spec) {
        Ok(w) => w,
        Err(e) => {
            log::error!("recorder: cannot open '{}': {e}", path.display());
            return;
        }
    };

    let mut n_samples: u64 = 0;

    loop {
        // Drain whatever is available
        while let Some(s) = cons.try_pop() {
            if let Err(e) = writer.write_sample(s) {
                log::error!("recorder write: {e}");
                return;
            }
            n_samples += 1;
        }

        if stop.load(Ordering::Acquire) {
            // Final drain — catch frames queued between last pop and stop signal
            while let Some(s) = cons.try_pop() {
                let _ = writer.write_sample(s);
                n_samples += 1;
            }
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let secs = n_samples as f64 / (sample_rate as f64 * 2.0);
    let m = secs as u64 / 60;
    let s = secs as u64 % 60;

    match writer.finalize() {
        Ok(()) => log::info!("recorder: saved {m}:{s:02} → {}", path.display()),
        Err(e) => log::error!("recorder finalize: {e}"),
    }
}
