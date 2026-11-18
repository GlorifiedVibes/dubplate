use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use ringbuf::traits::{Consumer, Observer, Producer};

use crate::deck::DeckPlayer;
use crate::mic::MicProcessor;
use crate::recorder::RecordSink;

/// Crossfader curve ids (shared as an atomic so the UI can switch live).
pub const CF_SMOOTH: u32 = 0;
pub const CF_FAST: u32 = 1;

/// Channel gains `(a, b)` for a crossfader position `cf` (0 = full A, 1 = full B).
///
/// Matches rekordbox / DJM behaviour: at the centre **both channels play at 100 %**;
/// moving towards one side only fades the *other* channel, reaching exactly zero at
/// the end stop. The near channel never rises above unity.
/// * Smooth ("slow"): the far channel fades gradually from the centre.
/// * Fast ("sharp"): both stay at full over most of the travel and the far channel is
///   cut in the last stretch (scratch style).
pub fn crossfade_gains(cf: f32, curve: u32) -> (f32, f32) {
    let cf = cf.clamp(0.0, 1.0);
    let shape = |t: f32| -> f32 {
        // t: 0 = no fade, 1 = fully faded out
        let t = t.clamp(0.0, 1.0);
        let x = if curve == CF_FAST { ((t - 0.85) / 0.15).clamp(0.0, 1.0) } else { t };
        if x >= 0.9999 {
            0.0
        } else {
            (x * std::f32::consts::FRAC_PI_2).cos()
        }
    };
    let fade_a = ((cf - 0.5) * 2.0).max(0.0);
    let fade_b = ((0.5 - cf) * 2.0).max(0.0);
    (shape(fade_a), shape(fade_b))
}

/// Transparent below the knee; rounds peaks above it instead of hard-clipping, so two
/// full-level decks in the crossfader overlap do not distort harshly.
#[inline]
fn soft_limit(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

/// Where the headphone (monitor) mix goes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum MonitorTarget {
    #[default]
    Off,
    /// Channels 3-4 of the main output device (a 4-channel card such as the Hercules Inpulse).
    SameDevice,
    /// A second output device, fed through a small buffer.
    Device(String),
    /// Channels 3-4 of a second 4-channel device (the headphone jack of a controller's sound card),
    /// while the main mix plays somewhere else.
    DeviceChannels34(String),
}

/// Live monitor settings, shared with the UI and the audio callback.
pub struct MonitorControls {
    /// Headphone cue (pre-fader listen) per deck.
    pub cue: [AtomicBool; 2],
    /// Adds the main mix to the headphones.
    pub master: AtomicBool,
    /// Headphone level, f32 bits (1.0 = unity).
    pub level: AtomicU32,
    /// Loudest headphone sample in the last block, f32 bits - for the UI meter.
    pub peak: AtomicU32,
    /// The last attempt to play the prepare player was refused: a deck is playing and there is no headphone
    /// output, so it must not reach the room. Cleared when a start is accepted.
    pub prep_blocked: AtomicBool,
    /// A deck started while the prepare player was playing to the main pair, so it was stopped. The screen
    /// leaves Prepare and clears this.
    pub prep_exited: AtomicBool,
    /// The click track for checking a grid by ear, played with the prepare player.
    pub click: crate::click::ClickControls,
    /// One VST3 effect on the main output (see `vst3_host`).
    #[cfg(feature = "vst3-host")]
    pub insert: crate::vst3_host::MasterInsert,
    /// One VST3 effect on each deck's channel, after its fader and before the crossfader, so an echo or reverb
    /// rings on when the fader closes. The headphone cue is taken before it.
    #[cfg(feature = "vst3-host")]
    pub deck_inserts: [crate::vst3_host::MasterInsert; 2],
}

/// Where the prepare player is being heard, decided when it starts and kept until it stops, so a deck
/// stopping can never move it into the room.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PrepRoute {
    Idle,
    Room,
    Phones,
}

impl Default for MonitorControls {
    fn default() -> Self {
        Self {
            cue: [AtomicBool::new(false), AtomicBool::new(false)],
            master: AtomicBool::new(false),
            level: AtomicU32::new(1.0f32.to_bits()),
            peak: AtomicU32::new(0),
            prep_blocked: AtomicBool::new(false),
            prep_exited: AtomicBool::new(false),
            click: Default::default(),
            #[cfg(feature = "vst3-host")]
            insert: Default::default(),
            #[cfg(feature = "vst3-host")]
            deck_inserts: Default::default(),
        }
    }
}

/// One frame of the headphone mix: the cued decks before their faders, plus the main mix when
/// master monitoring is on, plus the prepare player when it is routed to the headphones.
pub fn monitor_frame(
    pre: [(f32, f32); 2],
    cue: [bool; 2],
    master: (f32, f32),
    master_on: bool,
    prep: (f32, f32),
    level: f32,
) -> (f32, f32) {
    let (mut l, mut r) = (prep.0, prep.1);
    for d in 0..2 {
        if cue[d] {
            l += pre[d].0;
            r += pre[d].1;
        }
    }
    if master_on {
        l += master.0;
        r += master.1;
    }
    (soft_limit(l * level), soft_limit(r * level))
}

pub struct AudioOutput {
    _streams: Vec<cpal::Stream>,
    pub sample_rate: u32,
    pub device_name: String,
    /// What the monitor is actually doing, or why it is off.
    pub monitor_status: String,
    pub monitor_active: bool,
}

/// Returns names of all available output devices on the default host.
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices().map(|devs| devs.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

/// Output devices with the most channels each can open, for pickers that offer channel pairs.
pub fn list_output_devices_with_channels() -> Vec<(String, u16)> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|devs| {
            devs.filter_map(|d| {
                let name = d.name().ok()?;
                let max = d.supported_output_configs().map(|c| c.map(|c| c.channels()).max().unwrap_or(2)).unwrap_or(2);
                Some((name, max))
            })
            .collect()
        })
        .unwrap_or_default()
}

/// Returns the name of the current default output device.
pub fn default_output_device_name() -> Option<String> {
    cpal::default_host().default_output_device()?.name().ok()
}

fn find_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device, String> {
    match name {
        Some(n) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().ok().as_deref() == Some(n))
            .ok_or_else(|| format!("output device '{n}' not found")),
        None => host.default_output_device().ok_or_else(|| "no output device found".to_owned()),
    }
}

