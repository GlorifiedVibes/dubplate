//! Loads a VST3 effect and runs audio through it: `vst3check BUNDLE.vst3 [AUDIO] [param=value ...]`
//! (`cargo run -p dubplate-engine --release --features vst3-host --example vst3check -- ...`).
//! With no audio it makes a test signal. `param=value` sets a parameter (name or id, value 0..1) before processing.
use dubplate_engine::vst3_host::{inspect, Plugin};

fn rms(a: &[f32]) -> f32 {
    (a.iter().map(|v| v * v).sum::<f32>() / a.len().max(1) as f32).sqrt()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(bundle) = args.first() else {
        eprintln!("usage: vst3check BUNDLE.vst3 [AUDIO] [param=value ...]");
        return;
    };
    let infos = match inspect(std::path::Path::new(bundle)) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("can't read the bundle: {e}");
            return;
        }
    };
    for i in &infos {
        println!("plugin: {} ({}) effect={} class {}", i.name, i.vendor, i.is_effect, i.class_index);
    }
    let Some(info) = infos.iter().find(|i| i.is_effect).or(infos.first()) else {
        eprintln!("no audio plugin in it");
        return;
    };

    let rate = 48000.0;
    let mut p = match Plugin::load(info, rate) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("can't load: {e}");
            return;
        }
    };
    println!(
        "loaded; latency {} frames ({:.1} ms); {} parameters",
        p.latency_frames(),
        p.latency_frames() as f64 / rate * 1000.0,
        p.params().len()
    );
    for q in p.params().iter().take(40) {
        println!("  {:>6}  {:<28} {:<6} steps {:<3} default {:.3}", q.id, q.name, q.units, q.steps, q.default);
    }

    let (audio_arg, sets): (Vec<&String>, Vec<&String>) = args[1..].iter().partition(|a| !a.contains('='));
    let mut audio: Vec<f32> = match audio_arg.first() {
        Some(f) => {
            let t = dubplate_engine::decode::decode_file(std::path::Path::new(f)).expect("decode");
            let ch = t.channels as usize;
            let frames = (t.samples.len() / ch).min(t.sample_rate as usize * 20);
            (0..frames).flat_map(|i| [t.samples[i * ch], t.samples[i * ch + ch.min(2) - 1]]).collect()
        }
        None => (0..48000 * 4)
            .flat_map(|i| {
                let v = (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / 48000.0).sin() * 0.3;
                [v, v]
            })
            .collect(),
    };
    for s in sets {
        let (k, v) = s.split_once('=').unwrap();
        let id = k
            .parse::<u32>()
            .ok()
            .or_else(|| p.params().iter().find(|q| q.name.to_lowercase().contains(&k.to_lowercase())).map(|q| q.id));
        match (id, v.parse::<f64>()) {
            (Some(id), Ok(v)) => {
                p.set_param(id, v);
                println!("set {k} ({id}) = {v}  -> {}", p.param_text(id, v));
            }
            _ => println!("can't set {s}"),
        }
    }
    let before = audio.clone();
    let t0 = std::time::Instant::now();
    p.process(&mut audio);
    let took = t0.elapsed();
    let secs = audio.len() as f64 / 2.0 / rate;
    println!(
        "processed {:.1} s of audio in {:.0} ms ({:.1}% of one core)",
        secs,
        took.as_secs_f64() * 1000.0,
        took.as_secs_f64() / secs * 100.0
    );
    let diff: Vec<f32> = audio.iter().zip(&before).map(|(a, b)| a - b).collect();
    println!(
        "rms in {:.4}  out {:.4}  difference {:.4}  silent: {}",
        rms(&before),
        rms(&audio),
        rms(&diff),
        rms(&audio) < 1e-6
    );
    // a second pass: the plugin must keep running without trouble
    p.process(&mut audio);
    println!("second pass fine; unload...");
    drop(p);
    println!("unloaded cleanly");
}
