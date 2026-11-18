//! A live microphone in the main mix: a stream of mono samples comes in (from a sound card input now, from
//! phones later), is cleaned up, given some reverb and added to the music in the audio callback.
//!
//! The input and the output are separate streams with their own clocks, so a small buffer sits between
//! them. It is kept short for low delay; if the input can't keep up it grows by itself rather than
//! crackling (see `MicProcessor::pull`).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};

/// How the microphone is set, shared by the window, the phones and the audio callback.
pub struct MicControls {
    /// The microphone is part of the mix.
    pub on: AtomicBool,
    /// Voice level, f32 bits, 0 to 2 (1 = as it comes in).
    pub volume: AtomicU32,
    /// How much reverb is added, f32 bits, 0 to 1.
    pub reverb: AtomicU32,
    /// Sample rate of what is being fed in (0 while nothing is).
    pub source_rate: AtomicU32,
    /// Loudest sample lately, f32 bits, for a meter.
    pub peak: AtomicU32,
    /// Times the input ran dry and the voice cut out.
    pub dropouts: AtomicU32,
    /// The delay held in the buffer between input and mix, in microseconds.
    pub buffer_us: AtomicU32,
    /// Sample rate of the tap (the output's), once known.
    pub tap_rate: AtomicU32,
}

impl Default for MicControls {
    fn default() -> Self {
        Self {
            on: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            reverb: AtomicU32::new(0.25f32.to_bits()),
            source_rate: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            dropouts: AtomicU32::new(0),
            buffer_us: AtomicU32::new(0),
            tap_rate: AtomicU32::new(0),
        }
    }
}

impl MicControls {
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }
    pub fn set_reverb(&self, v: f32) {
        self.reverb.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
    pub fn reverb(&self) -> f32 {
        f32::from_bits(self.reverb.load(Ordering::Relaxed))
    }
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }
    pub fn buffer_ms(&self) -> f32 {
        self.buffer_us.load(Ordering::Relaxed) as f32 / 1000.0
    }
}

// --- reverb ---------------------------------------------------------------------------------------------

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
    feedback: f32,
    damp: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], i: 0, store: 0.0, feedback: 0.82, damp: 0.25 }
    }
    fn process(&mut self, x: f32) -> f32 {
        let out = self.buf[self.i];
        self.store = out * (1.0 - self.damp) + self.store * self.damp;
        self.buf[self.i] = x + self.store * self.feedback;
        self.i = (self.i + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], i: 0 }
    }
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        self.buf[self.i] = x + b * 0.5;
        self.i = (self.i + 1) % self.buf.len();
        b - x
    }
}

/// A small room: a mono voice in, a stereo sound out (a Freeverb-style arrangement of four comb filters
/// and two all-pass filters per side).
pub struct Reverb {
    left: Vec<Comb>,
    right: Vec<Comb>,
    ap_left: [Allpass; 2],
    ap_right: [Allpass; 2],
}

impl Reverb {
    pub fn new(rate: u32) -> Self {
        let s = rate as f32 / 44100.0;
        let len = |n: usize| (n as f32 * s) as usize;
        let combs = [1116usize, 1188, 1277, 1356];
        let spread = 23;
        Self {
            left: combs.iter().map(|&n| Comb::new(len(n))).collect(),
            right: combs.iter().map(|&n| Comb::new(len(n + spread))).collect(),
            ap_left: [Allpass::new(len(556)), Allpass::new(len(441))],
            ap_right: [Allpass::new(len(556 + spread)), Allpass::new(len(441 + spread))],
        }
    }

    /// Only the reverberation, to be mixed with the dry voice.
    pub fn process(&mut self, x: f32) -> (f32, f32) {
        let input = x * 0.25;
        let mut l: f32 = self.left.iter_mut().map(|c| c.process(input)).sum();
        let mut r: f32 = self.right.iter_mut().map(|c| c.process(input)).sum();
        for a in &mut self.ap_left {
            l = a.process(l);
        }
        for a in &mut self.ap_right {
            r = a.process(r);
        }
        (l, r)
    }
}