/// The mixing half of the audio callback: both decks, the crossfader and the monitor mix.
struct Mixer {
    deck_a: DeckPlayer,
    deck_b: DeckPlayer,
    crossfader: Arc<AtomicU32>,
    crossfader_curve: Arc<AtomicU32>,
    record_sink: RecordSink,
    controls: Arc<MonitorControls>,
    prev_a: f32,
    prev_b: f32,
    /// Monitor frames for a second device.
    monitor_tx: Option<ringbuf::HeapProd<f32>>,
    /// Live microphones added to the main mix (the computer's own, and phones').
    mics: Vec<MicProcessor>,
    /// The prepare player: a third, independent player for checking and fixing a track's grid. It plays at
    /// normal speed. Started with no deck playing it is heard on the main pair, and a deck starting then
    /// stops it; started while a deck plays it is heard only in the headphones (or refused without any).
    prep: Option<DeckPlayer>,
    prep_route: PrepRoute,
    click: crate::click::Click,
    out_rate: f32,
    scratch: Scratch,
}

/// What the first pass over a block keeps for the second (main mix before the effect, headphone parts).
#[derive(Clone, Copy, Default)]
struct FrameData {
    mix: (f32, f32),
    pre: [(f32, f32); 2],
    phones: (f32, f32),
    prep: (f32, f32),
}

#[derive(Default)]
struct Scratch {
    frames: Vec<FrameData>,
    fx: Vec<f32>,
    /// Each deck's block on its own (interleaved stereo), used while that deck has an effect switched on.
    deck: [Vec<f32>; 2],
}

impl Mixer {
    /// Fills `data` (interleaved, `channels` per frame). Channels 3-4 carry the monitor mix when the
    /// stream has them and `monitor_in_stream` is set.
    fn render(&mut self, data: &mut [f32], channels: usize, monitor_in_stream: bool) {
        crate::threads::boost_audio_thread();
        self.deck_a.drain_commands();
        self.deck_b.drain_commands();
        if let Some(p) = self.prep.as_mut() {
            p.drain_commands();
        }

        let cf = f32::from_bits(self.crossfader.load(Ordering::Relaxed));
        let curve = self.crossfader_curve.load(Ordering::Relaxed);
        let (target_a, target_b) = crossfade_gains(cf, curve);
        // Ramp across the block so crossfader moves do not zipper.
        let frames = (data.len() / channels).max(1) as f32;
        let step_a = (target_a - self.prev_a) / frames;
        let step_b = (target_b - self.prev_b) / frames;
        let (mut gain_a, mut gain_b) = (self.prev_a, self.prev_b);
        let deck_start = [self.prev_a, self.prev_b];
        let deck_step = [step_a, step_b];
        self.prev_a = target_a;
        self.prev_b = target_b;

        let monitoring = monitor_in_stream || self.monitor_tx.is_some();
        let cue = [self.controls.cue[0].load(Ordering::Relaxed), self.controls.cue[1].load(Ordering::Relaxed)];
        let master_on = self.controls.master.load(Ordering::Relaxed);
        let level = f32::from_bits(self.controls.level.load(Ordering::Relaxed));

        // Try-lock is non-blocking: if contested (start/stop recording only),
        // the frame is silently skipped — no allocation or blocking.
        let mut rec_guard = self.record_sink.try_lock().ok();
        let mut peak = 0.0f32;
        // Where the prepare player goes is decided when it starts; see `PrepRoute`.
        let decks_playing = self.deck_a.is_playing() || self.deck_b.is_playing();
        if let Some(p) = self.prep.as_mut() {
            if !p.is_playing() {
                self.prep_route = PrepRoute::Idle;
            } else if self.prep_route == PrepRoute::Idle {
                if !decks_playing {
                    self.prep_route = PrepRoute::Room;
                    self.controls.prep_blocked.store(false, Ordering::Relaxed);
                } else if monitoring {
                    self.prep_route = PrepRoute::Phones;
                    self.controls.prep_blocked.store(false, Ordering::Relaxed);
                } else {
                    // A deck is playing and there is no headphone output: never into the room.
                    p.stop_now();
                    self.controls.prep_blocked.store(true, Ordering::Relaxed);
                }
            }
            if self.prep_route == PrepRoute::Room && decks_playing {
                p.stop_now();
                self.prep_route = PrepRoute::Idle;
                self.controls.prep_exited.store(true, Ordering::Relaxed);
            }
        }
        let prep_route = self.prep_route;
        // The click follows the prepare player's playhead and goes wherever that goes.
        let click_on = prep_route != PrepRoute::Idle && self.controls.click.on.load(Ordering::Relaxed);
        let (click_bpm, click_down) = self.controls.click.grid();
        let click_level = self.controls.click.level();
        let click_t0 = match self.prep.as_ref() {
            Some(p) if click_on => {
                p.position_frames() as f64 / self.controls.click.source_rate.load(Ordering::Relaxed).max(1) as f64
            }
            _ => 0.0,
        };
        if !click_on {
            self.click.reset();
        }
        let mut click_i = 0usize;

        // Pass 1: the decks, microphones and prepare player, summed into a scratch block.
        let n_frames = data.len() / channels;
        if self.scratch.frames.len() < n_frames {
            self.scratch.frames.resize(n_frames, FrameData::default());
            self.scratch.fx.resize(n_frames * 2, 0.0);
            self.scratch.deck[0].resize(n_frames * 2, 0.0);
            self.scratch.deck[1].resize(n_frames * 2, 0.0);
        }
        // A deck with its own effect switched on is kept apart until the effect has had the whole block.
        #[cfg(feature = "vst3-host")]
        let apart = [self.controls.deck_inserts[0].enabled(), self.controls.deck_inserts[1].enabled()];
        #[cfg(not(feature = "vst3-host"))]
        let apart = [false; 2];
        for i in 0..n_frames {
            let (al, ar) = self.deck_a.next_frame();
            let (bl, br) = self.deck_b.next_frame();
            gain_a += step_a;
            gain_b += step_b;
            let (mut pl, mut pr) = match self.prep.as_mut() {
                Some(p) if prep_route != PrepRoute::Idle => p.next_frame(),
                _ => (0.0, 0.0),
            };
            if click_on {
                let t = click_t0 + click_i as f64 / self.out_rate as f64;
                click_i += 1;
                let c = self.click.sample(t, click_bpm, click_down, self.out_rate, click_level);
                pl += c;
                pr += c;
            }
            let phones = if prep_route == PrepRoute::Phones { (pl, pr) } else { (0.0, 0.0) };
            let (mut ml, mut mr) = (0.0, 0.0);
            for m in &mut self.mics {
                let (l, r) = m.next_frame();
                ml += l;
                mr += r;
            }
            let (mut xl, mut xr) = (ml, mr);
            for (d, (l, r, g)) in [(al, ar, gain_a), (bl, br, gain_b)].into_iter().enumerate() {
                if apart[d] {
                    self.scratch.deck[d][i * 2] = l;
                    self.scratch.deck[d][i * 2 + 1] = r;
                } else {
                    xl += l * g;
                    xr += r * g;
                }
            }
            self.scratch.frames[i] = FrameData {
                mix: (xl, xr),
                pre: if monitoring { [self.deck_a.pre_fader(), self.deck_b.pre_fader()] } else { [(0.0, 0.0); 2] },
                phones,
                prep: (pl, pr),
            };
        }

        // Each deck's own effect, then that deck joins the mix through the crossfader.
        #[cfg(feature = "vst3-host")]
        for d in 0..2 {
            if !apart[d] {
                continue;
            }
            let block = &mut self.scratch.deck[d][..n_frames * 2];
            self.controls.deck_inserts[d].process(block);
            let mut g = deck_start[d];
            for (i, f) in self.scratch.frames[..n_frames].iter_mut().enumerate() {
                g += deck_step[d];
                f.mix.0 += block[i * 2] * g;
                f.mix.1 += block[i * 2 + 1] * g;
            }
        }
        #[cfg(not(feature = "vst3-host"))]
        let _ = (deck_start, deck_step);

        // The effect on the main output, when there is one and it is on.
        #[cfg(feature = "vst3-host")]
        if self.controls.insert.enabled() {
            let fx = &mut self.scratch.fx[..n_frames * 2];
            for (i, f) in self.scratch.frames[..n_frames].iter().enumerate() {
                fx[i * 2] = f.mix.0;
                fx[i * 2 + 1] = f.mix.1;
            }
            self.controls.insert.process(fx);
            for (i, f) in self.scratch.frames[..n_frames].iter_mut().enumerate() {
                f.mix = (fx[i * 2], fx[i * 2 + 1]);
            }
        }

        // Pass 2: the limiter, the headphone mix, the recording, and the prepare player.
        for (i, frame) in data.chunks_exact_mut(channels).enumerate() {
            let f = self.scratch.frames[i];
            let l = soft_limit(f.mix.0);
            let r = soft_limit(f.mix.1);
            frame.fill(0.0);
            frame[0] = l;
            frame[1] = r;

            if monitoring {
                let (ml, mr) = monitor_frame(f.pre, cue, (l, r), master_on, f.phones, level);
                peak = peak.max(ml.abs()).max(mr.abs());
                if monitor_in_stream && channels >= 4 {
                    frame[2] = ml;
                    frame[3] = mr;
                }
                if let Some(tx) = self.monitor_tx.as_mut() {
                    let _ = tx.try_push(ml);
                    let _ = tx.try_push(mr);
                }
            }

            if let Some(ref mut g) = rec_guard {
                if let Some(ref mut prod) = **g {
                    let _ = prod.try_push(l);
                    let _ = prod.try_push(r);
                }
            }

            // After the recording, so preparing a track never ends up in a recorded set.
            if prep_route == PrepRoute::Room {
                frame[0] = soft_limit(frame[0] + f.prep.0);
                frame[1] = soft_limit(frame[1] + f.prep.1);
            }
        }
        self.controls.peak.store(peak.to_bits(), Ordering::Relaxed);
    }
}

