//! How far the beat grid sits from where the beats really start: `gridscore FILE...` or `gridscore LABELS` (the
//! tab-separated file `barscore` reads; its first column is used).
//!
//! The measure is deliberately not the one the analysis uses (which averages the high-passed audio around 400 beats):
//! for every beat of the grid, in a window of 60 ms either side, it finds the loudest point of a 1 ms envelope and
//! walks back down the rise to where it is within 15% of the way up from the quiet before it, so a soft early attack counts and a
//! late swell does not. The answer is the median over the strong beats, for the whole audio and for the part under
//! 200 Hz. Positive = the grid is late (the sound starts before the grid beat).
use dubplate_engine::{bpm, decode::decode_file};

const WINDOW_MS: i64 = 60;

/// Median (ms) and the spread between the quartiles, and how many beats were used.
fn attack_offsets(env: &[f32], sr: f64, first_s: f64, period_s: f64) -> Option<(f64, f64, usize)> {
    let ms = (sr / 1000.0) as usize;
    let n = env.len();
    let mut found: Vec<(f32, f64)> = Vec::new(); // (peak, grid minus attack start in ms)
    let mut k = ((0.5 - first_s) / period_s).ceil().max(0.0) as i64;
    loop {
        let t = ((first_s + k as f64 * period_s) * sr) as i64;
        k += 1;
        let (lo, hi) = (t - WINDOW_MS * ms as i64, t + WINDOW_MS * ms as i64);
        if hi as usize + 1 >= n {
            break;
        }
        if lo < 0 {
            continue;
        }
        let (lo, hi) = (lo as usize, hi as usize);
        let (mut p, mut pv) = (lo, 0.0f32);
        for (i, &v) in env[lo..hi].iter().enumerate() {
            if v > pv {
                pv = v;
                p = lo + i;
            }
        }
        if pv < 1e-3 {
            continue;
        }
        // the floor is the quiet of the first 15 ms of the window: dense music never falls to zero
        let mut floor: Vec<f32> = env[lo..lo + 15 * ms].iter().step_by(ms.max(1)).cloned().collect();
        floor.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let floor = floor[floor.len() / 4];
        let thr = floor + 0.15 * (pv - floor);
        let mut s = p;
        while s > lo && env[s] > thr {
            s -= 1;
        }
        found.push((pv, (t - s as i64) as f64 / sr * 1000.0));
    }
    if found.len() < 40 {
        return None;
    }
    found.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    found.truncate(found.len() / 2);
    let mut offs: Vec<f64> = found.iter().map(|f| f.1).collect();
    offs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = |f: f64| offs[((offs.len() - 1) as f64 * f) as usize];
    Some((q(0.5), q(0.75) - q(0.25), offs.len()))
}

/// 1 ms peak envelope of the rectified audio, optionally low-passed at 200 Hz first.
fn envelope(samples: &[f32], ch: usize, sr: f64, low: bool) -> Vec<f32> {
    let n = samples.len() / ch;
    let a = (-2.0 * std::f64::consts::PI * 200.0 / sr).exp() as f32;
    let mut lp = 0.0f32;
    let ms = (sr / 1000.0) as usize;
    let mut env = vec![0.0f32; n];
    for i in 0..n {
        let m = if ch == 1 { samples[i] } else { (samples[i * ch] + samples[i * ch + 1]) * 0.5 };
        let x = if low {
            lp = a * lp + (1.0 - a) * m;
            lp
        } else {
            m - samples[(i.saturating_sub(1)) * ch]
        };
        env[i] = x.abs();
    }
    // 1 ms running maximum, so one sample's zero crossing does not look like a gap
    let mut out = vec![0.0f32; n];
    for (b, chunk) in env.chunks(ms.max(1)).enumerate() {
        let m = chunk.iter().cloned().fold(0.0, f32::max);
        for o in &mut out[b * ms.max(1)..(b * ms.max(1) + chunk.len())] {
            *o = m;
        }
    }
    out
}

fn main() {
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.len() == 1 && std::fs::metadata(&paths[0]).map(|m| m.len() < 100_000).unwrap_or(false) {
        let labels = std::fs::read_to_string(&paths[0]).unwrap_or_default();
        paths = labels
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split('\t').next().map(str::to_string))
            .collect();
    }
    let mut all: Vec<f64> = Vec::new();
    println!("{:>9} {:>7} {:>9} {:>7}  file", "all ms", "spread", "low ms", "spread");
    for path in &paths {
        let Ok(t) = decode_file(std::path::Path::new(path)) else {
            println!("cannot decode {path}");
            continue;
        };
        let (ch, sr) = (t.channels, t.sample_rate as f64);
        let a = bpm::analyze_beats_detail(&t.samples, ch, t.sample_rate);
        let (period_s, first_s) = (60.0 / a.bpm as f64, a.phase_chunks * 256.0 / sr);
        let full = attack_offsets(&envelope(&t.samples, ch, sr, false), sr, first_s, period_s);
        let low = attack_offsets(&envelope(&t.samples, ch, sr, true), sr, first_s, period_s);
        let show = |r: Option<(f64, f64, usize)>| match r {
            Some((m, s, _)) => (format!("{m:+9.1}"), format!("{s:7.1}")),
            None => ("        -".into(), "      -".into()),
        };
        let (f, fs) = show(full);
        let (l, ls) = show(low);
        if let Some((m, _, _)) = full {
            all.push(m);
        }
        println!(
            "{f} {fs} {l} {ls}  {}",
            std::path::Path::new(path).file_name().map(|s| s.to_string_lossy()).unwrap_or_default()
        );
    }
    if !all.is_empty() {
        all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mean_abs = all.iter().map(|x| x.abs()).sum::<f64>() / all.len() as f64;
        println!(
            "\n{} tracks: median {:+.1} ms, mean |offset| {:.1} ms (all-band)",
            all.len(),
            all[all.len() / 2],
            mean_abs
        );
    }
}
