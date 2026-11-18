//! Plays 20 s of a file through a deck at a tempo with key lock on and off, and prints how the sound differs:
//! how far the spectrum has moved up, in cents (a stand-in for pitch: key lock keeps it, a plain speed change raises
//! it), the level,
//! the worst jump between samples (clicks) and the processing time.
//! `cargo run -p dubplate-engine --release --example keylockcheck -- FILE [TEMPO]`
use dubplate_engine::{create_deck_pair, DeckCommand, DeckShared};
use std::sync::Arc;

/// Long-term average spectrum, in dB, on a log frequency axis (100 Hz - 8 kHz, 24 points per octave).
fn ltas(x: &[f32], rate: f32) -> Vec<f64> {
    use rustfft::{num_complex::Complex, FftPlanner};
    let n = 8192;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let mut acc = vec![0f64; n / 2];
    let mut count = 0;
    let mut i = 0;
    while i + n <= x.len() {
        let mut b: Vec<Complex<f32>> = (0..n)
            .map(|k| {
                Complex::new(x[i + k] * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * k as f32 / n as f32).cos()), 0.0)
            })
            .collect();
        fft.process(&mut b);
        for k in 0..n / 2 {
            acc[k] += (b[k].norm() as f64).powi(2);
        }
        count += 1;
        i += n / 2;
    }
    let pts = (24.0 * (8000.0f64 / 100.0).log2()) as usize;
    (0..pts)
        .map(|j| {
            let f = 100.0 * 2f64.powf(j as f64 / 24.0);
            let k = (f * n as f64 / rate as f64) as usize;
            // average a few neighbouring bins so the curve is smooth
            let lo = k.saturating_sub(1);
            let hi = (k + 2).min(n / 2);
            10.0 * (acc[lo..hi].iter().sum::<f64>() / (hi - lo) as f64 / count.max(1) as f64 + 1e-12).log10()
        })
        .collect()
}

/// How far `b`'s spectrum sits above `a`'s on the frequency axis, in cents.
fn shift_cents(a: &[f64], b: &[f64]) -> f64 {
    let ma = a.iter().sum::<f64>() / a.len() as f64;
    let mb = b.iter().sum::<f64>() / b.len() as f64;
    let mut best = (f64::MIN, 0i32);
    for lag in -12..=12i32 {
        let mut c = 0.0;
        for i in 0..a.len() as i32 {
            let j = i + lag;
            if j >= 0 && (j as usize) < b.len() {
                c += (a[i as usize] - ma) * (b[j as usize] - mb);
            }
        }
        if c > best.0 {
            best = (c, lag);
        }
    }
    best.1 as f64 * 50.0 // 24 points per octave = 50 cents each
}

fn render(
    track: &Arc<dubplate_engine::DecodedTrack>,
    key_lock: bool,
    tempo: f32,
    start: f64,
    secs: usize,
) -> (Vec<f32>, f64) {
    let rate = track.sample_rate;
    let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
    p.set_output_sample_rate(rate);
    h.send(DeckCommand::Load(Arc::clone(track)));
    h.send(DeckCommand::SetKeyLock(key_lock));
    h.send(DeckCommand::SetTempo(tempo));
    h.send(DeckCommand::Seek(start / track.duration_secs));
    h.send(DeckCommand::Play);
    p.drain_commands();
    let frames = rate as usize * secs;
    let t0 = std::time::Instant::now();
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        out.push(p.next_frame().0);
    }
    (out, t0.elapsed().as_secs_f64())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("file");
    let tempo: f32 = args.next().map(|t| t.parse().unwrap()).unwrap_or(1.08);
    let track = Arc::new(dubplate_engine::decode::decode_file(std::path::Path::new(&path)).unwrap());
    let rate = track.sample_rate;
    let start = (track.duration_secs * 0.3).min(60.0);
    let secs = 20usize;
    // the same stretch of the song, played at its own speed
    let (reference, _) = render(&track, false, 1.0, start + 2.0 * tempo as f64, (secs as f32 * tempo) as usize - 2);
    let ref_ltas = ltas(&reference, rate as f32);
    for key_lock in [false, true] {
        let (out, took) = render(&track, key_lock, tempo, start, secs);
        let tail = &out[rate as usize * 2..];
        let rms = (tail.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        let jump = tail.windows(2).fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
        println!("key lock {:<5} tempo {tempo}: pitch {:+.0} cents vs the original  (a speed change of that much would be {:+.0})  rms {:.3}  worst step {:.3}  cpu {:.1}% of real time",
            key_lock, shift_cents(&ref_ltas, &ltas(tail, rate as f32)), 1200.0 * (tempo as f64).log2(), rms, jump, took / secs as f64 * 100.0);
    }
}
