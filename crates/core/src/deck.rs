use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::eq::{DeckEffect, DeckEq};
use crate::keylock::KeyLock;
use crate::stems::TrackStems;
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapRb,
};

pub const WAVEFORM_CHUNK_FRAMES: usize = 256;

/// A cue/loop/hot-cue point — mirrors the rekordbox djmdCue / PCOB schema
/// so that data maps 1-to-1 for ANLZ export and SQLite storage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Cue {
    /// Cue position in milliseconds from track start.
    pub time_ms: u32,
    /// Loop end position; `None` for a plain point cue.
    pub out_time_ms: Option<u32>,
    /// Exact cue position in source sample frames (grid-snapped cues are fractional).
    pub pos_frames: f64,
    /// Exact loop end in source sample frames; `None` for a point cue.
    pub out_frames: Option<f64>,
    /// Hot-cue pad number (1–8).  `None` = memory cue (not on a pad).
    pub hot_cue: Option<u8>,
    pub label: String,
    /// RGB colour shown on CDJ pad / waveform marker.
    pub color: [u8; 3],
}

impl Cue {
    pub fn new(
        hot_cue: Option<u8>,
        pos_frames: f64,
        out_frames: Option<f64>,
        sample_rate: u32,
        color: [u8; 3],
    ) -> Self {
        let ms = |f: f64| (f / sample_rate.max(1) as f64 * 1000.0).round().max(0.0) as u32;
        Cue {
            time_ms: ms(pos_frames),
            out_time_ms: out_frames.map(ms),
            pos_frames,
            out_frames,
            hot_cue,
            label: String::new(),
            color,
        }
    }

    pub fn is_loop(&self) -> bool {
        self.out_frames.is_some()
    }
}

pub struct DecodedTrack {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_frames: u64,
    /// Per-chunk spectral bands: [low, mid, high], each normalised 0.0–1.0.
    pub waveform: Vec<[f32; 3]>,
    /// What the waveform display draws from (empty when there is no audio).
    pub wave: std::sync::Arc<crate::waveform_data::WaveSummary>,
    pub title: String,
    pub duration_secs: f64,
    /// Estimated BPM; 0.0 means detection failed.
    pub bpm: f32,
    /// BPM × 100 — ANLZ PQTZ tag format (e.g. 12800 = 128.00 BPM).
    pub bpm_coarse: u32,
    /// Position of beat 1 in waveform chunks (fractional).
    /// Used to align the beat grid; 0.0 if BPM detection failed.
    pub beat_offset: f64,
    /// Absolute millisecond position of every detected beat.
    /// Beat 0 = first beat at or after t=0.  Maps directly to PQTZ entries.
    pub beat_times_ms: Vec<u32>,
    /// Detected musical key (`None` if the track has no tonal content).
    pub key: Option<crate::meta::Key>,
    /// Hot cues, memory cues, and loops set by the user.
    /// Populated from the library DB after the track is loaded.
    pub cues: Vec<Cue>,
    /// Measured loudness (`None` for silence); what auto gain works from.
    pub loudness: Option<crate::loudness::Loudness>,
}

/// Beat grid: tempo plus the position (waveform chunks) of a downbeat.
/// Beat `n` sits at `offset + n * period`; `n % 4 == 0` starts a bar and
/// `n % 16 == 0` a phrase. Owned by the UI side so it can be edited without
/// touching the audio data shared with the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: f32,
    pub offset: f64,
}

impl BeatGrid {
    pub fn is_valid(&self) -> bool {
        self.bpm > 0.0
    }

    pub fn period_chunks(&self, sample_rate: u32) -> f64 {
        sample_rate as f64 / WAVEFORM_CHUNK_FRAMES as f64 * 60.0 / self.bpm as f64
    }

    /// Fractional beat number at the given sample-frame position.
    pub fn beat_at_frame(&self, sample_rate: u32, frame: u64) -> f64 {
        if !self.is_valid() {
            return 0.0;
        }
        let chunk = frame as f64 / WAVEFORM_CHUNK_FRAMES as f64;
        (chunk - self.offset) / self.period_chunks(sample_rate)
    }

    /// Beat times (ms) starting on a downbeat — for ANLZ export.
    pub fn beat_times_ms(&self, sample_rate: u32, duration_frames: u64) -> Vec<u32> {
        beat_times_ms(self.bpm, self.offset, sample_rate, duration_frames)
    }
}

/// Absolute millisecond position of every beat, starting on a downbeat so that
/// `index % 4 == 0` is always beat 1 of a bar.
pub fn beat_times_ms(bpm: f32, beat_offset_chunks: f64, sample_rate: u32, duration_frames: u64) -> Vec<u32> {
    if bpm <= 0.0 || sample_rate == 0 {
        return Vec::new();
    }
    let chunk_rate = sample_rate as f64 / WAVEFORM_CHUNK_FRAMES as f64;
    let beat_period = chunk_rate * 60.0 / bpm as f64;
    let total_chunks = duration_frames as f64 / WAVEFORM_CHUNK_FRAMES as f64;

    let mut chunk = beat_offset_chunks.rem_euclid(beat_period * 4.0);
    let mut times = Vec::with_capacity((total_chunks / beat_period) as usize + 1);
    while chunk < total_chunks {
        let ms = (chunk * WAVEFORM_CHUNK_FRAMES as f64 / sample_rate as f64 * 1000.0) as u32;
        times.push(ms);
        chunk += beat_period;
    }
    times
}

pub enum DeckCommand {
    Load(Arc<DecodedTrack>),
    Play,
    /// Start playing after exactly this many output frames (quantised/phrase-synced start).
    PlayAfter(u64),
    Pause,
    Seek(f64),
    SetGain(f32),
    SetPregain(f32),
    /// Track gain from auto gain, as a linear factor (1.0 = untouched); changes glide so playing tracks do not click.
    SetTrim(f32),
    /// Key lock (Master Tempo) on or off: with it on, a tempo other than the track's own does not change the pitch.
    SetKeyLock(bool),
    /// Key shift in semitones (needs key lock on to be heard).
    SetKeyShift(f32),
    SetEffectMode(u8),
    /// The channel effect, already built ([`DeckHandle::send`] turns `SetEffectMode` into this, so the delay lines are
    /// not allocated on the audio thread).
    SetEffect(Box<DeckEffect>),
    SetTempo(f32),
    JogBend(f32),
    SetEqHi(f32),
    SetEqMid(f32),
    SetEqLo(f32),
    SetFilter(f32),
    SetBrake(bool), // alias of JogTouch (MIDI jog touch sensor)
    /// Platter top touched (true) / released (false): hold + scratch, ramped.
    JogTouch(bool),
    /// Scratch velocity while touched; 1.0 = normal forward speed, negative = reverse.
    JogScratch(f32),
    /// Absolute pitch-nudge target (relative tempo offset, e.g. +0.1) while edge-dragging; 0 releases.
    JogNudge(f32),
    /// Sets (and activates) a loop between two source-frame positions, or clears it.
    SetLoop(Option<(f64, f64)>),
    /// Instant jump to a source-frame position, keeping speed (short crossfade).
    Jump(f64),
    /// Stop and park the playhead at a source-frame position once the stop ramp finishes.
    StopAt(f64),
    LoadStems(Arc<TrackStems>), // activate stem playback
    ClearStems,                 // revert to original samples
    SetStemGain(usize, f32),    // stem 0-3, gain 0.0 (mute) – 1.0 (full)
}

