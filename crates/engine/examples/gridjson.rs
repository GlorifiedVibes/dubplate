//! Prints our grid for each file as TSV (path, BPM, first beat s, bar-1 anchor s, bar residue, confidence), for
//! `scripts/beat_this_eval/compare.py`: `gridjson FILE...`
use dubplate_engine::{bpm, decode::decode_file, downbeat};
fn main() {
    for p in std::env::args().skip(1) {
        let Ok(t) = decode_file(std::path::Path::new(&p)) else { continue };
        let a = bpm::analyze_beats_detail(&t.samples, t.channels, t.sample_rate);
        let chunk = 256.0 / t.sample_rate as f64;
        let Some(d) = downbeat::choose(&t.wave, &a, t.sample_rate) else { continue };
        println!(
            "{p}\t{}\t{}\t{}\t{}\t{:.2}",
            a.bpm,
            a.phase_chunks * chunk,
            d.anchor_chunks * chunk,
            d.residue,
            d.confidence
        );
    }
}