// --- the part that runs in the audio callback ----------------------------------------------------------

/// Largest buffer the processor will grow to when the input keeps running dry.
const MAX_TARGET_SECS: f64 = 0.15;
const START_TARGET_SECS: f64 = 0.02;

pub struct MicProcessor {
    rx: ringbuf::HeapCons<f32>,
    controls: Arc<MicControls>,
    out_rate: f64,
    reverb: Reverb,
    /// Where the buffer is kept, in seconds of input.
    target_secs: f64,
    pos: f64,
    prev: f32,
    next: f32,
    primed: bool,
    /// One-pole high-pass, to keep rumble and handling noise (and the lowest feedback) out.
    hp_x: f32,
    hp_y: f32,
    hp_k: f32,
    gain: f32,
    peak_acc: f32,
    peak_count: u32,
    /// A copy of the voice as it comes in (filtered, before the level), for listening to its pitch.
    tap: Option<ringbuf::HeapProd<f32>>,
}

impl MicProcessor {
    fn new(rx: ringbuf::HeapCons<f32>, controls: Arc<MicControls>) -> Self {
        let mut p = Self {
            rx,
            controls,
            out_rate: 48000.0,
            reverb: Reverb::new(48000),
            target_secs: START_TARGET_SECS,
            pos: 1.0,
            prev: 0.0,
            next: 0.0,
            primed: false,
            hp_x: 0.0,
            hp_y: 0.0,
            hp_k: 0.0,
            gain: 0.0,
            peak_acc: 0.0,
            peak_count: 0,
            tap: None,
        };
        p.set_output_rate(48000);
        p
    }

    /// Called once the output device (and so its rate) is known.
    pub fn set_output_rate(&mut self, rate: u32) {
        self.out_rate = rate.max(1) as f64;
        self.controls.tap_rate.store(rate, Ordering::Relaxed);
        self.reverb = Reverb::new(rate.max(1));
        // High-pass at about 110 Hz.
        self.hp_k = (-2.0 * std::f32::consts::PI * 110.0 / rate.max(1) as f32).exp();
    }

    fn fill_secs(&self, source_rate: f64) -> f64 {
        self.rx.occupied_len() as f64 / source_rate
    }

