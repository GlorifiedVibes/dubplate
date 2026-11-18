//! Scores bar 1 and the Drop sections against drops a person heard: `barscore LABELS`.
//! For each file: does a bar line fall on every labelled drop (within 60 ms), and does a Drop section start on it?
use dubplate_engine::{
    bpm,
    decode::decode_file,
    downbeat,
    structure::{Kind, Structure},
};

fn main() {
    let labels =
        std::fs::read_to_string(std::env::args().nth(1).expect("usage: barscore LABELS")).expect("read the labels");
    let (mut files, mut bars_ok, mut drops, mut drops_ok) = (0, 0, 0, 0);
    for line in labels.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        let mut cols = line.split('\t');
        let path = cols.next().unwrap();
        let times: Vec<f64> = cols.filter_map(|t| t.trim().parse().ok()).collect();
        let Ok(t) = decode_file(std::path::Path::new(path)) else {
            println!("cannot decode {path}");
            continue;
        };
        let a = bpm::analyze_beats_detail(&t.samples, t.channels, t.sample_rate);
        let chunk0 = 256.0 / t.sample_rate as f64;
        // BARSCORE_STEMS=1 separates the stems (no cache is written with DUBPLATE_NO_CACHE=1) and adds the drums vote
        let drums = std::env::var("BARSCORE_STEMS").ok().and_then(|_| {
            let st = dubplate_engine::stems::separate(
                &t.samples,
                t.channels,
                t.sample_rate,
                std::path::Path::new(path),
                None,
            )
            .ok()?;
            Some(downbeat::drums_sign(&st.drums, st.sample_rate, a.phase_chunks * chunk0, 60.0 / a.bpm as f64))
        });
        let Some(d) = downbeat::choose_with_drums(&t.wave, &a, t.sample_rate, drums) else {
            println!("no verdict for {path}");
            continue;
        };
        let chunk = 256.0 / t.sample_rate as f64;
        let (period, down) = (60.0 / a.bpm as f64, d.anchor_chunks * chunk);
        let s = Structure::analyse(&t.wave, period, a.phase_chunks * chunk, d.residue);
        files += 1;
        let mut file_ok = true;
        for &at in &times {
            let beats = (at - down) / period;
            let off_beats = beats.round() as i64;
            let on_bar = off_beats.rem_euclid(4) == 0 && (beats - beats.round()).abs() * period < 0.06;
            let section = s.sections.iter().any(|x| x.kind == Kind::Drop && (x.start_secs - at).abs() < 0.1);
            drops += 1;
            if section {
                drops_ok += 1;
            }
            file_ok &= on_bar;
            println!(
                "  drop at {at:7.2} s: {} beat {} of its bar, {}",
                if on_bar { "bar line" } else { "NOT a bar line:" },
                off_beats.rem_euclid(4) + 1,
                if section { "a Drop section starts there" } else { "no Drop section starts there" }
            );
        }
        if file_ok {
            bars_ok += 1;
        }
        println!("{} (confidence {:.1})", path, d.confidence);
    }
    println!(
        "\nbar lines on every drop: {bars_ok}/{files} files; Drop sections starting on a drop: {drops_ok}/{drops}"
    );
}