/// Things the audio thread is done with and must not free itself: freeing a track (hundreds of megabytes) or an
/// effect's delay lines can take long enough to be heard on the other deck. They go back to the UI side
/// ([`DeckHandle::empty_trash`]) and are dropped there.
pub enum Trash {
    Track(Arc<DecodedTrack>),
    Stems(Arc<TrackStems>),
    Effect(Box<DeckEffect>),
}

pub struct DeckShared {
    pub position_frames: AtomicU64,
    pub duration_frames: AtomicU64,
    pub is_playing: AtomicBool,
    pub is_loaded: AtomicBool,
    pub stems_loaded: AtomicBool,
    /// True while a `PlayAfter` start is pending.
    pub armed: AtomicBool, // true once LoadStems has been processed
    gain_bits: AtomicU32,
    pregain_bits: AtomicU32,
    pub tempo_bits: AtomicU32,
    pub eq_hi_bits: AtomicU32,
    pub eq_mid_bits: AtomicU32,
    pub eq_lo_bits: AtomicU32,
    pub filter_bits: AtomicU32,
    pub effect_mode: AtomicU32,
    /// The output's sample rate, once the player knows it.
    pub output_rate: AtomicU32,
}

impl Default for DeckShared {
    fn default() -> Self {
        let f1 = 1.0f32.to_bits();
        Self {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            is_playing: AtomicBool::new(false),
            is_loaded: AtomicBool::new(false),
            stems_loaded: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            gain_bits: AtomicU32::new(f1),
            pregain_bits: AtomicU32::new(f1),
            tempo_bits: AtomicU32::new(f1),
            eq_hi_bits: AtomicU32::new(f1),
            eq_mid_bits: AtomicU32::new(f1),
            eq_lo_bits: AtomicU32::new(f1),
            filter_bits: AtomicU32::new(0.5f32.to_bits()),
            effect_mode: AtomicU32::new(0),
            output_rate: AtomicU32::new(44100),
        }
    }
}

impl DeckShared {
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    pub fn set_gain(&self, g: f32) {
        self.gain_bits.store(g.to_bits(), Ordering::Relaxed);
    }

    pub fn pregain(&self) -> f32 {
        f32::from_bits(self.pregain_bits.load(Ordering::Relaxed))
    }

    pub fn tempo_ratio(&self) -> f32 {
        f32::from_bits(self.tempo_bits.load(Ordering::Relaxed))
    }

    pub fn tempo_percent(&self) -> f32 {
        (self.tempo_ratio() - 1.0) * 100.0
    }

    pub fn eq_hi(&self) -> f32 {
        f32::from_bits(self.eq_hi_bits.load(Ordering::Relaxed))
    }
    pub fn eq_mid(&self) -> f32 {
        f32::from_bits(self.eq_mid_bits.load(Ordering::Relaxed))
    }
    pub fn eq_lo(&self) -> f32 {
        f32::from_bits(self.eq_lo_bits.load(Ordering::Relaxed))
    }
    pub fn filter_val(&self) -> f32 {
        f32::from_bits(self.filter_bits.load(Ordering::Relaxed))
    }
    pub fn effect_mode_id(&self) -> u8 {
        self.effect_mode.load(Ordering::Relaxed) as u8
    }

    pub fn progress(&self) -> f64 {
        let pos = self.position_frames.load(Ordering::Relaxed) as f64;
        let dur = self.duration_frames.load(Ordering::Relaxed) as f64;
        if dur == 0.0 {
            0.0
        } else {
            (pos / dur).clamp(0.0, 1.0)
        }
    }

    pub fn position_secs(&self, sample_rate: u32) -> f64 {
        self.position_frames.load(Ordering::Relaxed) as f64 / sample_rate as f64
    }
}

// ---------------------------------------------------------------------------
// Jog / transport motion maths (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Slew-limited step: move `cur` toward `target` by at most `max_step`.
#[inline]
pub(crate) fn slew(cur: f32, target: f32, max_step: f32) -> f32 {
    let d = target - cur;
    if d.abs() <= max_step {
        target
    } else {
        cur + max_step * d.signum()
    }
}

/// Linear-interpolated stereo frame at fractional `pos`; `None` when `pos` is outside the buffer.
#[inline]
pub(crate) fn interp_frame(samples: &[f32], ch: usize, pos: f64) -> Option<(f32, f32)> {
    if pos < 0.0 {
        return None;
    }
    let fi = pos as usize;
    let t = (pos - fi as f64) as f32;
    let i0 = fi * ch;
    if i0 + ch > samples.len() {
        return None;
    }
    let s0l = samples[i0];
    let s0r = if ch > 1 { samples[i0 + 1] } else { s0l };
    let i1 = i0 + ch;
    let (s1l, s1r) = if i1 + ch <= samples.len() {
        (samples[i1], if ch > 1 { samples[i1 + 1] } else { samples[i1] })
    } else {
        (s0l, s0r)
    };
    Some((s0l + (s1l - s0l) * t, s0r + (s1r - s0r) * t))
}

// Speed is in "normal play = 1.0" units; rates are speed units per second.
const SCRATCH_RATE: f32 = 1.0 / 0.006; // following the hand while touched
const TOUCH_SETTLE_S: f32 = 0.030; // vinyl-stop ramp on touch-down
const TOUCH_STOP_RATE: f32 = 1.0 / 0.030;
const START_RATE: f32 = 1.0 / 0.060; // play / pause ramps
const RESUME_RATE: f32 = 1.0 / 0.120; // ramp back up after releasing the platter
const RESUME_WINDOW_S: f32 = 0.25;
const NUDGE_ATTACK: f32 = 1.5; // relative tempo per second
const NUDGE_RELEASE: f32 = 0.6;
const NUDGE_MAX: f32 = 0.6;
/// Below this |speed| output fades to silence (frozen playhead would otherwise be DC / click).
const MUTE_SPEED: f32 = 0.05;
/// Crossfade length for loop wraps and jumps (about 3 ms) — hides the splice click.
const XFADE_FRAMES: u32 = 128;
/// Key lock only runs between these playback speeds, and not within `KL_DEAD_BAND` of 1.0 (nothing to correct there).
const KL_MIN_TEMPO: f32 = 0.25;
const KL_MAX_TEMPO: f32 = 2.0;
const KL_DEAD_BAND: f32 = 0.002;
/// How fast the stretched sound is mixed in or out, per frame (about 10 ms).
const KL_MIX_STEP: f32 = 1.0 / 441.0;
/// How fast the track gain follows a change, per frame (about 50 ms to settle at 44.1 kHz).
const TRIM_GLIDE: f32 = 0.0005;
/// Shortest loop the engine will run (frames); shorter regions are ignored.
const MIN_LOOP_FRAMES: f64 = 64.0;

// ---------------------------------------------------------------------------
// Audio-thread side — lives inside the cpal callback closure, never shared.
// ---------------------------------------------------------------------------