    /// The next input sample at the output's rate. Follows the buffer's fill a little, so the two clocks
    /// drifting apart never grows the delay without bound or runs the buffer dry.
    fn pull(&mut self, source_rate: f64) -> Option<f32> {
        if !self.primed {
            if self.fill_secs(source_rate) < self.target_secs {
                return None;
            }
            self.primed = true;
            self.pos = 1.0;
        }
        let fill = self.fill_secs(source_rate);
        // Far too much buffered (a stall upstream, then a burst): skip ahead rather than play it late.
        if fill > self.target_secs * 3.0 + 0.03 {
            let keep = (self.target_secs * source_rate) as usize;
            let drop = self.rx.occupied_len().saturating_sub(keep);
            self.rx.skip(drop);
        }
        let error = (fill - self.target_secs) / self.target_secs.max(0.001);
        let step = source_rate / self.out_rate * (1.0 + (error * 0.01).clamp(-0.01, 0.01));
        self.pos += step;
        while self.pos >= 1.0 {
            match self.rx.try_pop() {
                Some(s) => {
                    self.prev = self.next;
                    self.next = s;
                }
                None => {
                    // Ran dry: cut the voice and wait for a bigger cushion next time.
                    self.primed = false;
                    self.target_secs = (self.target_secs * 1.5).min(MAX_TARGET_SECS);
                    self.controls.dropouts.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            }
            self.pos -= 1.0;
        }
        let t = self.pos as f32;
        Some(self.prev + (self.next - self.prev) * t)
    }

    /// One stereo frame to add to the mix.
    pub fn next_frame(&mut self) -> (f32, f32) {
        let c = &self.controls;
        let rate = c.source_rate.load(Ordering::Relaxed);
        if !c.on.load(Ordering::Relaxed) || rate == 0 {
            if self.primed || self.rx.occupied_len() > 0 {
                self.rx.skip(self.rx.occupied_len());
                self.primed = false;
            }
            self.gain = 0.0;
            return self.finish_peak(0.0, (0.0, 0.0));
        }
        let wanted = c.volume.load(Ordering::Relaxed);
        let wet = f32::from_bits(c.reverb.load(Ordering::Relaxed)).clamp(0.0, 1.0);
        // The level eases to its new value so moving the slider doesn't click.
        self.gain += (f32::from_bits(wanted).clamp(0.0, 2.0) - self.gain) * 0.002;

        let dry = match self.pull(rate as f64) {
            Some(raw) => {
                let y = raw - self.hp_x + self.hp_k * self.hp_y;
                self.hp_x = raw;
                self.hp_y = y;
                if let Some(t) = self.tap.as_mut() {
                    let _ = t.try_push(y);
                }
                y * self.gain
            }
            None => {
                if let Some(t) = self.tap.as_mut() {
                    let _ = t.try_push(0.0);
                }
                0.0
            }
        };
        let (rl, rr) = self.reverb.process(dry);
        let out = (dry + rl * wet * 0.6, dry + rr * wet * 0.6);
        self.finish_peak(dry.abs(), out)
    }

    fn finish_peak(&mut self, level: f32, out: (f32, f32)) -> (f32, f32) {
        self.peak_acc = self.peak_acc.max(level);
        self.peak_count += 1;
        if self.peak_count >= 1024 {
            self.controls.peak.store(self.peak_acc.to_bits(), Ordering::Relaxed);
            let secs = if self.primed {
                self.fill_secs(self.controls.source_rate.load(Ordering::Relaxed).max(1) as f64)
            } else {
                0.0
            };
            self.controls.buffer_us.store((secs * 1e6) as u32, Ordering::Relaxed);
            self.peak_acc = 0.0;
            self.peak_count = 0;
        }
        out
    }
}

// --- the input side ------------------------------------------------------------------------------------

/// Where microphone samples are pushed. Cloneable; whoever has a stream of mono samples feeds it.
#[derive(Clone)]
pub struct MicFeed {
    prod: Arc<Mutex<ringbuf::HeapProd<f32>>>,
    controls: Arc<MicControls>,
}

impl MicFeed {
    /// Adds mono samples recorded at `rate`. Never blocks; if the mix is not keeping up the extra is dropped.
    pub fn push(&self, rate: u32, samples: impl Iterator<Item = f32>) {
        if self.controls.source_rate.load(Ordering::Relaxed) != rate {
            self.controls.source_rate.store(rate, Ordering::Relaxed);
        }
        if let Ok(mut p) = self.prod.try_lock() {
            for s in samples {
                let _ = p.try_push(s);
            }
        }
    }
}

/// An open sound card input; the microphone stops being fed when this is dropped.
pub struct MicInput {
    _stream: cpal::Stream,
    pub device_name: String,
    pub sample_rate: u32,
}

/// Makes the processor for the mix and the feed that fills it.
pub fn mic_bus(controls: Arc<MicControls>) -> (MicFeed, MicProcessor) {
    let (prod, cons) = ringbuf::HeapRb::<f32>::new(48000).split();
    (MicFeed { prod: Arc::new(Mutex::new(prod)), controls: Arc::clone(&controls) }, MicProcessor::new(cons, controls))
}

/// A listener on the voice in the mix: one sample per output frame while the microphone is on, at the output's rate.
#[derive(Clone)]
pub struct MicTap {
    cons: Arc<Mutex<ringbuf::HeapCons<f32>>>,
    controls: Arc<MicControls>,
}

impl MicTap {
    /// Moves what has arrived since the last call onto `out`; the number of samples.
    pub fn drain(&self, out: &mut Vec<f32>) -> usize {
        let Ok(mut c) = self.cons.lock() else { return 0 };
        let n = c.occupied_len();
        out.extend(std::iter::from_fn(|| c.try_pop()).take(n));
        n
    }
    /// Sample rate of what `drain` gives (0 until the output is open).
    pub fn rate(&self) -> u32 {
        self.controls.tap_rate.load(Ordering::Relaxed)
    }
}

/// As `mic_bus`, with a tap on the voice as well.
pub fn mic_bus_tapped(controls: Arc<MicControls>) -> (MicFeed, MicProcessor, MicTap) {
    let (feed, mut processor) = mic_bus(Arc::clone(&controls));
    let (prod, cons) = ringbuf::HeapRb::<f32>::new(96_000).split();
    processor.tap = Some(prod);
    (feed, processor, MicTap { cons: Arc::new(Mutex::new(cons)), controls })
}

/// Names of the sound card inputs.
pub fn list_input_devices() -> Vec<String> {
    cpal::default_host().input_devices().map(|d| d.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

impl MicFeed {
    /// Starts listening on a sound card input (`None` = the system default).
    pub fn open_input(&self, device: Option<&str>) -> Result<MicInput, String> {
        let host = cpal::default_host();
        let dev = match device {
            Some(n) => host
                .input_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.name().ok().as_deref() == Some(n))
                .ok_or_else(|| format!("microphone '{n}' not found"))?,
            None => host.default_input_device().ok_or("no microphone found")?,
        };
        let name = dev.name().unwrap_or_else(|_| "Unknown".into());
        let supported = dev.default_input_config().map_err(|e| format!("{name}: {e}"))?;
        let (rate, channels) = (supported.sample_rate().0, supported.channels() as usize);
        let config = cpal::StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => self.stream::<f32>(&dev, &config, rate, channels),
            cpal::SampleFormat::I16 => self.stream::<i16>(&dev, &config, rate, channels),
            cpal::SampleFormat::U16 => self.stream::<u16>(&dev, &config, rate, channels),
            other => return Err(format!("{name}: can't read {other:?} samples")),
        }
        .map_err(|e| format!("{name}: {e}"))?;
        stream.play().map_err(|e| format!("{name}: {e}"))?;
        log::info!("microphone: {name} @ {rate}Hz ({channels}ch)");
        Ok(MicInput { _stream: stream, device_name: name, sample_rate: rate })
    }

    fn stream<T: cpal::SizedSample + Send + 'static>(
        &self,
        dev: &cpal::Device,
        config: &cpal::StreamConfig,
        rate: u32,
        channels: usize,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        f32: cpal::FromSample<T>,
    {
        let feed = self.clone();
        dev.build_input_stream(
            config,
            move |data: &[T], _| {
                // The first channel is the microphone on a mono or a stereo input.
                feed.push(rate, data.chunks_exact(channels).map(|f| <f32 as cpal::FromSample<T>>::from_sample_(f[0])));
            },
            |err| log::error!("microphone stream error: {err}"),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus(rate_in: u32, out: u32) -> (MicFeed, MicProcessor, Arc<MicControls>) {
        let c = Arc::new(MicControls::default());
        let (feed, mut p) = mic_bus(Arc::clone(&c));
        p.set_output_rate(out);
        c.on.store(true, Ordering::Relaxed);
        c.set_reverb(0.0);
        c.gain_now(&mut p);
        let _ = rate_in;
        (feed, p, c)
    }

    impl MicControls {
        /// Test helper: lets the level smoothing settle.
        fn gain_now(&self, p: &mut MicProcessor) {
            p.gain = self.volume();
        }
    }

    fn sine(n: usize, freq: f32, rate: f32, amp: f32) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * freq * std::f32::consts::TAU / rate).sin() * amp).collect()
    }

