//! Times the waveform summary on real files and can dump its finest level for comparing with rekordbox's.
//! `cargo run -p dubplate-engine --release --example wavecheck -- FILE [DUMP.bin]`
//! The dump is the finest level's bytes, six per column: min, max, rms, low, mid, high.
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else { return eprintln!("usage: wavecheck FILE [DUMP.bin]") };
    let t = dubplate_engine::decode::decode_file(std::path::Path::new(&path)).expect("decode");
    let started = Instant::now();
    let w = dubplate_engine::WaveSummary::build(&t.samples, t.channels, t.sample_rate);
    let took = started.elapsed();
    let bytes = postcard::to_allocvec(&w).map(|b| b.len()).unwrap_or(0);
    println!(
        "{} s of audio at {} Hz: summary in {:.0} ms ({:.0}x real time), {:.2} MB stored, ref peak {:.2}",
        t.duration_secs.round(),
        t.sample_rate,
        took.as_secs_f64() * 1000.0,
        t.duration_secs / took.as_secs_f64(),
        bytes as f64 / 1e6,
        w.ref_peak
    );
    if let Some(out) = args.next() {
        let raw: Vec<u8> =
            w.levels[0].cols.iter().flat_map(|c| [c.min as u8, c.max as u8, c.rms, c.low, c.mid, c.high]).collect();
        std::fs::write(out, raw).unwrap();
    }
}
