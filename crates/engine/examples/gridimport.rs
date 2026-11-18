//! Compares a rekordbox grid with the audio it was made for: `gridimport ANLZ0000.DAT AUDIO`.
//! Prints the tempo and downbeat in the file, how far the kicks sit from it, what the importer would do, and
//! how Dubplate's own analysis of the same audio compares.
use dubplate_engine::{anlz_read, bpm, decode::decode_file};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dat, audio] = &args[..] else {
        eprintln!("usage: gridimport ANLZ0000.DAT AUDIO");
        return;
    };
    let rb = anlz_read::read_dat(&std::fs::read(dat).expect("read the analysis file"))
        .expect("not a rekordbox analysis file with a grid");
    let t = rb.tempo().expect("no tempo in the grid");
    println!(
        "rekordbox: {} | {} beats, {:.4} BPM{}, downbeat {:.3} s, last beat {:.1} s",
        rb.file_name(),
        rb.beats.len(),
        t.bpm,
        if t.dynamic { " (dynamic: longest stretch)" } else { "" },
        t.downbeat_secs,
        rb.last_secs()
    );
    let tr = decode_file(std::path::Path::new(audio)).expect("decode the audio");
    println!(
        "audio: {:.1} s, Dubplate finds {:.4} BPM, first downbeat {:.3} s",
        tr.duration_secs,
        tr.bpm,
        tr.beat_offset * 256.0 / tr.sample_rate as f64
    );
    let (curve, frame) = bpm::onset_curve_full(&tr.samples, tr.channels as usize, tr.sample_rate);
    let g = rb.to_grid(0.0, tr.sample_rate).unwrap();
    match bpm::measure_import_offset(&curve, frame, g.bpm, g.offset) {
        None => println!("offset: too few clear kicks to measure"),
        Some(m) => {
            println!(
                "kicks are {:+.1} ms from rekordbox's beats (scatter {:.1} ms, {} beats)",
                m.median_ms, m.spread_ms, m.hits
            );
            let shift = bpm::mp3_offset_shift(&m, tr.sample_rate);
            println!(
                "importer: {}",
                shift.map_or("keeps the grid exactly as rekordbox has it".to_string(), |s| format!(
                    "moves the grid {:+.1} ms (looks like an MP3 start offset)",
                    s * 1000.0
                ))
            );
            let g2 = rb.to_grid(shift.unwrap_or(0.0), tr.sample_rate).unwrap();
            let q = bpm::grid_quality(&curve, frame, g2.bpm, g2.offset);
            println!("result: {:?}, scatter {:.1} ms, drift {:+.1} ms", q.verdict, q.spread_ms, q.drift_ms);
        }
    }
    let own = bpm::grid_quality(&curve, frame, tr.bpm, tr.beat_offset);
    println!("Dubplate's own grid: {:?}, scatter {:.1} ms, drift {:+.1} ms", own.verdict, own.spread_ms, own.drift_ms);
    // beat phase difference between the two grids, ms (rekordbox minus Dubplate, within half a beat)
    let p = 60.0 / g.bpm as f64;
    let d = (t.downbeat_secs - tr.beat_offset * 256.0 / tr.sample_rate as f64).rem_euclid(p);
    let d = if d > p / 2.0 { d - p } else { d };
    println!("rekordbox's beats are {:+.1} ms from Dubplate's (mod one beat)", d * 1000.0);
}