    fn render(p: &mut MicProcessor, frames: usize) -> Vec<(f32, f32)> {
        (0..frames).map(|_| p.next_frame()).collect()
    }

    /// Feeds `signal` the way a sound card would, 10 ms at a time, while rendering the same stretch of output.
    fn stream(feed: &MicFeed, p: &mut MicProcessor, rate_in: u32, out_rate: u32, signal: &[f32]) -> Vec<(f32, f32)> {
        let (chunk_in, chunk_out) = (rate_in as usize / 100, out_rate as usize / 100);
        let mut out = Vec::new();
        for block in signal.chunks(chunk_in) {
            feed.push(rate_in, block.iter().copied());
            out.extend(render(p, chunk_out));
        }
        out
    }

    #[test]
    fn the_tap_hears_the_voice_before_the_level_and_the_reverb() {
        let c = Arc::new(MicControls::default());
        let (feed, mut p, tap) = mic_bus_tapped(Arc::clone(&c));
        p.set_output_rate(48000);
        assert_eq!(tap.rate(), 48000);
        c.on.store(true, Ordering::Relaxed);
        c.set_volume(0.0); // the level doesn't change what the tap hears
        c.set_reverb(1.0);
        let sig = sine(48000, 1000.0, 48000.0, 0.5);
        let out = stream(&feed, &mut p, 48000, 48000, &sig);
        assert!(out.iter().all(|f| f.0.abs() < 1e-3), "muted in the mix");
        let mut heard = Vec::new();
        let n = tap.drain(&mut heard);
        assert_eq!((n, heard.len()), (48000, 48000), "one sample per output frame");
        let peak = heard[10_000..].iter().map(|v| v.abs()).fold(0.0, f32::max);
        assert!((peak - 0.5).abs() < 0.05, "peak {peak}");
        assert_eq!(tap.drain(&mut heard), 0, "drained");
        // Off: nothing more arrives.
        c.on.store(false, Ordering::Relaxed);
        render(&mut p, 1000);
        assert_eq!(tap.drain(&mut heard), 0);
    }