/// Reads the monitor ring at another device's sample rate. The read rate follows the buffer's fill
/// level a little, so the two clocks drifting apart never grows the delay without bound or runs dry.
pub struct MonitorReader {
    rx: ringbuf::HeapCons<f32>,
    /// Source frames per output frame when the clocks agree.
    ratio: f64,
    /// Fill the reader steers towards, in source frames.
    target: f64,
    pos: f64,
    prev: (f32, f32),
    next: (f32, f32),
    primed: bool,
}

impl MonitorReader {
    pub fn new(rx: ringbuf::HeapCons<f32>, source_rate: u32, device_rate: u32) -> Self {
        Self {
            rx,
            ratio: source_rate as f64 / device_rate.max(1) as f64,
            target: source_rate as f64 * 0.04,
            pos: 1.0,
            prev: (0.0, 0.0),
            next: (0.0, 0.0),
            primed: false,
        }
    }

    fn fill(&self) -> f64 {
        (self.rx.occupied_len() / 2) as f64
    }

    fn pull(&mut self) -> bool {
        match (self.rx.try_pop(), self.rx.try_pop()) {
            (Some(l), Some(r)) => {
                self.prev = self.next;
                self.next = (l, r);
                true
            }
            _ => false,
        }
    }

    /// Fills interleaved stereo `data`; underruns play silence until the buffer has refilled.
    /// `offset` is the first channel of the stereo pair to write (0 = front, 2 = channels 3-4).
    pub fn render(&mut self, data: &mut [f32], channels: usize, offset: usize) {
        for frame in data.chunks_exact_mut(channels) {
            frame.fill(0.0);
            if !self.primed {
                if self.fill() < self.target {
                    continue;
                }
                self.primed = true;
            }
            // Nudge the rate by up to 1 % towards the target fill.
            let error = (self.fill() - self.target) / self.target.max(1.0);
            let step = self.ratio * (1.0 + (error * 0.01).clamp(-0.01, 0.01));
            self.pos += step;
            while self.pos >= 1.0 {
                if !self.pull() {
                    self.primed = false;
                    self.pos = 1.0;
                    break;
                }
                self.pos -= 1.0;
            }
            if !self.primed {
                continue;
            }
            let t = self.pos as f32;
            frame[offset] = self.prev.0 + (self.next.0 - self.prev.0) * t;
            frame[offset + 1] = self.prev.1 + (self.next.1 - self.prev.1) * t;
        }
    }
}

/// Starts the audio output. `device_name` selects the main device by name (as returned by
/// `list_output_devices`); `None` uses the system default. `monitor` routes the headphone mix.
pub fn start_audio(
    deck_a: DeckPlayer,
    deck_b: DeckPlayer,
    crossfader: Arc<AtomicU32>,
    crossfader_curve: Arc<AtomicU32>,
    device_name: Option<&str>,
    monitor: &MonitorTarget,
    controls: Arc<MonitorControls>,
    record_sink: RecordSink,
) -> Result<AudioOutput, Box<dyn std::error::Error + Send + Sync>> {
    start_audio_with_mic(
        deck_a,
        deck_b,
        crossfader,
        crossfader_curve,
        device_name,
        monitor,
        controls,
        record_sink,
        Vec::new(),
    )
}

/// `start_audio` with live microphones mixed into the main output.
pub fn start_audio_with_mic(
    deck_a: DeckPlayer,
    deck_b: DeckPlayer,
    crossfader: Arc<AtomicU32>,
    crossfader_curve: Arc<AtomicU32>,
    device_name: Option<&str>,
    monitor: &MonitorTarget,
    controls: Arc<MonitorControls>,
    record_sink: RecordSink,
    mics: Vec<MicProcessor>,
) -> Result<AudioOutput, Box<dyn std::error::Error + Send + Sync>> {
    start_audio_with_prep(
        deck_a,
        deck_b,
        None,
        crossfader,
        crossfader_curve,
        device_name,
        monitor,
        controls,
        record_sink,
        mics,
    )
}

