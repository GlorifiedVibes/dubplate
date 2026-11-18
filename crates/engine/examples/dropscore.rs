//! Scores bar 1 and the Drop sections against times marked by ear, like `scripts/drop_tests/score.py`, but fast enough
//! to try an idea in a second: the first run decodes every file and keeps the per-beat loudness in `CACHE`, later runs
//! read that back and only redo the section finding.
//!
//! `cargo run --release -p dubplate-engine --example dropscore -- samples/drop-tests [CACHE] [-v]`
//!
//! The cache holds the grid as it was when it was written: delete it after changing the tempo, grid or bar-1 code.
use dubplate_engine::structure::{Kind, Structure};
use dubplate_engine::{bpm, decode::decode_file, downbeat, BeatEnergy};
use std::path::{Path, PathBuf};

struct Track {
    period: f64,
    down: f64,
    residue: i64,
    length: f64,
    be: BeatEnergy,
}

fn secs(t: &str) -> Option<f64> {
    let t = t.trim();
    match t.split_once(':') {
        Some((m, s)) => Some(m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?),
        None => t.parse().ok(),
    }
}

fn analyse(path: &Path) -> Option<Track> {
    let t = decode_file(path).ok()?;
    let a = bpm::analyze_beats_detail(&t.samples, t.channels, t.sample_rate);
    let chunk = 256.0 / t.sample_rate as f64;
    let period = 60.0 / a.bpm as f64;
    let d = downbeat::choose(&t.wave, &a, t.sample_rate)?;
    let be = BeatEnergy::of(&t.wave, period, a.phase_chunks * chunk)?;
    Some(Track {
        period,
        down: d.anchor_chunks * chunk,
        residue: d.residue,
        length: t.wave.total_frames as f64 / t.wave.sample_rate as f64,
        be,
    })
}

fn save(t: &Track, to: &Path) {
    let mut s = format!("{} {} {} {} {}\n", t.period, t.down, t.residue, t.length, t.be.first);
    for (x, b) in t.be.secs.iter().zip(&t.be.bands) {
        s += &format!("{x} {} {} {} {}\n", b[0], b[1], b[2], b[3]);
    }
    let _ = std::fs::write(to, s);
}