pub struct DeckPlayer {
    shared: Arc<DeckShared>,
    cmd_rx: ringbuf::HeapCons<DeckCommand>,
    trash: Option<ringbuf::HeapProd<Trash>>,
    track: Option<Arc<DecodedTrack>>,
    stems: Option<Arc<TrackStems>>,
    stem_gains: [f32; 4], // per-stem gain: 1.0 = full, 0.0 = muted
    frac_pos: f64,
    is_playing: bool,
    gain: f32,
    pregain: f32,
    trim_target: f32,
    trim_cur: f32,
    keylock_on: bool,
    /// The stretcher (made when the output rate is known), whether it is running, how far its sound is mixed in
    /// (0 = the plain playhead, 1 = all stretched) and frames left to hold at 0 after a jump.
    kl: Option<Box<KeyLock>>,
    kl_active: bool,
    key_shift: f32,
    kl_mix: f32,
    kl_hold: u32,
    tempo_ratio: f32,
    bend: f32, // decaying impulse (MIDI jog)
    bend_decay_touch: f32,
    bend_decay_free: f32,
    speed_cur: f32, // smoothed playback speed, 1.0 = normal
    touch: bool,
    scratch_vel: f32,
    nudge_target: f32,
    nudge_cur: f32,
    settle: u32,
    resume: u32,
    bound: i8, // -1/+1: playhead pinned at start/end (outward motion is ramped to 0)
    update_counter: u32,
    pending_play: u64, // frames left before a PlayAfter start (0 = none)
    loop_region: Option<(f64, f64)>,
    xfade_pos: f64, // continuing read position of the path we are fading away from
    xfade_left: u32,
    stop_at: Option<f64>,
    output_sample_rate: u32,
    eq: DeckEq,
    effect: DeckEffect,
    /// The last frame before the channel fader, for headphone cue (pre-fader listen).
    pre_fader: (f32, f32),
}

impl DeckPlayer {
    pub fn new(shared: Arc<DeckShared>, cmd_rx: ringbuf::HeapCons<DeckCommand>) -> Self {
        Self {
            shared,
            cmd_rx,
            trash: None,
            track: None,
            stems: None,
            stem_gains: [1.0; 4],
            frac_pos: 0.0,
            is_playing: false,
            gain: 1.0,
            pregain: 1.0,
            trim_target: 1.0,
            trim_cur: 1.0,
            keylock_on: false,
            kl: None,
            kl_active: false,
            key_shift: 0.0,
            kl_mix: 0.0,
            kl_hold: 0,
            tempo_ratio: 1.0,
            bend: 0.0,
            bend_decay_touch: (-1.0 / (0.060 * 44100.0_f32)).exp(),
            bend_decay_free: (-1.0 / (0.150 * 44100.0_f32)).exp(),
            speed_cur: 0.0,
            touch: false,
            scratch_vel: 0.0,
            nudge_target: 0.0,
            nudge_cur: 0.0,
            settle: 0,
            resume: 0,
            bound: 0,
            update_counter: 0,
            pre_fader: (0.0, 0.0),
            pending_play: 0,
            loop_region: None,
            xfade_pos: 0.0,
            xfade_left: 0,
            stop_at: None,
            output_sample_rate: 44100,
            eq: DeckEq::new(44100),
            effect: DeckEffect::new(0, 44100),
        }
    }

    /// Hands something to the UI side to be freed there. If the bin is full (nobody is emptying it) it is freed here
    /// after all: late is better than leaked.
    fn bin(&mut self, t: Trash) {
        if let Some(tx) = self.trash.as_mut() {
            let _ = tx.try_push(t);
        }
    }

    pub fn set_output_sample_rate(&mut self, sr: u32) {
        self.output_sample_rate = sr;
        self.shared.output_rate.store(sr, Ordering::Relaxed);
        let mut kl = KeyLock::new(sr);
        kl.set_shift(self.key_shift);
        self.kl = Some(Box::new(kl));
        self.kl_active = false;
        self.kl_mix = 0.0;
        self.bend_decay_touch = (-1.0 / (0.060 * sr as f32)).exp();
        self.bend_decay_free = (-1.0 / (0.150 * sr as f32)).exp();
        self.eq.set_sample_rate(sr);
        self.effect.set_sample_rate(sr);
    }

    pub fn drain_commands(&mut self) {
        while let Some(cmd) = self.cmd_rx.try_pop() {
            match cmd {
                DeckCommand::Load(track) => {
                    self.shared.duration_frames.store(track.duration_frames, Ordering::Relaxed);
                    self.shared.position_frames.store(0, Ordering::Relaxed);
                    self.shared.is_playing.store(false, Ordering::Relaxed);
                    self.shared.is_loaded.store(true, Ordering::Relaxed);
                    self.shared.stems_loaded.store(false, Ordering::Relaxed);
                    self.pending_play = 0;
                    self.loop_region = None;
                    self.xfade_left = 0;
                    self.stop_at = None;
                    self.shared.armed.store(false, Ordering::Relaxed);
                    self.frac_pos = 0.0;
                    self.is_playing = false;
                    self.speed_cur = 0.0;
                    self.touch = false;
                    self.scratch_vel = 0.0;
                    self.nudge_target = 0.0;
                    self.nudge_cur = 0.0;
                    self.bend = 0.0;
                    if let Some(old) = self.stems.take() {
                        self.bin(Trash::Stems(old));
                    }
                    self.stem_gains = [1.0; 4];
                    if let Some(old) = self.track.replace(track) {
                        self.bin(Trash::Track(old));
                    }
                }
                // An empty deck can't play; marking it playing would also block loading a track onto it.
                DeckCommand::Play | DeckCommand::PlayAfter(_) if self.track.is_none() => {}
                DeckCommand::Play => {
                    self.stop_at = None;
                    self.pending_play = 0;
                    self.shared.armed.store(false, Ordering::Relaxed);
                    self.is_playing = true;
                    self.shared.is_playing.store(true, Ordering::Relaxed);
                }
                DeckCommand::PlayAfter(frames) => {
                    if frames == 0 {
                        self.is_playing = true;
                        self.shared.is_playing.store(true, Ordering::Relaxed);
                    } else {
                        self.pending_play = frames;
                        self.shared.armed.store(true, Ordering::Relaxed);
                    }
                }
                DeckCommand::Pause => {
                    self.pending_play = 0;
                    self.shared.armed.store(false, Ordering::Relaxed);
                    self.is_playing = false;
                    self.shared.is_playing.store(false, Ordering::Relaxed);
                }
                DeckCommand::Seek(t) => {
                    self.stop_at = None;
                    if let Some(track) = &self.track {
                        self.frac_pos =
                            (t * track.duration_frames as f64).clamp(0.0, track.duration_frames as f64 - 1.0);
                        self.kl_active = false;
                        self.kl_mix = 0.0;
                        self.shared.position_frames.store(self.frac_pos as u64, Ordering::Relaxed);
                        // Jump = restart from silence (fade-in) rather than a click.
                        self.speed_cur = 0.0;
                    }
                }
                DeckCommand::SetGain(g) => {
                    self.gain = g;
                    self.shared.set_gain(g);
                }
                DeckCommand::SetKeyLock(on) => {
                    self.keylock_on = on;
                }
                DeckCommand::SetKeyShift(st) => {
                    self.key_shift = st.clamp(-12.0, 12.0);
                    if let Some(kl) = self.kl.as_mut() {
                        kl.set_shift(self.key_shift);
                    }
                }
                DeckCommand::SetTrim(g) => {
                    self.trim_target = g.clamp(0.0, 8.0);
                }
                DeckCommand::SetPregain(g) => {
                    self.pregain = g.clamp(0.0, 4.0);
                    self.shared.pregain_bits.store(self.pregain.to_bits(), Ordering::Relaxed);
                }
                DeckCommand::SetEffectMode(mode) => {
                    if mode != self.effect.mode_id() {
                        let old_value = self.effect.current_value();
                        self.effect = DeckEffect::new(mode, self.output_sample_rate);
                        self.effect.set_value(old_value);
                        self.shared.effect_mode.store(mode as u32, Ordering::Relaxed);
                    }
                }
                DeckCommand::SetEffect(mut new) => {
                    if new.mode_id() != self.effect.mode_id() {
                        new.set_sample_rate(self.output_sample_rate);
                        new.set_value(self.effect.current_value());
                        self.shared.effect_mode.store(new.mode_id() as u32, Ordering::Relaxed);
                        std::mem::swap(&mut self.effect, &mut *new);
                    }
                    self.bin(Trash::Effect(new));
                }
                DeckCommand::SetTempo(ratio) => {
                    self.tempo_ratio = ratio;
                    self.shared.tempo_bits.store(ratio.to_bits(), Ordering::Relaxed);
                }
                DeckCommand::JogBend(bend) => {
                    self.bend = (self.bend + bend).clamp(-3.0, 3.0);
                }
                DeckCommand::JogScratch(v) => {
                    self.scratch_vel = v.clamp(-8.0, 8.0);
                }
                DeckCommand::JogNudge(v) => {
                    self.nudge_target = v.clamp(-NUDGE_MAX, NUDGE_MAX);
                }
                DeckCommand::JogTouch(b) | DeckCommand::SetBrake(b) => self.set_touch(b),
                DeckCommand::SetEqHi(g) => {
                    self.eq.set_hi(g);
                    self.shared.eq_hi_bits.store(g.to_bits(), Ordering::Relaxed);
                }
                DeckCommand::SetEqMid(g) => {
                    self.eq.set_mid(g);
                    self.shared.eq_mid_bits.store(g.to_bits(), Ordering::Relaxed);
                }
                DeckCommand::SetEqLo(g) => {
                    self.eq.set_lo(g);
                    self.shared.eq_lo_bits.store(g.to_bits(), Ordering::Relaxed);
                }
                DeckCommand::SetFilter(v) => {
                    self.effect.set_value(v);
                    self.shared.filter_bits.store(v.to_bits(), Ordering::Relaxed);
                }
                DeckCommand::SetLoop(region) => {
                    self.loop_region = region.and_then(|(a, b)| {
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        (hi - lo >= MIN_LOOP_FRAMES).then_some((lo, hi))
                    });
                }
                DeckCommand::Jump(pos) => self.jump_to(pos),
                DeckCommand::StopAt(pos) => {
                    self.pending_play = 0;
                    self.shared.armed.store(false, Ordering::Relaxed);
                    self.is_playing = false;
                    self.shared.is_playing.store(false, Ordering::Relaxed);
                    if self.speed_cur.abs() < 0.01 {
                        self.park(pos);
                    } else {
                        self.stop_at = Some(pos);
                    }
                }
                DeckCommand::LoadStems(s) => {
                    if let Some(old) = self.stems.replace(s) {
                        self.bin(Trash::Stems(old));
                    }
                    self.shared.stems_loaded.store(true, Ordering::Relaxed);
                }
                DeckCommand::ClearStems => {
                    if let Some(old) = self.stems.take() {
                        self.bin(Trash::Stems(old));
                    }
                    self.shared.stems_loaded.store(false, Ordering::Relaxed);
                }
                DeckCommand::SetStemGain(idx, g) => {
                    if idx < 4 {
                        self.stem_gains[idx] = g.clamp(0.0, 1.0);
                    }
                }
            }
        }
    }