/// `start_audio_with_mic` plus a prepare player (see [`Mixer`]): heard in the headphones, and in the main pair
/// only while no deck is playing.
pub fn start_audio_with_prep(
    mut deck_a: DeckPlayer,
    mut deck_b: DeckPlayer,
    mut prep: Option<DeckPlayer>,
    crossfader: Arc<AtomicU32>,
    crossfader_curve: Arc<AtomicU32>,
    device_name: Option<&str>,
    monitor: &MonitorTarget,
    controls: Arc<MonitorControls>,
    record_sink: RecordSink,
    mut mics: Vec<MicProcessor>,
) -> Result<AudioOutput, Box<dyn std::error::Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = find_device(&host, device_name)?;
    let chosen_name = device.name().unwrap_or_else(|_| "Unknown".into());
    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate().0;

    // Channels 3-4 need a device that can open four.
    let four_ok = device
        .supported_output_configs()
        .map(|mut c| {
            c.any(|c| {
                c.channels() >= 4
                    && c.min_sample_rate() <= supported.sample_rate()
                    && c.max_sample_rate() >= supported.sample_rate()
            })
        })
        .unwrap_or(false);
    let same_device = *monitor == MonitorTarget::SameDevice && four_ok;
    let channels: u16 = if same_device { 4 } else { 2 };
    let mut status = match monitor {
        MonitorTarget::Off => "Monitor off".to_owned(),
        MonitorTarget::SameDevice if four_ok => format!("Monitor on {chosen_name}, channels 3-4"),
        MonitorTarget::SameDevice => format!("{chosen_name} has no channels 3-4, so the monitor is off"),
        MonitorTarget::Device(d) => format!("Monitor on {d}"),
        MonitorTarget::DeviceChannels34(d) => format!("Monitor on {d}, channels 3-4"),
    };
    let mut active = same_device;

    log::info!("audio: {chosen_name} @ {sample_rate}Hz ({channels}ch)");

    deck_a.set_output_sample_rate(sample_rate);
    deck_b.set_output_sample_rate(sample_rate);
    if let Some(p) = prep.as_mut() {
        p.set_output_sample_rate(sample_rate);
    }
    for m in &mut mics {
        m.set_output_rate(sample_rate);
    }

    // A second device is opened first so a failure leaves the main output untouched.
    let mut streams = Vec::new();
    let mut monitor_tx = None;
    let second = match monitor {
        MonitorTarget::Device(n) => Some((n, false)),
        MonitorTarget::DeviceChannels34(n) => Some((n, true)),
        _ => None,
    };
    if let Some((name, pair34)) = second {
        match open_monitor(&host, name, sample_rate, pair34) {
            Ok((stream, tx)) => {
                streams.push(stream);
                monitor_tx = Some(tx);
                active = true;
            }
            Err(e) => {
                log::warn!("monitor: {e}");
                status = format!("Monitor unavailable: {e}");
            }
        }
    }

    let mut mixer = Mixer {
        deck_a,
        deck_b,
        crossfader,
        crossfader_curve,
        record_sink,
        controls,
        prev_a: 1.0,
        prev_b: 1.0,
        monitor_tx,
        mics,
        prep,
        prep_route: PrepRoute::Idle,
        click: Default::default(),
        out_rate: sample_rate as f32,
        scratch: Default::default(),
    };
    // room for a big block up front, so the callback does not allocate
    mixer.scratch.frames.resize(8192, FrameData::default());
    mixer.scratch.fx.resize(8192 * 2, 0.0);
    mixer.scratch.deck[0].resize(8192 * 2, 0.0);
    mixer.scratch.deck[1].resize(8192 * 2, 0.0);
    #[cfg(feature = "vst3-host")]
    {
        mixer.controls.insert.set_rate(sample_rate);
        mixer.controls.deck_inserts[0].set_rate(sample_rate);
        mixer.controls.deck_inserts[1].set_rate(sample_rate);
    }
    let config =
        cpal::StreamConfig { channels, sample_rate: supported.sample_rate(), buffer_size: cpal::BufferSize::Default };
    let n = channels as usize;
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| mixer.render(data, n, same_device),
        |err| log::error!("audio stream error: {err}"),
        None,
    )?;
    stream.play()?;
    streams.push(stream);

    Ok(AudioOutput {
        _streams: streams,
        sample_rate,
        device_name: chosen_name,
        monitor_status: status,
        monitor_active: active,
    })
}