fn load(from: &Path) -> Option<Track> {
    let text = std::fs::read_to_string(from).ok()?;
    let mut lines = text.lines();
    let h: Vec<f64> = lines.next()?.split(' ').filter_map(|x| x.parse().ok()).collect();
    let mut be = BeatEnergy { first: *h.get(4)? as i64, ..Default::default() };
    for l in lines {
        let v: Vec<f64> = l.split(' ').filter_map(|x| x.parse().ok()).collect();
        be.secs.push(*v.first()?);
        be.bands.push([v[1] as f32, v[2] as f32, v[3] as f32, v[4] as f32]);
    }
    Some(Track { period: h[0], down: h[1], residue: h[2] as i64, length: h[3], be })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v");
    let plain: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let folder = PathBuf::from(plain.first().map(|s| s.as_str()).unwrap_or("samples/drop-tests"));
    let cache = plain.get(1).map(PathBuf::from);
    let labels = std::fs::read(folder.join("labels.tsv")).expect("labels.tsv in the folder");
    let labels = String::from_utf8_lossy(&labels).into_owned();
    let rows: Vec<(String, f64, Vec<f64>)> = labels
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            Some((c.first()?.to_string(), secs(c.get(1)?)?, c.get(2)?.split(',').filter_map(secs).collect()))
        })
        .collect();
    let tracks: Vec<Option<Track>> = std::thread::scope(|s| {
        let hs: Vec<_> = rows
            .iter()
            .map(|(name, _, _)| {
                let file = folder.join(name);
                let kept = cache.as_ref().map(|c| c.join(format!("{name}.beats")));
                s.spawn(move || {
                    if let Some(t) = kept.as_ref().and_then(|k| load(k)) {
                        return Some(t);
                    }
                    let t = analyse(&file)?;
                    if let Some(k) = &kept {
                        let _ = std::fs::create_dir_all(k.parent().unwrap());
                        save(&t, k);
                    }
                    Some(t)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let (mut bar_ok, mut found, mut named, mut flags, mut ours_named) = (0, 0, 0, 0, 0);
    let (mut phrased, mut on_line, mut of_line) = (0, 0, 0);
    for ((name, bar1, drops), t) in rows.iter().zip(&tracks) {
        let Some(t) = t else {
            println!("no verdict: {name}");
            continue;
        };
        let s = Structure::from_beats(&t.be, t.residue, t.length);
        let ours: Vec<f64> = s.sections.iter().filter(|x| x.kind == Kind::Drop).map(|x| x.start_secs).collect();
        let near = |a: f64, b: f64| (a - b).abs() <= 0.6 * t.period;
        let hit: Vec<f64> = drops.iter().copied().filter(|d| ours.iter().any(|o| near(*o, *d))).collect();
        let d = (bar1 - t.down) / t.period;
        bar_ok += (d.abs() < 0.4) as usize;
        found += hit.len();
        named += drops.len();
        flags += ours.len();
        ours_named += ours.iter().filter(|o| drops.iter().any(|d| near(**o, *d))).count();
        let missed: Vec<String> = drops.iter().filter(|d| !hit.contains(d)).map(|d| format!("{d:.1}")).collect();
        let extra: Vec<String> =
            ours.iter().filter(|o| !drops.iter().any(|d| near(**o, *d))).map(|o| format!("{o:.1}")).collect();
        // how many of the drops heard by ear are on the phrase line found (the line is in beat numbers of the grid)
        let mut line = String::from("phrase none");
        if let Some((p, r)) = s.phrase {
            phrased += 1;
            let on = drops
                .iter()
                .filter(|d| {
                    let i = t.be.secs.iter().position(|x| (x - **d).abs() <= 0.6 * t.period);
                    i.is_some_and(|i| (t.be.number(i) - r).rem_euclid(p as i64) == 0)
                })
                .count();
            on_line += on;
            of_line += drops.len();
            line = format!("phrase {} bars, {on}/{} drops on it", p / 4, drops.len());
        }
        let short: String = name.chars().take(28).collect();
        println!(
            "{short:28} bar 1 {d:+6.1}  drops {}/{}  flags {}  missed [{}]  extra [{}]  {line}",
            hit.len(),
            drops.len(),
            ours.len(),
            missed.join(" "),
            extra.join(" ")
        );
        if verbose {
            // where the bass comes in, as bar.beat counted from bar 1, and how much novelty each 8-bar phase collects
            let pos = |i: usize| ((t.be.secs[i] - t.down) / t.period).round() as i64;
            let ups: Vec<String> = t
                .be
                .bass_state()
                .flips
                .iter()
                .filter(|f| f.1)
                .map(|f| {
                    let k = pos(f.0);
                    format!("{}.{} (m8 {})", k.div_euclid(4) + 1, k.rem_euclid(4) + 1, k.div_euclid(4).rem_euclid(8))
                })
                .collect();
            println!("    bass in at bar.beat: {}", ups.join("  "));
            let nov = t.be.combined_novelty();
            let mut score = [0.0f32; 8];
            for (i, v) in nov.iter().enumerate() {
                let k = pos(i);
                if k.rem_euclid(4) == 0 {
                    score[k.div_euclid(4).rem_euclid(8) as usize] += v * v;
                }
            }
            let top = score.iter().cloned().fold(0.0, f32::max).max(1e-9);
            println!("    novelty by 8-bar phase: {}", score.map(|v| format!("{:.2}", v / top)).join(" "));
            // sharper: only the peaks of the narrow novelty (2 bars either side), each at its nearest bar line
            let n8 = t.be.novelty(8);
            let mut peak = [0.0f32; 8];
            for i in 4..n8.len().saturating_sub(4) {
                if n8[i] > 0.3 && (i - 4..=i + 4).all(|j| n8[j] <= n8[i]) {
                    let k = pos(i);
                    let bar = (k as f64 / 4.0).round() as i64;
                    peak[bar.rem_euclid(8) as usize] += n8[i];
                }
            }
            let top = peak.iter().cloned().fold(0.0, f32::max).max(1e-9);
            println!("    peaks by 8-bar phase:   {}", peak.map(|v| format!("{:.2}", v / top)).join(" "));
            println!("    phrase {:?}", s.phrase);
            for x in &s.sections {
                println!(
                    "    {:7.1}-{:7.1} s  bar {:4}  {:4} beats  {:6}  energy {:.2}",
                    x.start_secs,
                    x.end_secs,
                    ((x.start_secs - t.down) / (4.0 * t.period)).round() as i64 + 1,
                    x.beats,
                    x.kind.name(),
                    x.energy
                );
            }
        }
    }
    println!("\nbar 1 within 0.4 beat: {bar_ok}/{}", rows.len());
    println!("phrase line found on {phrased}/{} tracks; drops heard by ear on it: {on_line}/{of_line}", rows.len());
    println!(
        "drops found: {found}/{named}; Drop flags: {flags} ({:.0}% are drops named by ear)",
        100.0 * ours_named as f64 / flags.max(1) as f64
    );
}
