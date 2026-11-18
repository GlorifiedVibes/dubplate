//! Prints what the engine's analysis finds for each file: BPM, first beat (ms) and the beat grid's start.
//! `cargo run -p dubplate-engine --release --example beatcheck -- FILE...`
fn main() {
    for p in std::env::args().skip(1) {
        match dubplate_engine::decode::decode_file(std::path::Path::new(&p)) {
            Ok(t) => {
                let b = &t.beat_times_ms;
                // BEATCHECK_GENRE="Dubstep" tries a genre on files that have no tag.
                let genre = std::env::var("BEATCHECK_GENRE")
                    .ok()
                    .or(dubplate_engine::meta::read_tags(std::path::Path::new(&p)).genre);
                let hint = genre.as_deref().and_then(dubplate_engine::bpm::tempo_hint_for_genre);
                let plain = dubplate_engine::bpm::analyze_beats_hinted(&t.samples, t.channels, t.sample_rate, None).bpm;
                println!(
                    "  genre {:?}: hint {:?}; BPM without the hint {:.3}, with it {:.3}",
                    genre,
                    hint.map(|h| h.centre),
                    plain,
                    dubplate_engine::bpm::analyze_beats_hinted(&t.samples, t.channels, t.sample_rate, hint).bpm
                );
                let d = dubplate_engine::bpm::analyze_beats_detail(&t.samples, t.channels, t.sample_rate);
                let (curve, frame) = dubplate_engine::bpm::onset_curve_full(&t.samples, t.channels, t.sample_rate);
                let q = dubplate_engine::bpm::grid_quality(&curve, frame, d.bpm, d.offset_chunks);
                println!(
                    "  quality: {:?}, {} kicks ({:.0}% of beats), bias {:+.1} ms, spread {:.1} ms, drift {:+.1} ms",
                    q.verdict,
                    q.hits,
                    q.hit_rate * 100.0,
                    q.bias_ms,
                    q.spread_ms,
                    q.drift_ms
                );
                println!("  fit: {} beats, spread {:.1} ms", d.fitted_beats, d.spread_ms);
                println!(
                    "{}\tbpm {:.3}\tfirst beat {:.0} ms\t{} beats\tduration {:.1}s",
                    std::path::Path::new(&p).file_name().unwrap().to_string_lossy(),
                    t.bpm,
                    b.first().copied().map_or(f64::NAN, |v| v as f64),
                    b.len(),
                    t.duration_secs
                );
            }
            Err(e) => println!("{p}\terror {e}"),
        }
    }
}
