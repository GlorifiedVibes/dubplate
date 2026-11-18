//! What the downbeat signs and the section finder say about audio files: `barcheck FILE...`.
//! Prints the tempo, the old (kick only) and new bar-1 answers, every sign's z-scores, and the sections found.
use dubplate_engine::{bpm, decode::decode_file, downbeat, structure::Structure};

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(t) = decode_file(std::path::Path::new(&path)) else {
            eprintln!("cannot decode {path}");
            continue;
        };
        let a = bpm::analyze_beats_detail(&t.samples, t.channels, t.sample_rate);
        let chunk = 256.0 / t.sample_rate as f64;
        let period = 60.0 / a.bpm as f64;
        println!(
            "\n{path}\n  {:.3} BPM, half-beat ratio {:.2}, kick-only downbeat {:.3} s\n  kick between/on in thirds of the track {:.2} {:.2} {:.2}",
            a.bpm,
            a.half_beat_ratio,
            a.offset_chunks * chunk,
            a.kick_thirds[0],
            a.kick_thirds[1],
            a.kick_thirds[2]
        );
        let Some(d) = downbeat::choose(&t.wave, &a, t.sample_rate) else {
            println!("  no verdict");
            continue;
        };
        let fmt = |z: &[f32; 4]| z.iter().map(|v| format!("{v:+5.1}")).collect::<Vec<_>>().join(" ");
        println!(
            "  new downbeat {:.3} s (residue {}, confidence {:.1}, {} beats from the kick-only answer)",
            d.anchor_chunks * chunk,
            d.residue,
            d.confidence,
            (((d.anchor_chunks - a.offset_chunks) * chunk / period).round() as i64).rem_euclid(4)
        );
        for (name, z) in ["kick ", "bass ", "drops", "bound", "start"].iter().zip(&d.signs) {
            println!("    {name} {}", fmt(z));
        }
        if let Some(be) = dubplate_engine::BeatEnergy::of(&t.wave, period, a.phase_chunks * chunk) {
            let (v, n) = be.boundary_residues();
            println!("  boundaries per residue {v:?} of {n}");
        }
        if let Ok(range) = std::env::var("BARCHECK_DUMP") {
            let v: Vec<f64> = range.split(',').filter_map(|x| x.parse().ok()).collect();
            if let (Some(be), [a, b]) =
                (dubplate_engine::BeatEnergy::of(&t.wave, period, a.phase_chunks * chunk), &v[..])
            {
                let down = d.anchor_chunks * chunk;
                for i in 0..be.len() {
                    if be.secs[i] < *a || be.secs[i] > *b {
                        continue;
                    }
                    let k = ((be.secs[i] - down) / period).round() as i64;
                    let x = be.bands[i];
                    println!(
                        "  {:7.2}s bar {:4} beat {} (n mod 4 = {})  low {:.3} mid {:.3} high {:.3} all {:.3}",
                        be.secs[i],
                        k.div_euclid(4) + 1,
                        k.rem_euclid(4) + 1,
                        be.number(i).rem_euclid(4),
                        x[0],
                        x[1],
                        x[2],
                        x[3]
                    );
                }
            }
        }
        if let Some(be) = dubplate_engine::BeatEnergy::of(&t.wave, period, a.phase_chunks * chunk) {
            let flips: Vec<String> = be
                .bass_state()
                .flips
                .iter()
                .map(|&(i, up)| {
                    format!("{}{:.0}s/r{}", if up { "+" } else { "-" }, be.secs[i], be.number(i).rem_euclid(4))
                })
                .collect();
            println!("  bass flips {}", flips.join(" "));
        }
        let clock = std::time::Instant::now();
        let s = Structure::analyse(&t.wave, period, a.phase_chunks * chunk, d.residue);
        println!("  sections found in {:.1} ms", clock.elapsed().as_secs_f64() * 1000.0);
        println!("  phrase {:?}", s.phrase);
        let down = d.anchor_chunks * chunk;
        for r in dubplate_engine::cue_roles::suggest(&s, down.rem_euclid(4.0 * period)) {
            println!(
                "  pad {} {:7}  {:7.1} s  bar {}",
                (b'A' + r.pad - 1) as char,
                r.name,
                r.secs,
                ((r.secs - down) / (4.0 * period)).round() as i64 + 1
            );
        }
        for x in &s.sections {
            println!(
                "    {:7.1}-{:7.1} s  bar {:4}  {:4} beats  {:6}  energy {:.2}",
                x.start_secs,
                x.end_secs,
                ((x.start_secs - down) / (4.0 * period)).round() as i64 + 1,
                x.beats,
                x.kind.name(),
                x.energy
            );
        }
    }
}