    #[test]
    fn nothing_is_heard_while_the_microphone_is_off() {
        let (feed, mut p, c) = bus(48000, 48000);
        c.on.store(false, Ordering::Relaxed);
        feed.push(48000, sine(4800, 440.0, 48000.0, 0.5).into_iter());
        assert!(render(&mut p, 4800).iter().all(|&(l, r)| l == 0.0 && r == 0.0));
    }

    #[test]
    fn a_voice_comes_through_at_the_chosen_level_once_the_buffer_has_filled() {
        let (feed, mut p, _c) = bus(48000, 48000);
        let out = stream(&feed, &mut p, 48000, 48000, &sine(48000, 1000.0, 48000.0, 0.5));
        assert!(out[..300].iter().all(|f| f.0 == 0.0), "silent until the cushion is there");
        let peak = out[5000..].iter().map(|f| f.0.abs()).fold(0.0, f32::max);
        assert!((peak - 0.5).abs() < 0.05, "peak {peak}");
        assert!(out.iter().all(|f| f.0 == f.1), "no reverb: both sides alike");
    }

    #[test]
    fn the_volume_scales_the_voice() {
        let (feed, mut p, c) = bus(48000, 48000);
        c.set_volume(0.5);
        c.gain_now(&mut p);
        let out = stream(&feed, &mut p, 48000, 48000, &sine(48000, 1000.0, 48000.0, 0.8));
        let peak = out[5000..].iter().map(|f| f.0.abs()).fold(0.0, f32::max);
        assert!((peak - 0.4).abs() < 0.04, "peak {peak}");
    }

    #[test]
    fn rumble_is_removed() {
        let (feed, mut p, _c) = bus(48000, 48000);
        let out = stream(&feed, &mut p, 48000, 48000, &vec![0.3; 48000]); // a steady offset
        assert!(out[30000..].iter().all(|f| f.0.abs() < 0.01), "DC must be gone");
    }