    /// Moves the playhead without touching speed; crossfades from the old position
    /// when audio is running so the splice does not click.
    fn jump_to(&mut self, pos: f64) {
        let Some(track) = &self.track else { return };
        let max = (track.duration_frames as f64 - 1.0).max(0.0);
        if self.speed_cur.abs() > 0.01 {
            self.xfade_pos = self.frac_pos;
            self.xfade_left = XFADE_FRAMES;
        }
        self.frac_pos = pos.clamp(0.0, max);
        self.kl_restart();
        self.shared.position_frames.store(self.frac_pos as u64, Ordering::Relaxed);
    }

    /// The stretcher's sound is no longer where the playhead is: play the plain signal for a moment, then bring the
    /// stretcher back in at the new place.
    fn kl_restart(&mut self) {
        self.kl_active = false;
        self.kl_mix = 0.0;
    }

    /// Parks a silent playhead at `pos`.
    fn park(&mut self, pos: f64) {
        let Some(track) = &self.track else { return };
        let max = (track.duration_frames as f64 - 1.0).max(0.0);
        self.frac_pos = pos.clamp(0.0, max);
        self.kl_restart();
        self.speed_cur = 0.0;
        self.xfade_left = 0;
        self.stop_at = None;
        self.shared.position_frames.store(self.frac_pos as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn cmd_rx_push_stop(&mut self, pos: f64) {
        self.is_playing = false;
        self.stop_at = Some(pos);
    }

    /// Source signal at `pos` after pregain (stems mixed when loaded); silence if out of range.
    #[inline]
    fn read_src(&self, pos: f64) -> (f32, f32) {
        let Some(track) = &self.track else { return (0.0, 0.0) };
        if let Some(stems) = &self.stems {
            let mut sl = 0.0f32;
            let mut sr = 0.0f32;
            for i in 0..4usize {
                if self.stem_gains[i] > 0.0 {
                    let (fl, fr) = stems.frame_interp(i, pos);
                    sl += fl * self.stem_gains[i];
                    sr += fr * self.stem_gains[i];
                }
            }
            let g = self.pregain * self.trim_cur;
            (sl * g, sr * g)
        } else {
            match interp_frame(&track.samples, track.channels, pos) {
                Some((l, r)) => {
                    let g = self.pregain * self.trim_cur;
                    (l * g, r * g)
                }
                None => (0.0, 0.0),
            }
        }
    }

    fn set_touch(&mut self, down: bool) {
        if down == self.touch {
            return;
        }
        self.touch = down;
        let sr = self.output_sample_rate as f32;
        if down {
            self.settle = (TOUCH_SETTLE_S * sr) as u32;
        } else {
            self.scratch_vel = 0.0;
            self.resume = (RESUME_WINDOW_S * sr) as u32;
        }
    }

    /// Advance one sample of speed smoothing; returns the new smoothed speed.
    #[inline]
    fn update_speed(&mut self) -> f32 {
        let sr = self.output_sample_rate as f32;
        let nr = if self.nudge_target.abs() > self.nudge_cur.abs() { NUDGE_ATTACK } else { NUDGE_RELEASE };
        self.nudge_cur = slew(self.nudge_cur, self.nudge_target, nr / sr);
        self.bend *= if self.touch { self.bend_decay_touch } else { self.bend_decay_free };
        if self.bend.abs() < 1e-5 {
            self.bend = 0.0;
        }

        let (target, rate) = if self.touch {
            let r = if self.settle > 0 {
                self.settle -= 1;
                TOUCH_STOP_RATE
            } else {
                SCRATCH_RATE
            };
            (self.scratch_vel + self.bend, r)
        } else {
            let t = if self.is_playing {
                self.tempo_ratio + (self.nudge_cur + self.bend).clamp(-NUDGE_MAX, NUDGE_MAX)
            } else {
                0.0
            };
            let r = if self.resume > 0 {
                self.resume -= 1;
                RESUME_RATE
            } else {
                START_RATE
            };
            (t, r)
        };
        let mut target = target;
        if (self.bound < 0 && target < 0.0) || (self.bound > 0 && target > 0.0) {
            target = 0.0;
        } else if target != 0.0 {
            self.bound = 0;
        }
        self.speed_cur = slew(self.speed_cur, target, rate / sr);
        self.speed_cur
    }

    /// The frame `next_frame` just produced, before the channel fader.
    pub fn pre_fader(&self) -> (f32, f32) {
        self.pre_fader
    }
    /// Playhead in source frames.
    pub fn position_frames(&self) -> u64 {
        self.shared.position_frames.load(Ordering::Relaxed)
    }
    /// True while this player is playing (not paused, not parked).
    pub fn is_playing(&self) -> bool {
        self.is_playing
    }
    /// Stops playback at once (what `DeckCommand::Pause` does), for the mixer to call on a player it owns.
    pub fn stop_now(&mut self) {
        self.pending_play = 0;
        self.shared.armed.store(false, Ordering::Relaxed);
        self.is_playing = false;
        self.shared.is_playing.store(false, Ordering::Relaxed);
    }

    /// Returns the next interpolated stereo sample pair.
    /// Allocation-free — safe to call in a real-time audio callback.
    pub fn next_frame(&mut self) -> (f32, f32) {
        self.pre_fader = (0.0, 0.0);
        if self.pending_play > 0 {
            self.pending_play -= 1;
            if self.pending_play == 0 {
                self.is_playing = true;
                self.shared.is_playing.store(true, Ordering::Relaxed);
                self.shared.armed.store(false, Ordering::Relaxed);
            }
        }
        if self.track.is_none() {
            return (0.0, 0.0);
        }
        if !self.is_playing && !self.touch && self.speed_cur == 0.0 {
            return (0.0, 0.0);
        }

        self.trim_cur += (self.trim_target - self.trim_cur) * TRIM_GLIDE;
        let speed_norm = self.update_speed();

        // A stop request lands once the stop ramp has fully silenced the output.
        if let Some(p) = self.stop_at {
            if self.speed_cur.abs() < 0.01 {
                self.park(p);
                return (0.0, 0.0);
            }
        }

        let (src_rate, dur) = match &self.track {
            Some(t) => (t.sample_rate, t.duration_frames),
            None => return (0.0, 0.0),
        };
        let speed = speed_norm * src_rate as f32 / self.output_sample_rate as f32;

        let frame_i = self.frac_pos as usize;
        if self.frac_pos >= dur as f64 {
            // Ran off the end.
            self.speed_cur = 0.0;
            if self.touch {
                self.frac_pos = (dur as f64 - 1.0).max(0.0);
            } else {
                self.is_playing = false;
                self.shared.is_playing.store(false, Ordering::Relaxed);
            }
            return (0.0, 0.0);
        }

        // Signal chain: source → pregain → EQ → effect → volume (gain) → output
        let (mut l, mut r) = self.read_src(self.frac_pos);
        if self.xfade_left > 0 {
            // Blend away from the path we just left (loop wrap / jump) to avoid a click.
            let (xl, xr) = self.read_src(self.xfade_pos);
            let w = self.xfade_left as f32 / XFADE_FRAMES as f32;
            l += (xl - l) * w;
            r += (xr - r) * w;
            self.xfade_pos += speed as f64;
            self.xfade_left -= 1;
        }
        // Key lock: at a steady forward tempo other than the track's own, play through the stretcher instead.
        let (mut l, mut r) = (l, r);
        let wanted = self.keylock_on
            && self.is_playing
            && !self.touch
            && speed_norm > KL_MIN_TEMPO
            && speed_norm < KL_MAX_TEMPO
            && ((speed_norm - 1.0).abs() > KL_DEAD_BAND || self.key_shift != 0.0);
        if wanted || self.kl_active {
            if let Some(mut kl) = self.kl.take() {
                let rs = src_rate as f64 / self.output_sample_rate as f64;
                let (tempo, per) = (speed_norm as f64, speed as f64);
                if wanted && (!self.kl_active || kl.lost(self.frac_pos, per)) {
                    kl.engage(self.frac_pos, tempo, rs, &mut |p| self.read_src(p));
                    self.kl_active = true;
                    self.kl_mix = 0.0;
                    self.kl_hold = XFADE_FRAMES + 64;
                }
                let (sl, sr_) = kl.next(self.frac_pos, per, tempo, rs, &mut |p| self.read_src(p));
                self.kl = Some(kl);
                let target = if wanted && self.kl_hold == 0 { 1.0 } else { 0.0 };
                if self.kl_hold > 0 {
                    self.kl_hold -= 1;
                }
                self.kl_mix += (target - self.kl_mix).clamp(-KL_MIX_STEP, KL_MIX_STEP);
                l += (sl - l) * self.kl_mix;
                r += (sr_ - r) * self.kl_mix;
                if !wanted && self.kl_mix <= 0.0 {
                    self.kl_active = false;
                }
            }
        }
        let fade = (speed_norm.abs() / MUTE_SPEED).min(1.0);
        let (l, r) = self.eq.process(l * fade, r * fade);
        let (l, r) = self.effect.process(l, r);
        self.pre_fader = (l, r);
        let l = l * self.gain;
        let r = r * self.gain;

        let max_pos = dur as f64;
        let mut np = self.frac_pos + speed as f64;
        if let Some((lin, lout)) = self.loop_region {
            // Wrap only when the playhead crosses an edge from inside, so a loop set
            // behind or ahead of the playhead does not grab it. The overshoot is kept,
            // so loop length stays sample-exact.
            if speed > 0.0 && self.frac_pos < lout && np >= lout {
                self.xfade_pos = np;
                self.xfade_left = XFADE_FRAMES;
                np = lin + (np - lout);
                self.kl_restart();
            } else if speed < 0.0 && self.frac_pos >= lin && np < lin {
                self.xfade_pos = np;
                self.xfade_left = XFADE_FRAMES;
                np = lout - (lin - np);
                self.kl_restart();
            }
        }
        if np <= 0.0 {
            self.frac_pos = 0.0;
            self.bound = -1;
        } else if np >= max_pos {
            if self.touch {
                self.frac_pos = (max_pos - 1.0).max(0.0);
                self.bound = 1;
            } else {
                self.frac_pos = max_pos;
            }
        } else {
            self.frac_pos = np;
        }

        self.update_counter += 1;
        if self.update_counter >= 256 {
            self.update_counter = 0;
            self.shared.position_frames.store(frame_i as u64, Ordering::Relaxed);
        }

        (l, r)
    }
}

// ---------------------------------------------------------------------------
// UI-thread side
// ---------------------------------------------------------------------------

/// A loop region in source frames; `active` loops are running in the engine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopRegion {
    pub start: f64,
    pub end: f64,
    pub active: bool,
}

/// Auto-loop sizes in beats (rekordbox: 1/4 .. 32), and the default.
pub const LOOP_SIZES: [f32; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
pub const DEFAULT_LOOP_BEATS: f32 = 4.0;

/// Tempo fader range (+-8 %).
pub const TEMPO_RANGE: f32 = 0.08;

pub struct DeckHandle {
    pub shared: Arc<DeckShared>,
    pub track: Option<Arc<DecodedTrack>>,
    /// Original file path — kept so stem analysis can re-open the same file.
    pub track_path: Option<std::path::PathBuf>,
    /// Active beat grid (analysis result, or a saved manual override).
    pub grid: BeatGrid,
    /// What the analyser found, so a manual override can be reset.
    pub analyzed_grid: BeatGrid,
    /// Whole-number BPM the deck is playing at (effective tempo).
    pub target_bpm: f32,
    /// Main cue point (source frames): where CUE returns to and where the track loads.
    pub cue_point: f64,
    /// Hot cues / hot loops saved on pads 1-8 (persisted per track by the app).
    pub cues: Vec<Cue>,
    /// Loop-in set but loop-out not yet pressed.
    pub loop_in: Option<f64>,
    pub loop_region: Option<LoopRegion>,
    /// Size of the next auto loop, in beats.
    pub auto_loop_beats: f32,
    /// Snap cue and loop points to the beat grid.
    pub quantize: bool,
    cmd_tx: ringbuf::HeapProd<DeckCommand>,
    trash_rx: ringbuf::HeapCons<Trash>,
}

impl DeckHandle {
    pub fn send(&mut self, cmd: DeckCommand) {
        // an effect is built here, so its buffers are not allocated on the audio thread
        let cmd = match cmd {
            DeckCommand::SetEffectMode(mode) => {
                DeckCommand::SetEffect(Box::new(DeckEffect::new(mode, self.shared.output_rate.load(Ordering::Relaxed))))
            }
            other => other,
        };
        // Best-effort push — drop if queue full (shouldn't happen at normal UI rates).
        let _ = self.cmd_tx.try_push(cmd);
    }

    /// Frees what the audio thread handed back (old tracks, stems and effects). Call it every frame or so.
    pub fn empty_trash(&mut self) {
        while self.trash_rx.try_pop().is_some() {}
    }

    /// Called by the UI when a decoded track arrives from the decode thread.
    pub fn load_track(&mut self, track: Arc<DecodedTrack>, path: std::path::PathBuf) {
        self.shared.duration_frames.store(track.duration_frames, Ordering::Relaxed);
        self.send(DeckCommand::Load(Arc::clone(&track)));
        self.analyzed_grid = BeatGrid { bpm: track.bpm, offset: track.beat_offset };
        self.grid = self.analyzed_grid;
        self.track = Some(track);
        self.track_path = Some(path);
        self.reset_tempo();
        self.cues.clear();
        self.loop_in = None;
        self.loop_region = None;
        self.cue_point = self.default_cue_point();
    }

    /// Source frames in one beat on this deck's grid (at the track's true tempo).
    pub fn frames_per_beat(&self) -> Option<f64> {
        let t = self.track.as_ref()?;
        if !self.grid.is_valid() {
            return None;
        }
        Some(60.0 / self.grid.bpm as f64 * t.sample_rate as f64)
    }

    /// Cue point for a track with none saved: the first downbeat, else the start.
    pub fn default_cue_point(&self) -> f64 {
        match (&self.track, self.grid.is_valid()) {
            (Some(t), true) => {
                let f = self.grid.offset * WAVEFORM_CHUNK_FRAMES as f64;
                if f > 0.0 && f < t.duration_frames as f64 {
                    f
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }

    /// Nearest beat-grid position (source frames); unchanged without a grid.
    pub fn snap_to_beat(&self, frame: f64) -> f64 {
        let (Some(t), Some(fpb)) = (&self.track, self.frames_per_beat()) else { return frame };
        let anchor = self.grid.offset * WAVEFORM_CHUNK_FRAMES as f64;
        let beat = ((frame - anchor) / fpb).round();
        (anchor + beat * fpb).clamp(0.0, (t.duration_frames as f64 - 1.0).max(0.0))
    }

    /// Current playhead in source frames (6 ms granularity from the audio thread).
    pub fn position(&self) -> f64 {
        self.shared.position_frames.load(Ordering::Relaxed) as f64
    }

    /// Whole-number BPM shown for the track (100 stands in when there is no grid,
    /// which makes the tempo control a whole-percent pitch fader).
    pub fn nominal_bpm(&self) -> f32 {
        if self.grid.is_valid() {
            self.grid.bpm.round().max(1.0)
        } else {
            100.0
        }
    }

    /// Reachable whole-BPM range of the tempo control.
    pub fn tempo_bounds(&self) -> (f32, f32) {
        let n = self.nominal_bpm();
        ((n * (1.0 - TEMPO_RANGE)).ceil(), (n * (1.0 + TEMPO_RANGE)).floor())
    }

    /// Sets the playing tempo to a whole BPM (clamped to the range). The engine
    /// ratio is `target / true tempo`, so a 173.8 BPM track plays at exactly 174.
    pub fn set_target_bpm(&mut self, bpm: f32) -> f32 {
        let (lo, hi) = self.tempo_bounds();
        let t = bpm.round().clamp(lo, hi);
        let native = if self.grid.is_valid() { self.grid.bpm } else { 100.0 };
        self.target_bpm = t;
        self.send(DeckCommand::SetTempo(t / native));
        t
    }

    pub fn reset_tempo(&mut self) {
        let n = self.nominal_bpm();
        self.set_target_bpm(n);
    }

    /// Tempo offset in percent relative to the whole-number nominal BPM.
    pub fn tempo_percent_vs_nominal(&self) -> f32 {
        (self.target_bpm / self.nominal_bpm() - 1.0) * 100.0
    }

    /// Replaces the beat grid (e.g. manual BPM edit) and resets tempo to nominal.
    pub fn set_grid(&mut self, grid: BeatGrid) {
        self.grid = grid;
        self.reset_tempo();
    }

    /// Fractional beat position of the playhead on this deck's grid.
    pub fn beat_now(&self) -> Option<f64> {
        let t = self.track.as_ref()?;
        if !self.grid.is_valid() {
            return None;
        }
        Some(self.grid.beat_at_frame(t.sample_rate, self.shared.position_frames.load(Ordering::Relaxed)))
    }
}

pub fn create_deck_pair(shared: Arc<DeckShared>) -> (DeckHandle, DeckPlayer) {
    let rb = HeapRb::<DeckCommand>::new(64);
    let (tx, rx) = rb.split();
    let mut player = DeckPlayer::new(Arc::clone(&shared), rx);
    let (trash_tx, trash_rx) = HeapRb::<Trash>::new(32).split();
    player.trash = Some(trash_tx);
    let handle = DeckHandle {
        trash_rx,
        shared,
        track: None,
        track_path: None,
        grid: BeatGrid { bpm: 0.0, offset: 0.0 },
        analyzed_grid: BeatGrid { bpm: 0.0, offset: 0.0 },
        target_bpm: 100.0,
        cue_point: 0.0,
        cues: Vec::new(),
        loop_in: None,
        loop_region: None,
        auto_loop_beats: DEFAULT_LOOP_BEATS,
        quantize: true,
        cmd_tx: tx,
    };
    (handle, player)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_track(frames: usize) -> Arc<DecodedTrack> {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = ((i as f32) * 0.05).sin() * 0.5;
            samples.push(v);
            samples.push(v);
        }
        Arc::new(DecodedTrack {
            loudness: None,
            samples,
            sample_rate: 44100,
            channels: 2,
            duration_frames: frames as u64,
            waveform: vec![],
            wave: Default::default(),
            title: "t".into(),
            duration_secs: frames as f64 / 44100.0,
            bpm: 0.0,
            bpm_coarse: 0,
            beat_offset: 0.0,
            beat_times_ms: vec![],
            key: None,
            cues: vec![],
        })
    }

    fn rig(frames: usize) -> (DeckHandle, DeckPlayer) {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        p.set_output_sample_rate(44100);
        h.send(DeckCommand::Load(test_track(frames)));
        h.send(DeckCommand::Play);
        p.drain_commands();
        (h, p)
    }

    #[test]
    fn slew_never_exceeds_step_and_lands_on_target() {
        let (mut cur, target, step) = (0.0_f32, 3.0_f32, 0.01_f32);
        let mut n = 0;
        while cur != target {
            let next = slew(cur, target, step);
            assert!((next - cur).abs() <= step + 1e-6);
            cur = next;
            n += 1;
            assert!(n < 10_000);
        }
        assert_eq!(slew(1.0, -1.0, 0.5), 0.5);
    }

    #[test]
    fn reverse_reads_are_in_bounds() {
        let t = test_track(1000);
        let mut pos = 999.0_f64;
        while pos >= 0.0 {
            assert!(interp_frame(&t.samples, 2, pos).is_some(), "pos {pos}");
            pos -= 0.37;
        }
        assert!(interp_frame(&t.samples, 2, -0.1).is_none());
        assert!(interp_frame(&t.samples, 2, 1000.0).is_none());
    }

    #[test]
    fn speed_slew_is_bounded_through_touch_scratch_release() {
        let (mut h, mut p) = rig(44100 * 4);
        let max_step = SCRATCH_RATE.max(START_RATE).max(TOUCH_STOP_RATE) / 44100.0 + 1e-6;
        let mut last = p.speed_cur;
        let run = |p: &mut DeckPlayer, n: usize, last: &mut f32| {
            for _ in 0..n {
                let (l, r) = p.next_frame();
                assert!(l.is_finite() && r.is_finite());
                assert!((p.speed_cur - *last).abs() <= max_step, "jump {} -> {}", last, p.speed_cur);
                assert!(p.frac_pos >= 0.0 && p.frac_pos <= p.track.as_ref().unwrap().duration_frames as f64);
                *last = p.speed_cur;
            }
        };
        run(&mut p, 4410, &mut last);
        assert!((p.speed_cur - 1.0).abs() < 1e-3, "should be at tempo");

        h.send(DeckCommand::JogScratch(0.0));
        h.send(DeckCommand::JogTouch(true));
        p.drain_commands();
        run(&mut p, 4410, &mut last);
        assert!(p.speed_cur.abs() < 1e-3, "touch must hold the audio");

        h.send(DeckCommand::JogScratch(-2.5));
        p.drain_commands();
        let before = p.frac_pos;
        run(&mut p, 2000, &mut last);
        assert!(p.frac_pos < before, "negative scratch moves the playhead backwards");

        h.send(DeckCommand::JogTouch(false));
        p.drain_commands();
        run(&mut p, 22050, &mut last);
        assert!((p.speed_cur - 1.0).abs() < 1e-3, "release resumes at tempo");
    }

    #[test]
    fn scratching_a_paused_deck_is_audible_and_pause_ramps_to_silence() {
        let (mut h, mut p) = rig(44100 * 2);
        h.send(DeckCommand::Pause);
        p.drain_commands();
        let mut peak_after = 0.0_f32;
        for i in 0..8000 {
            let (l, _) = p.next_frame();
            if i > 4000 {
                peak_after = peak_after.max(l.abs());
            }
        }
        assert_eq!(peak_after, 0.0, "paused deck is silent once ramp finished");

        h.send(DeckCommand::JogTouch(true));
        h.send(DeckCommand::JogScratch(1.5));
        p.drain_commands();
        let mut peak = 0.0_f32;
        for _ in 0..4000 {
            let (l, _) = p.next_frame();
            peak = peak.max(l.abs());
        }
        assert!(peak > 0.01, "scratching while paused makes sound");
    }

    #[test]
    fn nudge_decays_back_to_tempo() {
        let (mut h, mut p) = rig(44100 * 8);
        h.send(DeckCommand::JogNudge(0.3));
        p.drain_commands();
        for _ in 0..22050 {
            p.next_frame();
        }
        assert!(p.speed_cur > 1.2, "nudge raises speed, got {}", p.speed_cur);
        h.send(DeckCommand::JogNudge(0.0));
        p.drain_commands();
        for _ in 0..44100 {
            p.next_frame();
        }
        assert!((p.speed_cur - 1.0).abs() < 1e-3);
    }

    #[test]
    fn play_after_starts_on_exactly_the_requested_frame() {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        p.set_output_sample_rate(44100);
        h.send(DeckCommand::Load(test_track(100_000)));
        h.send(DeckCommand::PlayAfter(500));
        p.drain_commands();
        assert!(h.shared.armed.load(Ordering::Relaxed));
        for _ in 0..499 {
            p.next_frame();
        }
        assert!(!h.shared.is_playing.load(Ordering::Relaxed), "still waiting");
        p.next_frame();
        assert!(h.shared.is_playing.load(Ordering::Relaxed), "started on frame 500");
        assert!(!h.shared.armed.load(Ordering::Relaxed));
    }

    #[test]
    fn pause_cancels_a_pending_start() {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        h.send(DeckCommand::Load(test_track(10_000)));
        h.send(DeckCommand::PlayAfter(100));
        h.send(DeckCommand::Pause);
        p.drain_commands();
        for _ in 0..200 {
            p.next_frame();
        }
        assert!(!h.shared.is_playing.load(Ordering::Relaxed));
    }

    #[test]
    fn tempo_is_whole_bpm_and_plays_at_the_rounded_rate() {
        let (mut h, _p) = create_deck_pair(Arc::new(DeckShared::default()));
        h.grid = BeatGrid { bpm: 173.8, offset: 0.0 };
        h.reset_tempo();
        assert_eq!(h.nominal_bpm(), 174.0);
        assert_eq!(h.target_bpm, 174.0);
        // 173.8 true tempo played at 174 BPM => ratio 174 / 173.8
        let (lo, hi) = h.tempo_bounds();
        assert_eq!((lo, hi), (161.0, 187.0));
        assert_eq!(h.set_target_bpm(176.4), 176.0);
        assert_eq!(h.set_target_bpm(500.0), 187.0);
        assert_eq!(h.set_target_bpm(1.0), 161.0);
    }

    #[test]
    fn loop_wraps_sample_exactly_and_stays_inside() {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        p.set_output_sample_rate(44100);
        h.send(DeckCommand::Load(test_track(200_000)));
        h.send(DeckCommand::SetLoop(Some((10_000.0, 14_410.0))));
        h.send(DeckCommand::Jump(9_000.0));
        h.send(DeckCommand::Play);
        p.drain_commands();
        let mut entered = false;
        for _ in 0..120_000 {
            p.next_frame();
            if p.frac_pos >= 10_000.0 {
                entered = true;
            }
            if entered {
                assert!(p.frac_pos >= 10_000.0 - 1e-6 && p.frac_pos < 14_410.0 + 1e-6, "left the loop: {}", p.frac_pos);
            }
        }
        assert!(entered, "playhead should have reached the loop");
    }

    #[test]
    fn loop_does_not_grab_a_playhead_that_starts_past_it() {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        p.set_output_sample_rate(44100);
        h.send(DeckCommand::Load(test_track(200_000)));
        h.send(DeckCommand::SetLoop(Some((1_000.0, 2_000.0))));
        h.send(DeckCommand::Jump(50_000.0));
        h.send(DeckCommand::Play);
        p.drain_commands();
        for _ in 0..20_000 {
            p.next_frame();
        }
        assert!(p.frac_pos > 50_000.0, "must keep playing forward, at {}", p.frac_pos);
    }

    #[test]
    fn jump_while_playing_crossfades_and_keeps_speed() {
        let (_h, mut p) = rig(200_000);
        for _ in 0..20_000 {
            p.next_frame();
        }
        let speed_before = p.speed_cur;
        assert!(speed_before > 0.9);
        p.jump_to(100_000.0);
        assert_eq!(p.xfade_left, XFADE_FRAMES);
        assert_eq!(p.frac_pos, 100_000.0);
        assert!((p.speed_cur - speed_before).abs() < 1e-6, "speed must not reset");
    }

    #[test]
    fn crossfaded_loop_wrap_has_no_large_sample_jump() {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        p.set_output_sample_rate(44100);
        h.send(DeckCommand::Load(test_track(200_000)));
        h.send(DeckCommand::SetLoop(Some((10_000.0, 10_700.0))));
        h.send(DeckCommand::Jump(10_000.0));
        h.send(DeckCommand::Play);
        p.drain_commands();
        for _ in 0..8_000 {
            p.next_frame();
        } // reach full speed
        let mut prev = p.next_frame().0;
        let mut worst = 0.0f32;
        for _ in 0..20_000 {
            let cur = p.next_frame().0;
            worst = worst.max((cur - prev).abs());
            prev = cur;
        }
        // Source is a 0.05 rad/sample sine of amplitude 0.5: natural step <= 0.025.
        assert!(worst < 0.12, "click at loop wrap: {worst}");
    }

    #[test]
    fn stop_at_parks_on_the_cue_after_the_stop_ramp() {
        let (_h, mut p) = rig(200_000);
        for _ in 0..30_000 {
            p.next_frame();
        }
        p.cmd_rx_push_stop(5_000.0);
        for _ in 0..30_000 {
            p.next_frame();
        }
        assert_eq!(p.frac_pos, 5_000.0);
        assert!(!p.is_playing);
        assert_eq!(p.speed_cur, 0.0);
    }

    #[test]
    fn snap_to_beat_lands_on_grid_and_handles_no_grid() {
        let (mut h, _p) = create_deck_pair(Arc::new(DeckShared::default()));
        h.track = Some(test_track(441_000));
        assert_eq!(h.snap_to_beat(12_345.0), 12_345.0, "no grid: unchanged");
        h.grid = BeatGrid { bpm: 120.0, offset: 0.0 }; // 22_050 frames per beat
        assert_eq!(h.snap_to_beat(22_000.0), 22_050.0);
        assert_eq!(h.snap_to_beat(33_000.0), 22_050.0);
        assert_eq!(h.snap_to_beat(33_200.0), 44_100.0);
    }

    #[test]
    fn an_empty_deck_ignores_play_so_a_track_can_still_be_loaded() {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        h.send(DeckCommand::Play);
        h.send(DeckCommand::PlayAfter(0));
        h.send(DeckCommand::PlayAfter(100));
        p.drain_commands();
        assert!(!h.shared.is_playing.load(Ordering::Relaxed));
        assert!(!h.shared.armed.load(Ordering::Relaxed));
        h.send(DeckCommand::Load(test_track(1000)));
        h.send(DeckCommand::Play);
        p.drain_commands();
        assert!(h.shared.is_playing.load(Ordering::Relaxed), "playing works once something is loaded");
    }

    fn custom_track(frames: usize, f: impl Fn(usize) -> f32) -> Arc<DecodedTrack> {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = f(i);
            samples.push(v);
            samples.push(v);
        }
        Arc::new(DecodedTrack {
            loudness: None,
            samples,
            sample_rate: 44100,
            channels: 2,
            duration_frames: frames as u64,
            waveform: vec![],
            wave: Default::default(),
            title: "t".into(),
            duration_secs: frames as f64 / 44100.0,
            bpm: 0.0,
            bpm_coarse: 0,
            beat_offset: 0.0,
            beat_times_ms: vec![],
            key: None,
            cues: vec![],
        })
    }

    /// Plays a 1 kHz tone at `tempo` and returns the frequency heard in the last second.
    fn heard_hz(key_lock: bool, tempo: f32) -> f64 {
        let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
        p.set_output_sample_rate(44100);
        h.send(DeckCommand::Load(custom_track(44100 * 8, |i| {
            (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 44100.0).sin() * 0.4
        })));
        h.send(DeckCommand::SetKeyLock(key_lock));
        h.send(DeckCommand::SetTempo(tempo));
        h.send(DeckCommand::Play);
        p.drain_commands();
        let mut out = Vec::new();
        for _ in 0..44100 * 3 {
            out.push(p.next_frame().0);
        }
        let tail = &out[44100 * 2..];
        let crossings = tail.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f64
    }

    #[test]
    fn key_lock_keeps_the_pitch_and_without_it_the_pitch_follows_the_tempo() {
        let locked = heard_hz(true, 1.08);
        let free = heard_hz(false, 1.08);
        assert!((locked - 1000.0).abs() < 15.0, "locked {locked} Hz");
        assert!((free - 1080.0).abs() < 15.0, "free {free} Hz");
    }

    #[test]
    fn key_shift_moves_the_pitch_by_semitones_at_any_tempo() {
        for &(tempo, shift) in &[(1.0f32, 2.0f32), (1.0, -3.0), (1.06, 2.0), (0.94, -1.0)] {
            let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
            p.set_output_sample_rate(44100);
            h.send(DeckCommand::Load(custom_track(44100 * 8, |i| {
                (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 44100.0).sin() * 0.4
            })));
            h.send(DeckCommand::SetKeyLock(true));
            h.send(DeckCommand::SetKeyShift(shift));
            h.send(DeckCommand::SetTempo(tempo));
            h.send(DeckCommand::Play);
            p.drain_commands();
            let mut out = Vec::new();
            for _ in 0..44100 * 3 {
                out.push(p.next_frame().0);
            }
            let tail = &out[44100 * 2..];
            let hz = tail.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64;
            let want = 1000.0 * 2f64.powf(shift as f64 / 12.0);
            assert!((hz - want).abs() < want * 0.015, "tempo {tempo} shift {shift}: {hz} Hz, wanted {want}");
        }
    }

    #[test]
    fn with_key_lock_a_click_is_heard_when_the_playhead_reaches_it() {
        for &tempo in &[0.93f32, 1.06, 1.12] {
            let click_at = 44100 * 3 + 777;
            let (mut h, mut p) = create_deck_pair(Arc::new(DeckShared::default()));
            p.set_output_sample_rate(44100);
            h.send(DeckCommand::Load(custom_track(44100 * 8, move |i| {
                let d = i as i64 - click_at as i64;
                if (0..64).contains(&d) {
                    (2.0 * std::f32::consts::PI * 2000.0 * d as f32 / 44100.0).sin()
                        * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * d as f32 / 64.0).cos())
                } else {
                    0.0
                }
            })));
            h.send(DeckCommand::SetKeyLock(true));
            h.send(DeckCommand::SetTempo(tempo));
            h.send(DeckCommand::Play);
            p.drain_commands();
            let mut best = (0f32, 0f64);
            for _ in 0..44100 * 5 {
                let pos = p.frac_pos;
                let (l, _) = p.next_frame();
                if l.abs() > best.0 {
                    best = (l.abs(), pos);
                }
            }
            assert!(best.0 > 0.2, "tempo {tempo}: no click heard");
            // 1.5 ms: well inside what a beat grid or a loop edge can notice
            assert!(
                (best.1 - (click_at as f64 + 20.0)).abs() < 66.0,
                "tempo {tempo}: heard with the playhead at {} but the click is at {}",
                best.1,
                click_at + 20
            );
        }
    }
}
