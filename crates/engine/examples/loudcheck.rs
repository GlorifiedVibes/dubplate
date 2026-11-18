//! Prints each file's loudness and the trim auto gain would apply.
//! `cargo run -p dubplate-engine --release --example loudcheck -- FILE...`
use dubplate_engine::loudness::{auto_trim_db, measure, TARGET_LUFS};

fn main() {
    for p in std::env::args().skip(1) {
        let path = std::path::Path::new(&p);
        let name = path.file_name().unwrap().to_string_lossy();
        match dubplate_engine::decode::decode_file(path) {
            Ok(t) => {
                let start = std::time::Instant::now();
                let m = measure(&t.samples, t.channels, t.sample_rate);
                let took = start.elapsed().as_secs_f64() * 1000.0;
                match m {
                    Some(m) => println!(
                        "{name}\t{:.1} LUFS\ttrue peak {:+.1} dB\ttrim {:+.1} dB\t({took:.0} ms for {:.0} s)",
                        m.lufs,
                        m.true_peak_db,
                        auto_trim_db(m, TARGET_LUFS),
                        t.duration_secs
                    ),
                    None => println!("{name}\tnot measurable"),
                }
            }
            Err(e) => println!("{name}\terror {e}"),
        }
    }
}