    #[test]
    fn a_different_input_rate_is_converted() {
        let (feed, mut p, c) = bus(44100, 48000);
        let out = stream(&feed, &mut p, 44100, 48000, &sine(44100, 1000.0, 44100.0, 0.5));
        assert_eq!(c.source_rate.load(Ordering::Relaxed), 44100);
        assert_eq!(out.len(), 48000);
        // A 1 kHz tone stays a 1 kHz tone: count rising zero crossings in the second half.
        let tail: Vec<f32> = out[24000..].iter().map(|f| f.0).collect();
        let crossings = tail.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((crossings as i32 - 500).abs() <= 6, "crossings {crossings}");
        assert_eq!(c.dropouts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn running_dry_cuts_out_counts_it_and_grows_the_cushion() {
        let (feed, mut p, c) = bus(48000, 48000);
        feed.push(48000, sine(2000, 440.0, 48000.0, 0.5).into_iter());
        render(&mut p, 6000);
        assert!(c.dropouts.load(Ordering::Relaxed) >= 1);
        assert!(p.target_secs > START_TARGET_SECS && p.target_secs <= MAX_TARGET_SECS);
        // It recovers once more arrives.
        feed.push(48000, sine(20000, 440.0, 48000.0, 0.5).into_iter());
        let out = render(&mut p, 10000);
        assert!(out.iter().any(|f| f.0.abs() > 0.1));
    }

    #[test]
    fn a_burst_after_a_stall_is_skipped_not_played_late() {
        let (feed, mut p, _c) = bus(48000, 48000);
        feed.push(48000, sine(1000, 440.0, 48000.0, 0.5).into_iter());
        render(&mut p, 100); // primes
        feed.push(48000, sine(40000, 440.0, 48000.0, 0.5).into_iter()); // most of a second at once
        render(&mut p, 10);
        assert!(p.rx.occupied_len() < 8000, "left {}", p.rx.occupied_len());
    }

    #[test]
    fn reverb_adds_a_tail_that_dies_away_and_stays_bounded() {
        let mut r = Reverb::new(48000);
        let mut energy_early = 0.0;
        let mut energy_late = 0.0;
        let mut peak = 0.0f32;
        let (mut l, mut rr);
        for i in 0..48000 * 6 {
            (l, rr) = r.process(if i == 0 { 1.0 } else { 0.0 });
            peak = peak.max(l.abs()).max(rr.abs());
            if (4800..24000).contains(&i) {
                energy_early += l * l;
            }
            if i >= 48000 * 5 {
                energy_late += l * l;
            }
        }
        assert!(energy_early > 1e-4, "there is a tail");
        assert!(energy_late < energy_early * 0.01, "and it decays: {energy_late} vs {energy_early}");
        assert!(peak < 2.0, "peak {peak}");
    }

    #[test]
    fn reverb_makes_the_two_sides_differ() {
        let mut r = Reverb::new(48000);
        let mut diff = 0.0;
        for i in 0..20000 {
            let (l, rr) = r.process(if i == 0 { 1.0 } else { 0.0 });
            diff += (l - rr).abs();
        }
        assert!(diff > 0.01);
    }

    #[test]
    fn a_loud_voice_with_full_reverb_stays_in_range_for_the_limiter() {
        let (feed, mut p, c) = bus(48000, 48000);
        c.set_reverb(1.0);
        c.set_volume(2.0);
        c.gain_now(&mut p);
        let out = stream(&feed, &mut p, 48000, 48000, &sine(48000, 300.0, 48000.0, 0.9));
        let peak = out.iter().map(|f| f.0.abs().max(f.1.abs())).fold(0.0, f32::max);
        assert!(peak < 4.0, "peak {peak}");
    }

    /// Manual check with a real microphone: `cargo test -p dubplate-engine -- --ignored hears_the_default_microphone --nocapture`
    #[test]
    #[ignore]
    fn hears_the_default_microphone() {
        let c = Arc::new(MicControls::default());
        let (feed, mut p) = mic_bus(Arc::clone(&c));
        c.on.store(true, Ordering::Relaxed);
        let input = feed.open_input(std::env::var("DUBPLATE_TEST_MIC").ok().as_deref()).unwrap();
        println!("{} @ {} Hz", input.device_name, input.sample_rate);
        p.set_output_rate(48000);
        let mut loudest = 0.0f32;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            for _ in 0..960 {
                let f = p.next_frame();
                loudest = loudest.max(f.0.abs());
            }
        }
        println!("loudest {loudest}, buffer {:.1} ms, dropouts {}", c.buffer_ms(), c.dropouts.load(Ordering::Relaxed));
        assert!(c.source_rate.load(Ordering::Relaxed) > 0, "samples arrived");
    }
}
