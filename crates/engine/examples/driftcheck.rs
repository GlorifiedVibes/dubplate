//! Does the tempo hold? For every beat of the grid, how far the kick's attack is from it, as the median of each
//! 16-bar block down the track: `driftcheck FILE...`. A straight slope means the tempo is slightly out, a bend or
//! a step means the track itself changes speed or skips, a flat line means the grid holds.
//! Positive = the sound is after the grid beat (the grid runs early).
use dubplate_engine::{bpm, decode::decode_file};

/// 1 ms peak envelope of the sample-to-sample difference (the clicks of attacks), as `gridscore` uses.
fn envelope(samples: &[f32], ch: usize, sr: f64) -> (Vec<f32>, f64) {
    let n = samples.len() / ch;
    let ms = ((sr / 1000.0) as usize).max(1);
    let mono = |i: usize| if ch == 1 { samples[i] } else { (samples[i * ch] + samples[i * ch + 1]) * 0.5 };
    let mut out = Vec::with_capacity(n / ms + 1);
    let mut peak = 0.0f32;
    for i in 1..n {
        peak = peak.max((mono(i) - mono(i - 1)).abs());
        if (i + 1) % ms == 0 {
            out.push(peak);
            peak = 0.0;
        }
    }
    (out, sr / ms as f64)
}

fn main() {
    for path in std::env::args().skip(1) {
        let Ok(t) = decode_file(std::path::Path::new(&path)) else {
            println!("cannot decode {path}");
            continue;
        };
        let sr = t.sample_rate as f64;
        let a = bpm::analyze_beats_detail(&t.samples, t.channels, t.sample_rate);
        let (period, first) = (60.0 / a.bpm as f64, a.phase_chunks * 256.0 / sr);
        let (env, rate) = envelope(&t.samples, t.channels, sr);
        println!("\n{path}\n  {:.3} BPM", a.bpm);
        // DRIFT_PROFILE=1: the average beat, in 5 ms steps from the grid beat on: the clicks, and the level under
        // 150 Hz (the kick's body), each against its own largest value
        if std::env::var("DRIFT_PROFILE").is_ok() {
            let bins = (period / 0.005) as usize;
            let (mut click, mut low) = (vec![0.0f64; bins], vec![0.0f64; bins]);
            let al = (-2.0 * std::f64::consts::PI * 150.0 / sr).exp() as f32;
            let mut lp = 0.0f32;
            let n = t.samples.len() / t.channels;
            let mut body = vec![0.0f32; env.len() + 1];
            for i in 0..n {
                let m = (t.samples[i * t.channels] + t.samples[i * t.channels + t.channels - 1]) * 0.5;
                lp = al * lp + (1.0 - al) * m;
                let j = (i as f64 / sr * rate) as usize;
                if j < body.len() {
                    body[j] = body[j].max(lp.abs());
                }
            }
            for (i, &v) in env.iter().enumerate() {
                let ph = ((i as f64 / rate - first) / period).rem_euclid(1.0);
                let b = ((ph * bins as f64) as usize).min(bins - 1);
                click[b] += v as f64;
                low[b] += body[i] as f64;
            }
            let (mc, ml) = (click.iter().cloned().fold(0.0, f64::max), low.iter().cloned().fold(0.0, f64::max));
            for b in 0..bins {
                let bar = |v: f64| "#".repeat((v * 40.0) as usize);
                println!("  {:4} ms  click {:40}  low {}", b * 5, bar(click[b] / mc), bar(low[b] / ml));
            }
        }
        // per beat: the loudest point within 60 ms either side, walked back down its rise to where the attack starts
        let mut offs: Vec<(i64, f64, f32)> = Vec::new();
        let mut k = ((0.2 - first) / period).ceil() as i64;
        loop {
            let c = (first + k as f64 * period) * rate;
            let (lo, hi) = ((c - 0.06 * rate) as usize, (c + 0.06 * rate) as usize);
            if hi + 1 >= env.len() {
                break;
            }
            let (mut pv, mut p) = (0.0f32, lo);
            for (i, &v) in env.iter().enumerate().take(hi).skip(lo) {
                if v > pv {
                    pv = v;
                    p = i;
                }
            }
            let mut floor: Vec<f32> = env[lo..lo + 15].to_vec();
            floor.sort_by(f32::total_cmp);
            let thr = floor[3] + 0.15 * (pv - floor[3]);
            let mut at = p;
            while at > lo && env[at] > thr {
                at -= 1;
            }
            offs.push((k, (at as f64 - c) / rate * 1000.0, pv));
            k += 1;
        }
        let mut rises: Vec<f32> = offs.iter().map(|o| o.2).collect();
        rises.sort_by(f32::total_cmp);
        let strong = rises[rises.len() * 6 / 10];
        for block in offs.chunks(64) {
            let mut v: Vec<f64> = block.iter().filter(|o| o.2 >= strong).map(|o| o.1).collect();
            if v.len() < 12 {
                println!("  {:6.1} s   (too few kicks: {})", first + block[0].0 as f64 * period, v.len());
                continue;
            }
            v.sort_by(f64::total_cmp);
            println!(
                "  {:6.1} s  {:+6.1} ms  (quartiles {:+.1} {:+.1}, {} kicks)",
                first + block[0].0 as f64 * period,
                v[v.len() / 2],
                v[v.len() / 4],
                v[v.len() * 3 / 4],
                v.len()
            );
        }
    }
}