/// Opens `name` as the monitor device; returns its stream and the producer the mixer feeds.
fn open_monitor(
    host: &cpal::Host,
    name: &str,
    main_rate: u32,
    pair34: bool,
) -> Result<(cpal::Stream, ringbuf::HeapProd<f32>), String> {
    use ringbuf::traits::Split;
    let device = find_device(host, Some(name))?;
    let cfg = device.default_output_config().map_err(|e| e.to_string())?;
    let rate = cfg.sample_rate().0;
    let (channels, offset) = if pair34 {
        let ok = device
            .supported_output_configs()
            .map(|mut c| {
                c.any(|c| {
                    c.channels() >= 4
                        && c.min_sample_rate() <= cfg.sample_rate()
                        && c.max_sample_rate() >= cfg.sample_rate()
                })
            })
            .unwrap_or(false);
        if !ok {
            return Err(format!("{name} has no channels 3-4"));
        }
        (4, 2)
    } else {
        (cfg.channels().max(2), 0)
    };
    let (tx, rx) = ringbuf::HeapRb::<f32>::new(main_rate as usize / 2).split(); // 0.25 s of stereo
    let mut reader = MonitorReader::new(rx, main_rate, rate);
    let config =
        cpal::StreamConfig { channels, sample_rate: cfg.sample_rate(), buffer_size: cpal::BufferSize::Default };
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _| reader.render(data, channels as usize, offset),
            |err| log::error!("monitor stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, tx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_has_both_channels_at_full_volume() {
        for curve in [CF_SMOOTH, CF_FAST] {
            assert_eq!(crossfade_gains(0.5, curve), (1.0, 1.0));
        }
    }

    #[test]
    fn hard_ends_silence_the_far_channel_exactly() {
        for curve in [CF_SMOOTH, CF_FAST] {
            assert_eq!(crossfade_gains(0.0, curve), (1.0, 0.0), "hard A");
            assert_eq!(crossfade_gains(1.0, curve), (0.0, 1.0), "hard B");
        }
    }

    #[test]
    fn moving_towards_a_only_fades_b_and_never_boosts_a() {
        for curve in [CF_SMOOTH, CF_FAST] {
            let mut prev_b = 1.0f32;
            for i in 0..=50 {
                let cf = 0.5 - i as f32 / 100.0; // centre -> hard A
                let (a, b) = crossfade_gains(cf, curve);
                assert_eq!(a, 1.0, "A must stay at unity (cf={cf})");
                assert!(b <= prev_b + 1e-6, "B must only fade (cf={cf})");
                assert!((0.0..=1.0).contains(&b));
                prev_b = b;
            }
        }
    }

    #[test]
    fn symmetric_for_deck_b() {
        for curve in [CF_SMOOTH, CF_FAST] {
            for i in 0..=100 {
                let cf = i as f32 / 100.0;
                let (a, b) = crossfade_gains(cf, curve);
                let (a2, b2) = crossfade_gains(1.0 - cf, curve);
                assert!((a - b2).abs() < 1e-6 && (b - a2).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn fast_curve_keeps_a_wide_plateau_then_cuts() {
        // 80 % of the way to A on the fast curve, B is still at full level...
        assert_eq!(crossfade_gains(0.3, CF_FAST).1, 1.0);
        // ...while the smooth curve has already faded it noticeably.
        assert!(crossfade_gains(0.2, CF_SMOOTH).1 < 0.8);
        assert!(crossfade_gains(0.02, CF_FAST).1 < 0.5);
    }

    #[test]
    fn soft_limit_is_transparent_below_the_knee_and_bounded_above() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.79), -0.79);
        assert!(soft_limit(2.0) <= 1.0 && soft_limit(2.0) > 0.9);
        assert!(soft_limit(-5.0) >= -1.0);
        assert!(soft_limit(0.9) > 0.8 && soft_limit(0.9) < 0.9);
    }

    #[test]
    fn monitor_mix_is_the_cued_decks_before_their_faders() {
        let pre = [(0.2, 0.2), (0.3, -0.3)];
        assert_eq!(monitor_frame(pre, [false, false], (0.9, 0.9), false, (0.0, 0.0), 1.0), (0.0, 0.0));
        assert_eq!(monitor_frame(pre, [true, false], (0.9, 0.9), false, (0.0, 0.0), 1.0), (0.2, 0.2));
        let both = monitor_frame(pre, [true, true], (0.0, 0.0), false, (0.0, 0.0), 1.0);
        assert!((both.0 - 0.5).abs() < 1e-6 && (both.1 + 0.1).abs() < 1e-6);
        // Master adds the main mix; level scales the lot.
        assert_eq!(monitor_frame(pre, [false, false], (0.4, 0.4), true, (0.0, 0.0), 0.5), (0.2, 0.2));
    }

    fn ring(frames: &[(f32, f32)]) -> ringbuf::HeapCons<f32> {
        use ringbuf::traits::Split;
        let (mut tx, rx) = ringbuf::HeapRb::<f32>::new(frames.len() * 2 + 8).split();
        for f in frames {
            tx.try_push(f.0).ok();
            tx.try_push(f.1).ok();
        }
        rx
    }

    #[test]
    fn the_monitor_reader_is_silent_until_it_has_filled_and_after_it_runs_dry() {
        let mut r = MonitorReader::new(ring(&[(0.5, 0.5); 10]), 48000, 48000);
        let mut out = vec![1.0f32; 2 * 20];
        r.render(&mut out, 2, 0);
        assert!(out.iter().all(|&x| x == 0.0), "too little buffered to start: stay silent");
    }

    #[test]
    fn the_monitor_reader_passes_a_steady_signal_at_matching_rates() {
        let n = 4000;
        let mut r = MonitorReader::new(ring(&vec![(0.5, -0.5); n]), 48000, 48000);
        let mut out = vec![0.0f32; 2 * 500];
        r.render(&mut out, 2, 0);
        let tail = &out[out.len() - 200..];
        assert!(tail.chunks(2).all(|f| (f[0] - 0.5).abs() < 1e-3 && (f[1] + 0.5).abs() < 1e-3));
    }

    #[test]
    fn the_monitor_reader_converts_between_sample_rates() {
        // 44.1 kHz source into a 48 kHz device consumes 44.1/48 source frames per output frame.
        let n = 6000;
        let mut r = MonitorReader::new(ring(&vec![(0.25, 0.25); n]), 44100, 48000);
        let before = r.fill();
        let mut out = vec![0.0f32; 2 * 1000];
        r.render(&mut out, 2, 0);
        let used = before - r.fill();
        assert!((used - 1000.0 * 44100.0 / 48000.0).abs() < 25.0, "used {used}");
    }

    #[test]
    fn extra_channels_stay_silent() {
        let n = 4000;
        let mut r = MonitorReader::new(ring(&vec![(0.5, 0.5); n]), 48000, 48000);
        let mut out = vec![9.0f32; 6 * 300];
        r.render(&mut out, 6, 0);
        assert!(out.chunks(6).all(|f| f[2..].iter().all(|&x| x == 0.0)));
    }

    /// Manual check against real hardware: `DUBPLATE_TEST_DEVICE="sysdefault:CARD=Mk2" cargo test -p dubplate-engine -- --ignored opens_four_channels`
    #[test]
    #[ignore]
    fn opens_four_channels_on_the_named_device() {
        let Ok(name) = std::env::var("DUBPLATE_TEST_DEVICE") else { return };
        let (_, a) = crate::deck::create_deck_pair(Arc::new(crate::deck::DeckShared::default()));
        let (_, b) = crate::deck::create_deck_pair(Arc::new(crate::deck::DeckShared::default()));
        let out = start_audio(
            a,
            b,
            Arc::new(AtomicU32::new(0.5f32.to_bits())),
            Arc::new(AtomicU32::new(0)),
            Some(&name),
            &MonitorTarget::SameDevice,
            Arc::new(MonitorControls::default()),
            crate::recorder::new_sink(),
        )
        .unwrap();
        println!("{} / {}", out.device_name, out.monitor_status);
        assert!(out.monitor_active, "{}", out.monitor_status);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    /// With `DUBPLATE_TEST_VST3=/path/Effect.vst3`: the effect on the main output changes the mix when it is on, leaves it
    /// alone when it is off, and what is rendered is the same length and finite either way.
    #[cfg(feature = "vst3-host")]
    #[test]
    fn the_main_output_effect_is_heard_only_when_it_is_on() {
        let Some(path) = std::env::var_os("DUBPLATE_TEST_VST3") else { return };
        let controls = Arc::new(MonitorControls::default());
        controls.insert.set_rate(44100);
        let info =
            crate::vst3_host::inspect(std::path::Path::new(&path)).unwrap().into_iter().find(|i| i.is_effect).unwrap();
        controls.insert.load(&info).unwrap();
        // the first effect type of Surge XT Effects is a delay, which is audible on a sine; bypass off
        if let Some(b) = controls.insert.params().iter().find(|p| p.name.eq_ignore_ascii_case("bypass")) {
            controls.insert.set_param(b.id, 0.0);
        }
        let render = |on: bool| {
            controls.insert.set_enabled(on);
            let mut m = playing_mixer(controls.clone());
            let mut out = vec![0.0f32; 2 * 4096];
            m.render(&mut out, 2, false);
            out
        };
        let off = render(false);
        let on = render(true);
        assert!(on.iter().all(|v| v.is_finite()));
        let diff: f32 = on.iter().zip(&off).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 1.0, "the effect changed nothing ({diff})");
        let again = render(false);
        assert_eq!(off, again, "switching it off gives back the plain mix");
    }

    /// With `DUBPLATE_TEST_VST3`: a deck's own effect changes the mix when it is on and leaves it exactly as it was
    /// when it is off, and with the effect on but that deck's side of the crossfader shut, nothing of it is heard.
    #[cfg(feature = "vst3-host")]
    #[test]
    fn a_decks_effect_is_heard_only_when_on_and_sits_before_the_crossfader() {
        let Some(path) = std::env::var_os("DUBPLATE_TEST_VST3") else { return };
        let controls = Arc::new(MonitorControls::default());
        let insert = &controls.deck_inserts[0];
        insert.set_rate(44100);
        let info =
            crate::vst3_host::inspect(std::path::Path::new(&path)).unwrap().into_iter().find(|i| i.is_effect).unwrap();
        insert.load(&info).unwrap();
        if let Some(b) = insert.params().iter().find(|p| p.name.eq_ignore_ascii_case("bypass")) {
            insert.set_param(b.id, 0.0);
        }
        let render = |on: bool, crossfader: f32| {
            insert.set_enabled(on);
            let mut m = playing_mixer(controls.clone());
            m.crossfader.store(crossfader.to_bits(), Ordering::Relaxed);
            let mut out = vec![0.0f32; 2 * 4096];
            m.render(&mut out, 2, false);
            m.render(&mut out, 2, false); // the second block: the crossfader has finished moving
            out
        };
        let off = render(false, 0.5);
        let on = render(true, 0.5);
        assert!(on.iter().all(|v| v.is_finite()));
        let diff: f32 = on.iter().zip(&off).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 1.0, "the effect changed nothing ({diff})");
        assert_eq!(off, render(false, 0.5), "switching it off gives back the plain mix");
        // crossfader hard over to deck B: deck A, effect and all, is gone
        let shut = render(true, 1.0);
        assert!(shut.iter().all(|v| v.abs() < 1e-4), "deck A's effect leaked past the crossfader");
    }

    thread_local! { static GAIN: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) }; }

    fn playing_mixer(controls: Arc<MonitorControls>) -> Mixer {
        use crate::deck::{create_deck_pair, DeckCommand, DeckShared, DecodedTrack};
        let frames = 44100usize;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let v = ((i as f32) * 0.05).sin() * 0.5;
                [v, v]
            })
            .collect();
        let track = Arc::new(DecodedTrack {
            loudness: None,
            samples,
            sample_rate: 44100,
            channels: 2,
            duration_frames: frames as u64,
            waveform: vec![],
            wave: Default::default(),
            title: "t".into(),
            duration_secs: 1.0,
            bpm: 0.0,
            bpm_coarse: 0,
            beat_offset: 0.0,
            beat_times_ms: vec![],
            key: None,
            cues: vec![],
        });
        let (mut ha, mut pa) = create_deck_pair(Arc::new(DeckShared::default()));
        let (_hb, pb) = create_deck_pair(Arc::new(DeckShared::default()));
        pa.set_output_sample_rate(44100);
        ha.send(DeckCommand::Load(track));
        ha.send(DeckCommand::Play);
        GAIN.with(|g| {
            if let Some(v) = g.get() {
                ha.send(DeckCommand::SetGain(v));
            }
        });
        Mixer {
            deck_a: pa,
            deck_b: pb,
            crossfader: Arc::new(AtomicU32::new(0.5f32.to_bits())),
            crossfader_curve: Arc::new(AtomicU32::new(0)),
            record_sink: crate::recorder::new_sink(),
            controls,
            prev_a: 1.0,
            prev_b: 1.0,
            monitor_tx: None,
            mics: vec![],
            prep: None,
            prep_route: PrepRoute::Idle,
            click: Default::default(),
            out_rate: 44100.0,
            scratch: Default::default(),
        }
    }

    fn energy(data: &[f32], channel: usize, channels: usize) -> f32 {
        data.chunks(channels).map(|f| f[channel].abs()).sum()
    }

    #[test]
    fn a_microphone_is_added_to_the_main_pair_and_the_recording() {
        use crate::mic::{mic_bus, MicControls};
        let c = Arc::new(MicControls::default());
        let (feed, mut proc_) = mic_bus(Arc::clone(&c));
        proc_.set_output_rate(44100);
        c.on.store(true, Ordering::Relaxed);
        c.set_reverb(0.0);
        let mut m = playing_mixer(Arc::new(MonitorControls::default()));
        m.crossfader.store(1.0f32.to_bits(), Ordering::Relaxed); // deck A is silent in the main mix
        m.mics = vec![proc_];
        let mut out = vec![0.0f32; 2 * 441];
        let mut heard = (0.0f32, 0.0f32);
        for n in 0..40 {
            feed.push(44100, (0..441).map(|i| ((n * 441 + i) as f32 * 0.1).sin() * 0.5));
            m.render(&mut out, 2, false);
            if n >= 10 {
                heard = (heard.0 + energy(&out, 0, 2), heard.1 + energy(&out, 1, 2));
            }
        }
        assert!(heard.0 > 100.0 && heard.1 > 100.0, "the voice is in both sides: {heard:?}");
    }

    #[test]
    fn cue_sends_the_deck_to_channels_3_and_4_but_not_the_main_pair() {
        let controls = Arc::new(MonitorControls::default());
        let mut m = playing_mixer(Arc::clone(&controls));
        let mut out = vec![0.0f32; 4 * 4096];
        // Nothing cued: the main pair plays, the headphones are silent.
        m.render(&mut out, 4, true);
        assert!(energy(&out, 0, 4) > 1.0 && energy(&out, 2, 4) == 0.0);
        // Cue deck A: it now reaches channels 3-4.
        controls.cue[0].store(true, Ordering::Relaxed);
        m.render(&mut out, 4, true);
        assert!(energy(&out, 2, 4) > 1.0, "cued deck must be audible on the monitor pair");
        assert!(energy(&out, 3, 4) > 1.0);
    }

    #[test]
    fn a_cued_deck_is_heard_even_with_its_fader_down() {
        let controls = Arc::new(MonitorControls::default());
        controls.cue[0].store(true, Ordering::Relaxed);
        let mut m = playing_mixer(Arc::clone(&controls));
        m.crossfader.store(1.0f32.to_bits(), Ordering::Relaxed); // hard B: deck A silent in the main mix
        let mut out = vec![0.0f32; 4 * 4096];
        m.render(&mut out, 4, true);
        m.render(&mut out, 4, true); // let the crossfader ramp finish
        assert!(energy(&out, 0, 4) < 1e-3, "deck A is cut from the main mix");
        assert!(energy(&out, 2, 4) > 1.0, "but still audible when cued");
    }

    /// A prepare player already playing a steady tone, and a mixer around it whose decks are idle.
    fn prep_mixer(controls: Arc<MonitorControls>) -> Mixer {
        use crate::deck::{create_deck_pair, DeckCommand, DeckShared, DecodedTrack};
        let frames = 44100usize;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let v = ((i as f32) * 0.05).sin() * 0.5;
                [v, v]
            })
            .collect();
        let track = Arc::new(DecodedTrack {
            loudness: None,
            samples,
            sample_rate: 44100,
            channels: 2,
            duration_frames: frames as u64,
            waveform: vec![],
            wave: Default::default(),
            title: "p".into(),
            duration_secs: 1.0,
            bpm: 0.0,
            bpm_coarse: 0,
            beat_offset: 0.0,
            beat_times_ms: vec![],
            key: None,
            cues: vec![],
        });
        let (mut hp, mut pp) = create_deck_pair(Arc::new(DeckShared::default()));
        pp.set_output_sample_rate(44100);
        hp.send(DeckCommand::Load(track));
        hp.send(DeckCommand::Play);
        let (_ha, pa) = create_deck_pair(Arc::new(DeckShared::default()));
        let (_hb, pb) = create_deck_pair(Arc::new(DeckShared::default()));
        Mixer {
            deck_a: pa,
            deck_b: pb,
            crossfader: Arc::new(AtomicU32::new(0.5f32.to_bits())),
            crossfader_curve: Arc::new(AtomicU32::new(0)),
            record_sink: crate::recorder::new_sink(),
            controls,
            prev_a: 1.0,
            prev_b: 1.0,
            monitor_tx: None,
            mics: vec![],
            prep: Some(pp),
            prep_route: PrepRoute::Idle,
            click: Default::default(),
            out_rate: 44100.0,
            scratch: Default::default(),
        }
    }

    /// A deck already playing a tone, to put into a mixer.
    fn playing_deck() -> DeckPlayer {
        use crate::deck::{create_deck_pair, DeckCommand, DeckShared, DecodedTrack};
        let frames = 44100usize;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let v = ((i as f32) * 0.03).sin() * 0.5;
                [v, v]
            })
            .collect();
        let track = Arc::new(DecodedTrack {
            loudness: None,
            samples,
            sample_rate: 44100,
            channels: 2,
            duration_frames: frames as u64,
            waveform: vec![],
            wave: Default::default(),
            title: "d".into(),
            duration_secs: 1.0,
            bpm: 0.0,
            bpm_coarse: 0,
            beat_offset: 0.0,
            beat_times_ms: vec![],
            key: None,
            cues: vec![],
        });
        let (mut ha, mut pa) = create_deck_pair(Arc::new(DeckShared::default()));
        pa.set_output_sample_rate(44100);
        ha.send(DeckCommand::Load(track));
        ha.send(DeckCommand::Play);
        pa.drain_commands();
        pa
    }

    #[test]
    fn with_nothing_playing_the_prepare_player_uses_the_main_pair() {
        let mut m = prep_mixer(Arc::new(MonitorControls::default()));
        let mut out = vec![0.0f32; 4 * 4096];
        m.render(&mut out, 4, true); // even with a headphone pair available
        assert!(energy(&out, 0, 4) > 1.0 && energy(&out, 1, 4) > 1.0, "the main pair carries it");
        assert_eq!(energy(&out, 2, 4), 0.0);
        assert!(!m.controls.prep_blocked.load(Ordering::Relaxed));
    }

    #[test]
    fn a_deck_starting_stops_the_prepare_player() {
        let mut m = prep_mixer(Arc::new(MonitorControls::default()));
        let mut out = vec![0.0f32; 2 * 4096];
        m.render(&mut out, 2, false);
        assert!(energy(&out, 0, 2) > 1.0);
        m.deck_a = playing_deck();
        m.crossfader.store(1.0f32.to_bits(), Ordering::Relaxed); // deck A is cut from the main mix
        m.render(&mut out, 2, false); // stopped here
        m.render(&mut out, 2, false);
        m.render(&mut out, 2, false);
        assert!(m.controls.prep_exited.load(Ordering::Relaxed), "the screen is told to leave Prepare");
        assert!(!m.prep.as_ref().unwrap().is_playing());
        assert!(energy(&out, 0, 2) < 1e-3, "and it is quiet in the room");
    }

    #[test]
    fn started_while_a_deck_plays_it_is_heard_only_in_the_headphones_and_stays_there() {
        let mut m = prep_mixer(Arc::new(MonitorControls::default()));
        m.deck_a = playing_deck();
        m.crossfader.store(1.0f32.to_bits(), Ordering::Relaxed);
        let mut out = vec![0.0f32; 4 * 4096];
        m.render(&mut out, 4, true);
        m.render(&mut out, 4, true);
        assert!(energy(&out, 2, 4) > 1.0 && energy(&out, 3, 4) > 1.0, "heard on channels 3-4");
        assert!(energy(&out, 0, 4) < 1e-3, "not in the room");
        // The deck stops: the prepare player must not move into the room.
        m.deck_a.stop_now();
        m.render(&mut out, 4, true);
        assert!(energy(&out, 2, 4) > 1.0 && energy(&out, 0, 4) < 1e-3, "still only in the headphones");
        assert!(!m.controls.prep_exited.load(Ordering::Relaxed));
    }

    #[test]
    fn started_while_a_deck_plays_with_no_headphones_it_is_refused() {
        let mut m = prep_mixer(Arc::new(MonitorControls::default()));
        m.deck_a = playing_deck();
        m.crossfader.store(1.0f32.to_bits(), Ordering::Relaxed);
        let mut out = vec![0.0f32; 2 * 4096];
        m.render(&mut out, 2, false);
        m.render(&mut out, 2, false);
        assert!(m.controls.prep_blocked.load(Ordering::Relaxed), "the screen says why");
        assert!(!m.prep.as_ref().unwrap().is_playing(), "and it does not keep running silently");
        assert!(energy(&out, 0, 2) < 1e-3);
    }

    #[test]
    fn the_click_plays_with_the_prepare_player_on_the_beat() {
        let controls = Arc::new(MonitorControls::default());
        controls.click.on.store(true, Ordering::Relaxed);
        controls.click.set_grid(120.0, 0.0);
        let mut m = prep_mixer(Arc::clone(&controls));
        // Make the prepare player silent so only the click is heard: an empty-track player is enough.
        m.prep = Some({
            use crate::deck::{create_deck_pair, DeckCommand, DeckShared, DecodedTrack};
            let frames = 44100usize * 4;
            let track = Arc::new(DecodedTrack {
                loudness: None,
                samples: vec![0.0; frames * 2],
                sample_rate: 44100,
                channels: 2,
                duration_frames: frames as u64,
                waveform: vec![],
                wave: Default::default(),
                title: "s".into(),
                duration_secs: 4.0,
                bpm: 0.0,
                bpm_coarse: 0,
                beat_offset: 0.0,
                beat_times_ms: vec![],
                key: None,
                cues: vec![],
            });
            let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
            p.set_output_sample_rate(44100);
            h.send(DeckCommand::Load(track));
            h.send(DeckCommand::Play);
            p
        });
        let mut out = vec![0.0f32; 2 * 44100]; // one second: two beats at 120 BPM
        m.render(&mut out, 2, false);
        let ticks = out.chunks(2).filter(|f| f[0].abs() > 0.02).count();
        assert!(ticks > 200, "the click is audible in the main pair ({ticks} loud frames)");
        // With the click off it is silent.
        controls.click.on.store(false, Ordering::Relaxed);
        let mut quiet = vec![0.0f32; 2 * 4410];
        m.render(&mut quiet, 2, false);
        assert!(energy(&quiet, 0, 2) < 1e-6);
    }

    /// Manual check with a real file: `DUBPLATE_TEST_TRACK=/path/song.mp3 cargo test -p dubplate-engine -- --ignored real_track_cue`
    #[test]
    #[ignore]
    fn real_track_cue_reaches_the_monitor() {
        use crate::deck::{create_deck_pair, DeckCommand, DeckShared};
        let Ok(path) = std::env::var("DUBPLATE_TEST_TRACK") else { return };
        let track = Arc::new(crate::decode::decode_track(std::path::Path::new(&path)).unwrap());
        let controls = Arc::new(MonitorControls::default());
        controls.cue[0].store(true, Ordering::Relaxed);
        let (mut ha, mut pa) = create_deck_pair(Arc::new(DeckShared::default()));
        let (_hb, pb) = create_deck_pair(Arc::new(DeckShared::default()));
        pa.set_output_sample_rate(48000);
        ha.load_track(Arc::clone(&track), path.into());
        ha.send(DeckCommand::Play);
        let mut m = Mixer {
            deck_a: pa,
            deck_b: pb,
            crossfader: Arc::new(AtomicU32::new(0.5f32.to_bits())),
            crossfader_curve: Arc::new(AtomicU32::new(0)),
            record_sink: crate::recorder::new_sink(),
            controls,
            prev_a: 1.0,
            prev_b: 1.0,
            monitor_tx: None,
            mics: vec![],
            prep: None,
            prep_route: PrepRoute::Idle,
            click: Default::default(),
            out_rate: 44100.0,
            scratch: Default::default(),
        };
        let mut out = vec![0.0f32; 4 * 48000];
        for _ in 0..3 {
            m.render(&mut out, 4, true);
        }
        println!("main {} monitor {}", energy(&out, 0, 4), energy(&out, 2, 4));
        assert!(energy(&out, 2, 4) > 1.0);
    }

    #[test]
    fn a_cued_deck_is_heard_with_its_channel_fader_at_zero() {
        GAIN.with(|g| g.set(Some(0.0)));
        let controls = Arc::new(MonitorControls::default());
        controls.cue[0].store(true, Ordering::Relaxed);
        let mut m = playing_mixer(Arc::clone(&controls));
        let mut out = vec![0.0f32; 4 * 4096];
        m.render(&mut out, 4, true);
        m.render(&mut out, 4, true);
        GAIN.with(|g| g.set(None));
        assert!(energy(&out, 0, 4) < 1e-3, "channel fader at zero silences the main mix");
        assert!(energy(&out, 2, 4) > 1.0, "but cue listens before the fader");
    }

    #[test]
    fn cue_alone_survives_the_trip_through_the_second_device_buffer() {
        use ringbuf::traits::Split;
        let controls = Arc::new(MonitorControls::default());
        controls.cue[0].store(true, Ordering::Relaxed);
        let mut m = playing_mixer(Arc::clone(&controls));
        let (tx, rx) = ringbuf::HeapRb::<f32>::new(44100 / 2).split();
        m.monitor_tx = Some(tx);
        let mut reader = MonitorReader::new(rx, 44100, 48000);
        let mut out = vec![0.0f32; 2 * 512];
        let mut heard = 0.0f32;
        for _ in 0..40 {
            m.render(&mut out, 2, false); // the main callback fills the ring
            let mut hp = vec![0.0f32; 2 * 556]; // the headphone device drains it
            reader.render(&mut hp, 2, 0);
            heard += energy(&hp, 0, 2);
        }
        assert!(heard > 10.0, "cue alone should be audible on a second device, heard {heard}");
    }

    #[test]
    fn the_monitor_reader_can_write_channels_3_and_4() {
        let mut r = MonitorReader::new(ring(&vec![(0.5, -0.5); 4000]), 48000, 48000);
        let mut out = vec![9.0f32; 4 * 400];
        r.render(&mut out, 4, 2);
        let tail = &out[out.len() - 80..];
        assert!(tail
            .chunks(4)
            .all(|f| f[0] == 0.0 && f[1] == 0.0 && (f[2] - 0.5).abs() < 1e-3 && (f[3] + 0.5).abs() < 1e-3));
    }

    /// Manual check: `DUBPLATE_TEST_DEVICE="sysdefault:CARD=Mk2" cargo test -p dubplate-engine -- --ignored second_device_channels`
    #[test]
    #[ignore]
    fn second_device_channels_3_and_4_open_beside_the_default_output() {
        let Ok(name) = std::env::var("DUBPLATE_TEST_DEVICE") else { return };
        let (_, a) = crate::deck::create_deck_pair(Arc::new(crate::deck::DeckShared::default()));
        let (_, b) = crate::deck::create_deck_pair(Arc::new(crate::deck::DeckShared::default()));
        let out = start_audio(
            a,
            b,
            Arc::new(AtomicU32::new(0.5f32.to_bits())),
            Arc::new(AtomicU32::new(0)),
            None,
            &MonitorTarget::DeviceChannels34(name),
            Arc::new(MonitorControls::default()),
            crate::recorder::new_sink(),
        )
        .unwrap();
        println!("{} / {}", out.device_name, out.monitor_status);
        assert!(out.monitor_active, "{}", out.monitor_status);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
