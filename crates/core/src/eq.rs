use std::f32::consts::PI;

// ── Direct-Form I stereo biquad ──────────────────────────────────────────────

#[derive(Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    xl1: f32,
    xl2: f32,
    yl1: f32,
    yl2: f32,
    xr1: f32,
    xr2: f32,
    yr1: f32,
    yr2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            xl1: 0.0,
            xl2: 0.0,
            yl1: 0.0,
            yl2: 0.0,
            xr1: 0.0,
            xr2: 0.0,
            yr1: 0.0,
            yr2: 0.0,
        }
    }
}

impl Biquad {
    fn from_coeffs(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        let s = 1.0 / a0;
        Self { b0: b0 * s, b1: b1 * s, b2: b2 * s, a1: a1 * s, a2: a2 * s, ..Default::default() }
    }

    fn update_coeffs(&mut self, src: &Biquad) {
        self.b0 = src.b0;
        self.b1 = src.b1;
        self.b2 = src.b2;
        self.a1 = src.a1;
        self.a2 = src.a2;
    }

    fn reset_state(&mut self) {
        self.xl1 = 0.0;
        self.xl2 = 0.0;
        self.yl1 = 0.0;
        self.yl2 = 0.0;
        self.xr1 = 0.0;
        self.xr2 = 0.0;
        self.yr1 = 0.0;
        self.yr2 = 0.0;
    }

    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let yl = self.b0 * l + self.b1 * self.xl1 + self.b2 * self.xl2 - self.a1 * self.yl1 - self.a2 * self.yl2;
        let yr = self.b0 * r + self.b1 * self.xr1 + self.b2 * self.xr2 - self.a1 * self.yr1 - self.a2 * self.yr2;
        self.xl2 = self.xl1;
        self.xl1 = l;
        self.yl2 = self.yl1;
        self.yl1 = yl;
        self.xr2 = self.xr1;
        self.xr1 = r;
        self.yr2 = self.yr1;
        self.yr1 = yr;
        (yl, yr)
    }
}

// ── RBJ cookbook coefficient builders ────────────────────────────────────────

fn low_shelf(sr: u32, freq: f32, gain_lin: f32) -> Biquad {
    let a = gain_lin.max(1e-6_f32).sqrt();
    let w0 = 2.0 * PI * freq / sr as f32;
    let cw = w0.cos();
    let sw = w0.sin();
    let alph = sw / 2.0 * 2.0_f32.sqrt();
    let s2a = 2.0 * a.sqrt();
    Biquad::from_coeffs(
        a * ((a + 1.0) - (a - 1.0) * cw + s2a * alph),
        2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
        a * ((a + 1.0) - (a - 1.0) * cw - s2a * alph),
        (a + 1.0) + (a - 1.0) * cw + s2a * alph,
        -2.0 * ((a - 1.0) + (a + 1.0) * cw),
        (a + 1.0) + (a - 1.0) * cw - s2a * alph,
    )
}

fn high_shelf(sr: u32, freq: f32, gain_lin: f32) -> Biquad {
    let a = gain_lin.max(1e-6_f32).sqrt();
    let w0 = 2.0 * PI * freq / sr as f32;
    let cw = w0.cos();
    let sw = w0.sin();
    let alph = sw / 2.0 * 2.0_f32.sqrt();
    let s2a = 2.0 * a.sqrt();
    Biquad::from_coeffs(
        a * ((a + 1.0) + (a - 1.0) * cw + s2a * alph),
        -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
        a * ((a + 1.0) + (a - 1.0) * cw - s2a * alph),
        (a + 1.0) - (a - 1.0) * cw + s2a * alph,
        2.0 * ((a - 1.0) - (a + 1.0) * cw),
        (a + 1.0) - (a - 1.0) * cw - s2a * alph,
    )
}

fn peak_eq(sr: u32, freq: f32, q: f32, gain_lin: f32) -> Biquad {
    let a = gain_lin.max(1e-6_f32).sqrt();
    let w0 = 2.0 * PI * freq / sr as f32;
    let cw = w0.cos();
    let alph = w0.sin() / (2.0 * q);
    Biquad::from_coeffs(1.0 + alph * a, -2.0 * cw, 1.0 - alph * a, 1.0 + alph / a, -2.0 * cw, 1.0 - alph / a)
}

fn low_pass(sr: u32, freq: f32, q: f32) -> Biquad {
    let w0 = 2.0 * PI * freq / sr as f32;
    let cw = w0.cos();
    let alph = w0.sin() / (2.0 * q);
    Biquad::from_coeffs((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alph, -2.0 * cw, 1.0 - alph)
}

fn high_pass(sr: u32, freq: f32, q: f32) -> Biquad {
    let w0 = 2.0 * PI * freq / sr as f32;
    let cw = w0.cos();
    let alph = w0.sin() / (2.0 * q);
    Biquad::from_coeffs((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alph, -2.0 * cw, 1.0 - alph)
}

// First-order allpass: y[n] = -c*x[n] + x[n-1] + c*y[n-1]   (c = tan(π*f/sr) - 1)/(tan(...)+1))
fn allpass_coeff(freq: f32, sr: u32) -> f32 {
    let t = (PI * freq / sr as f32).tan();
    (t - 1.0) / (t + 1.0)
}

// ── 3-band DJ EQ ─────────────────────────────────────────────────────────────

/// The three-band channel EQ. A knob move sets a target; the gains glide to it over a few milliseconds and the filters
/// keep their state while their coefficients change, so turning a knob neither clicks nor zippers. (Swapping in a new
/// filter with empty state on every knob message, as this once did, puts a step in the signal each time.)
pub struct DeckEq {
    lo: Biquad,
    mid: Biquad,
    hi: Biquad,
    sr: u32,
    /// Where the knobs are.
    gains: [f32; 3],
    /// Where the filters are, on their way there.
    cur: [f32; 3],
    tick: u32,
}

/// The gains move every this many samples (a power of two)...
const EQ_GLIDE_EVERY: u32 = 32;
/// ...by this share of what is left: about 5 ms to get most of the way at 44.1 kHz.
const EQ_GLIDE: f32 = 0.15;

impl DeckEq {
    pub fn new(sr: u32) -> Self {
        let mut e = Self {
            lo: Biquad::default(),
            mid: Biquad::default(),
            hi: Biquad::default(),
            sr,
            gains: [1.0; 3],
            cur: [1.0; 3],
            tick: 0,
        };
        e.rebuild();
        e
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            self.sr = sr;
            self.rebuild();
        }
    }

    pub fn set_lo(&mut self, g: f32) {
        self.gains[0] = g.clamp(0.0, 2.0);
    }
    pub fn set_mid(&mut self, g: f32) {
        self.gains[1] = g.clamp(0.0, 2.0);
    }
    pub fn set_hi(&mut self, g: f32) {
        self.gains[2] = g.clamp(0.0, 2.0);
    }

    fn band(&self, i: usize) -> Biquad {
        let g = self.cur[i].max(1e-6);
        match i {
            0 => low_shelf(self.sr, 200.0, g),
            1 => peak_eq(self.sr, 1000.0, 1.2, g),
            _ => high_shelf(self.sr, 8000.0, g),
        }
    }

    /// New coefficients for all three bands at the gains they are at now; the filters' state is kept.
    fn rebuild(&mut self) {
        let (lo, mid, hi) = (self.band(0), self.band(1), self.band(2));
        self.lo.update_coeffs(&lo);
        self.mid.update_coeffs(&mid);
        self.hi.update_coeffs(&hi);
    }

    /// One step of the glide, for the bands that are not where their knob is.
    fn glide(&mut self) {
        for i in 0..3 {
            let left = self.gains[i] - self.cur[i];
            if left == 0.0 {
                continue;
            }
            self.cur[i] = if left.abs() < 1e-4 { self.gains[i] } else { self.cur[i] + left * EQ_GLIDE };
            let b = self.band(i);
            match i {
                0 => self.lo.update_coeffs(&b),
                1 => self.mid.update_coeffs(&b),
                _ => self.hi.update_coeffs(&b),
            }
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.tick = self.tick.wrapping_add(1);
        if self.tick & (EQ_GLIDE_EVERY - 1) == 0 && self.cur != self.gains {
            self.glide();
        }
        let (l, r) = self.lo.tick(l, r);
        let (l, r) = self.mid.tick(l, r);
        self.hi.tick(l, r)
    }
}

// ── LP/HP sweep filter ────────────────────────────────────────────────────────

const FILTER_Q: f32 = 0.85;
const EFFECT_DEAD: f32 = 0.025;

/// Rekordbox-style sweep filter: 0.0 = deep LP, 0.5 = bypass, 1.0 = deep HP.
/// Uses coefficient smoothing (every 64 samples) to prevent clicks during sweep.
pub struct FilterKnob {
    pub value: f32,
    smooth_val: f32,
    filter: Biquad,
    active: bool,
    sr: u32,
    tick: u32,
}

impl FilterKnob {
    pub fn new(sr: u32) -> Self {
        Self { value: 0.5, smooth_val: 0.5, filter: Biquad::default(), active: false, sr, tick: 0 }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            self.sr = sr;
            self.rebuild_from_smooth();
        }
    }

    fn rebuild_from_smooth(&mut self) {
        let v = self.smooth_val;
        let was_active = self.active;
        if (v - 0.5).abs() <= EFFECT_DEAD {
            if was_active {
                self.filter.reset_state();
            }
            self.active = false;
            return;
        }
        let new_filter = if v < 0.5 {
            let t = v / (0.5 - EFFECT_DEAD);
            let freq = 60.0_f32 * (18_000.0_f32 / 60.0).powf(t);
            low_pass(self.sr, freq.clamp(20.0, 20_000.0), FILTER_Q)
        } else {
            let t = ((v - 0.5 - EFFECT_DEAD) / (0.5 - EFFECT_DEAD)).max(0.0);
            let freq = 25.0_f32 * (16_000.0_f32 / 25.0).powf(t);
            high_pass(self.sr, freq.clamp(20.0, 20_000.0), FILTER_Q)
        };
        if !was_active {
            self.filter = new_filter;
        } else {
            self.filter.update_coeffs(&new_filter);
        }
        self.active = true;
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        self.tick = self.tick.wrapping_add(1);
        if self.tick & 63 == 0 && self.smooth_val != self.value {
            let left = self.value - self.smooth_val;
            self.smooth_val = if left.abs() < 1e-5 { self.value } else { self.smooth_val + left * 0.20 };
            self.rebuild_from_smooth();
        }
        if self.active {
            self.filter.tick(l, r)
        } else {
            (l, r)
        }
    }
}

// ── Shared effect helpers ────────────────────────────────────────────────────
//
// Every effect below follows the rekordbox Color-FX convention: knob centre =
// effect fully off (bit-exact dry), and the signed knob position is smoothed
// per sample so quick knob moves, side changes and engage/disengage never click.

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-18 {
        0.0
    } else {
        x
    }
}
#[inline]
fn soft(x: f32) -> f32 {
    x.tanh()
}
#[inline]
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn smooth_coef(sr: u32, tau: f32) -> f32 {
    1.0 - (-1.0 / (tau * sr as f32)).exp()
}
#[inline]
fn onepole_coef(fc: f32, sr: u32) -> f32 {
    1.0 - (-2.0 * PI * fc / sr as f32).exp()
}

/// One-pole low-pass tick on an external state.
#[inline]
fn lp1(z: &mut f32, a: f32, x: f32) -> f32 {
    *z = flush(*z + a * (x - *z));
    *z
}

/// Per-sample one-pole parameter smoother; snaps to target when within 1e-5.
#[derive(Clone, Copy)]
struct Smooth {
    v: f32,
    k: f32,
}

impl Smooth {
    fn new(v: f32, sr: u32, tau: f32) -> Self {
        Self { v, k: smooth_coef(sr, tau) }
    }
    #[inline]
    fn tick(&mut self, target: f32) -> f32 {
        self.v += (target - self.v) * self.k;
        if (target - self.v).abs() < 1e-5 {
            self.v = target;
        }
        self.v
    }
}

/// Knob (0..1, centre 0.5) → signed position in [-1, 1] with a dead zone at centre.
#[inline]
fn knob_signed(v: f32) -> f32 {
    let d = (v - 0.5).abs();
    if d <= EFFECT_DEAD {
        return 0.0;
    }
    let depth = ((d - EFFECT_DEAD) / (0.5 - EFFECT_DEAD)).clamp(0.0, 1.0);
    if v < 0.5 {
        -depth
    } else {
        depth
    }
}

#[inline]
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// Circular delay line with 4-point Hermite fractional reads.
/// `read(d)` returns the signal `d` samples ago (d is clamped to [2, len-4]), so a
/// modulated delay can never go negative or read the not-yet-written slot.
struct DelayLine {
    buf: Box<[f32]>,
    w: usize,
}

impl DelayLine {
    fn new(max_samples: usize) -> Self {
        Self { buf: vec![0.0; max_samples + 8].into_boxed_slice(), w: 0 }
    }

    #[inline]
    fn read(&self, d: f32) -> f32 {
        let n = self.buf.len();
        let d = d.clamp(2.0, (n - 4) as f32);
        let di = d as usize;
        let t = d - di as f32;
        let at = |k: usize| self.buf[(self.w + n - k) % n];
        hermite(at(di - 1), at(di), at(di + 1), at(di + 2), t)
    }

    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.w] = x;
        self.w += 1;
        if self.w == self.buf.len() {
            self.w = 0;
        }
    }
}

/// Fixed-length ring used by the reverb combs / allpasses.
struct Ring {
    buf: Box<[f32]>,
    p: usize,
}

impl Ring {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(2)].into_boxed_slice(), p: 0 }
    }
    #[inline]
    fn cur(&self) -> f32 {
        self.buf[self.p]
    }
    #[inline]
    fn push(&mut self, x: f32) {
        self.buf[self.p] = x;
        self.p += 1;
        if self.p == self.buf.len() {
            self.p = 0;
        }
    }
}

/// Zavalishin/Cytomic TPT state-variable filter. Stable under per-sample
/// cutoff modulation, which is what makes the Sweep glide cleanly.
#[derive(Clone, Copy, Default)]
struct Svf {
    ic1: [f32; 2],
    ic2: [f32; 2],
}

impl Svf {
    /// Returns (low, band, high).
    #[inline]
    fn tick(&mut self, ch: usize, x: f32, g: f32, k: f32) -> (f32, f32, f32) {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2[ch];
        let v1 = a1 * self.ic1[ch] + a2 * v3;
        let v2 = self.ic2[ch] + a2 * self.ic1[ch] + a3 * v3;
        self.ic1[ch] = flush(2.0 * v1 - self.ic1[ch]);
        self.ic2[ch] = flush(2.0 * v2 - self.ic2[ch]);
        (v2, v1, x - k * v1 - v2)
    }
}

// ── Echo (rekordbox DUB ECHO) ────────────────────────────────────────────────
//
// Evidence (Pioneer DJM manual): turn left → echo only the MID band, turn right
// → echo only the HIGH band; delay time shifts with the knob. Centre = off.
// Left: mids (300 Hz–3 kHz) into a slightly longer delay; right: highs
// (>2.5 kHz) into a slightly shorter one. Feedback runs through a 3.5 kHz
// damper and tanh so repeats degrade like a tape/dub echo and can never blow up.

const ECHO_BASE_SECS: f32 = 0.375;
const ECHO_BUF_SECS: f32 = 0.6;

pub struct EchoEffect {
    dl: [DelayLine; 2],
    sr: u32,
    pub value: f32,
    pos: Smooth,
    time: Smooth,
    z_lo: [f32; 2],
    z_mid: [f32; 2],
    z_hi: [f32; 2],
    z_fb: [f32; 2],
    a_lo: f32,
    a_mid: f32,
    a_hi: f32,
    a_fb: f32,
}

impl EchoEffect {
    pub fn new(sr: u32) -> Self {
        let n = (sr as f32 * ECHO_BUF_SECS) as usize;
        Self {
            dl: [DelayLine::new(n), DelayLine::new(n)],
            sr,
            value: 0.5,
            pos: Smooth::new(0.0, sr, 0.012),
            time: Smooth::new(ECHO_BASE_SECS, sr, 0.25),
            z_lo: [0.0; 2],
            z_mid: [0.0; 2],
            z_hi: [0.0; 2],
            z_fb: [0.0; 2],
            a_lo: onepole_coef(300.0, sr),
            a_mid: onepole_coef(3000.0, sr),
            a_hi: onepole_coef(2500.0, sr),
            a_fb: onepole_coef(3500.0, sr),
        }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            let v = self.value;
            *self = Self::new(sr);
            self.value = v;
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pos = self.pos.tick(knob_signed(self.value));
        let depth = pos.abs();
        let t = self.time.tick(ECHO_BASE_SECS * (1.0 - 0.30 * pos));
        let d = t * self.sr as f32;
        let send_gain = (depth * 6.0).min(1.0);
        let wet = depth.powf(0.6) * 0.95;
        let fb = 0.50 + 0.08 * depth;

        let ins = [l, r];
        let mut out = [0.0_f32; 2];
        for ch in 0..2 {
            let x = ins[ch];
            let lo = lp1(&mut self.z_lo[ch], self.a_lo, x);
            let mid = lp1(&mut self.z_mid[ch], self.a_mid, x - lo);
            let hi = x - lp1(&mut self.z_hi[ch], self.a_hi, x);
            let band = if pos < 0.0 { mid } else { hi };

            let del = self.dl[ch].read(d);
            let fbs = lp1(&mut self.z_fb[ch], self.a_fb, del);
            self.dl[ch].write(soft(band * send_gain + fbs * fb));
            out[ch] = x + del * wet;
        }
        (out[0], out[1])
    }
}

// ── Flanger ───────────────────────────────────────────────────────────────────
//
// Not a rekordbox Color FX (it is a Beat FX there), so this is our own mapping:
// left = slow sweep with NEGATIVE feedback (hollow, "through" sound), right =
// faster sweep with POSITIVE feedback (metallic jet). Centre = off.
// Click-free by construction: delay stays within 0.5–3.5 ms (never negative),
// Hermite fractional reads, per-sample smoothed rate/feedback/mix, tanh feedback.

const FLANGER_CENTRE_MS: f32 = 2.0;

pub struct FlangerEffect {
    dl: [DelayLine; 2],
    phase: f32,
    sr: u32,
    pub value: f32,
    pos: Smooth,
    rate: Smooth,
}

impl FlangerEffect {
    pub fn new(sr: u32) -> Self {
        let n = (sr as f32 * 0.010) as usize;
        Self {
            dl: [DelayLine::new(n), DelayLine::new(n)],
            phase: 0.0,
            sr,
            value: 0.5,
            pos: Smooth::new(0.0, sr, 0.012),
            rate: Smooth::new(0.1, sr, 0.25),
        }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            let v = self.value;
            *self = Self::new(sr);
            self.value = v;
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pos = self.pos.tick(knob_signed(self.value));
        let depth = pos.abs();
        let rate = self.rate.tick(0.10 + if pos < 0.0 { 0.55 * depth } else { 1.8 * depth });
        self.phase += rate / self.sr as f32;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        let amp_ms = 1.5 * (0.35 + 0.65 * depth);
        let fb = if pos < 0.0 { -0.55 * depth } else { 0.62 * depth };
        let mix = (depth * 2.5).min(1.0) * 0.85;
        let norm = 1.0 / (1.0 + 0.3 * mix);
        let ms = self.sr as f32 / 1000.0;

        let ins = [l, r];
        let mut out = [0.0_f32; 2];
        for ch in 0..2 {
            let x = ins[ch];
            let lfo = (2.0 * PI * (self.phase + 0.25 * ch as f32)).sin();
            let del = self.dl[ch].read((FLANGER_CENTRE_MS + amp_ms * lfo) * ms);
            self.dl[ch].write(soft(x + fb * del));
            out[ch] = (x + del * mix) * norm;
        }
        (out[0], out[1])
    }
}

// ── Reverb (rekordbox SPACE-style) ────────────────────────────────────────────
//
// Evidence: turn right → MID+HIGH bands go to the effect, turn left → MID+LOW.
// Centre = off. Freeverb-style 4 parallel damped combs + 2 allpasses per channel
// (mono send, different L/R lengths for width). Comb input is auto-normalised
// (0.35*sqrt(1-g²)) so the tail level is ~constant whatever the decay, and the
// wet gain is strong enough that it is clearly audible from small knob moves.

const REVERB_COMBS_L: [usize; 4] = [1116, 1356, 1422, 1617];
const REVERB_COMBS_R: [usize; 4] = [1139, 1381, 1447, 1642];
const REVERB_ALLPASS: [usize; 2] = [225, 341];

pub struct ReverbEffect {
    comb: [[Ring; 4]; 2],
    comb_z: [[f32; 4]; 2],
    ap: [[Ring; 2]; 2],
    sr: u32,
    pub value: f32,
    pos: Smooth,
    z_send_lp: f32,
    z_send_hp: f32,
    a_send_lp: f32,
    a_send_hp: f32,
}

impl ReverbEffect {
    pub fn new(sr: u32) -> Self {
        let sc = sr as f32 / 44100.0;
        let len = |n: usize| (n as f32 * sc) as usize;
        Self {
            comb: [
                std::array::from_fn(|i| Ring::new(len(REVERB_COMBS_L[i]))),
                std::array::from_fn(|i| Ring::new(len(REVERB_COMBS_R[i]))),
            ],
            comb_z: [[0.0; 4]; 2],
            ap: [
                std::array::from_fn(|i| Ring::new(len(REVERB_ALLPASS[i]))),
                std::array::from_fn(|i| Ring::new(len(REVERB_ALLPASS[i] + 23))),
            ],
            sr,
            value: 0.5,
            pos: Smooth::new(0.0, sr, 0.012),
            z_send_lp: 0.0,
            z_send_hp: 0.0,
            a_send_lp: onepole_coef(3500.0, sr),
            a_send_hp: onepole_coef(350.0, sr),
        }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            let v = self.value;
            *self = Self::new(sr);
            self.value = v;
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pos = self.pos.tick(knob_signed(self.value));
        let depth = pos.abs();
        let g = 0.80 + 0.13 * depth;
        let damp = 0.25_f32;
        let s_in = 0.35 * (1.0 - g * g).sqrt();

        let mono = 0.5 * (l + r);
        let lowband = lp1(&mut self.z_send_lp, self.a_send_lp, mono);
        let highband = mono - lp1(&mut self.z_send_hp, self.a_send_hp, mono);
        let send = if pos < 0.0 { lowband } else { highband };
        let inp = send * (depth * 6.0).min(1.0) * s_in;
        let wet = depth.powf(0.6) * 0.9;

        let ins = [l, r];
        let mut out = [0.0_f32; 2];
        for ch in 0..2 {
            let mut acc = 0.0_f32;
            for i in 0..4 {
                let o = self.comb[ch][i].cur();
                self.comb_z[ch][i] = flush(o * (1.0 - damp) + self.comb_z[ch][i] * damp);
                self.comb[ch][i].push(inp + self.comb_z[ch][i] * g);
                acc += o;
            }
            for i in 0..2 {
                let bo = self.ap[ch][i].cur();
                let y = -acc + bo;
                self.ap[ch][i].push(acc + bo * 0.5);
                acc = y;
            }
            out[ch] = ins[ch] + acc * wet;
        }
        (out[0], out[1])
    }
}

// ── Phaser ────────────────────────────────────────────────────────────────────
//
// Our own mapping (not a rekordbox Color FX). 6-stage allpass chain swept
// 180 Hz–5.5 kHz, feedback for resonance, and the wet mix reaches 50 % so the
// notches are fully deep (this is why the old 4-stage/0.45 version was almost
// inaudible). Left = slow, right = fast. L/R LFOs 0.2 cycle apart for width.

const PHASER_STAGES: usize = 6;

pub struct PhaserEffect {
    z: [[f32; PHASER_STAGES]; 2],
    fbk: [f32; 2],
    phase: f32,
    sr: u32,
    pub value: f32,
    pos: Smooth,
    rate: Smooth,
}

impl PhaserEffect {
    pub fn new(sr: u32) -> Self {
        Self {
            z: [[0.0; PHASER_STAGES]; 2],
            fbk: [0.0; 2],
            phase: 0.0,
            sr,
            value: 0.5,
            pos: Smooth::new(0.0, sr, 0.012),
            rate: Smooth::new(0.12, sr, 0.25),
        }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            let v = self.value;
            *self = Self::new(sr);
            self.value = v;
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pos = self.pos.tick(knob_signed(self.value));
        let depth = pos.abs();
        let rate = self.rate.tick(0.12 + if pos < 0.0 { 0.5 * depth } else { 1.7 * depth });
        self.phase += rate / self.sr as f32;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        let fbamt = 0.55 * depth;
        let m = 0.5 * (depth * 2.5).min(1.0);

        let ins = [l, r];
        let mut out = [0.0_f32; 2];
        for ch in 0..2 {
            let x = ins[ch];
            let lfo = 0.5 + 0.5 * (2.0 * PI * (self.phase + 0.2 * ch as f32)).sin();
            let fc = 180.0_f32 * (5500.0_f32 / 180.0).powf(lfo);
            let c = allpass_coeff(fc, self.sr);

            let mut v = x + soft(self.fbk[ch]) * fbamt;
            for st in 0..PHASER_STAGES {
                let y = -c * v + self.z[ch][st];
                self.z[ch][st] = flush(v + c * y);
                v = y;
            }
            self.fbk[ch] = flush(v);
            out[ch] = x * (1.0 - m) + v * m;
        }
        (out[0], out[1])
    }
}

// ── Bitcrusher (rekordbox CRUSH) ──────────────────────────────────────────────
//
// Evidence: either direction changes the RATE; turning right adds a high-pass
// after the crusher; PARAMETER sets bit reduction. With one knob:
//   left  : bit-depth 16→4.5 bits (the character the user loves) + mild rate cut (÷1→÷5)
//   right : rate reduction ÷1→÷16 + bits 16→9.5 + rising high-pass (60 Hz→2.3 kHz)
// Rate reduction uses a phase accumulator (fractional ratios, no integer jumps),
// all parameters are per-sample smoothed and the wet path is smoothstep-faded in.

pub struct BitcrusherEffect {
    acc: f32,
    hold: [f32; 2],
    hp1: [f32; 2],
    hp2: [f32; 2],
    sr: u32,
    pub value: f32,
    pos: Smooth,
    /// What the knob position works out to, kept until the position moves: five powers and an exponential a
    /// sample otherwise, for numbers that only change while the knob is turning.
    at: f32,
    set: CrushSet,
}

/// The crusher's settings at one knob position.
#[derive(Clone, Copy, Default)]
struct CrushSet {
    levels: f32,
    step: f32,
    hp_mix: f32,
    a_hp: f32,
    wm: f32,
}

impl BitcrusherEffect {
    pub fn new(sr: u32) -> Self {
        Self {
            acc: 0.0,
            hold: [0.0; 2],
            hp1: [0.0; 2],
            hp2: [0.0; 2],
            sr,
            value: 0.5,
            pos: Smooth::new(0.0, sr, 0.012),
            at: f32::NAN,
            set: CrushSet::default(),
        }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            let v = self.value;
            *self = Self::new(sr);
            self.value = v;
        }
    }

    fn settings(pos: f32, sr: u32) -> CrushSet {
        let l_amt = (-pos).max(0.0);
        let r_amt = pos.max(0.0);
        let bits = 16.0 - 11.5 * l_amt - 6.5 * r_amt;
        let ratediv = 1.0 + 4.0 * l_amt.powf(1.2) + 15.0 * r_amt.powf(1.3);
        CrushSet {
            levels: 2.0_f32.powf(bits - 1.0),
            step: 1.0 / ratediv,
            hp_mix: (r_amt * 6.0).min(1.0),
            a_hp: onepole_coef(60.0 + 2240.0 * r_amt.powf(1.5), sr),
            wm: smoothstep(pos.abs() * 5.0),
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pos = self.pos.tick(knob_signed(self.value));
        if pos != self.at {
            self.at = pos;
            self.set = Self::settings(pos, self.sr);
        }
        let CrushSet { levels, step, hp_mix, a_hp, wm } = self.set;

        let ins = [l, r];
        self.acc += step;
        let sample_now = self.acc >= 1.0;
        if sample_now {
            self.acc -= 1.0;
        }

        let mut out = [0.0_f32; 2];
        for ch in 0..2 {
            let x = ins[ch];
            if sample_now {
                self.hold[ch] = (x * levels).round() / levels;
            }
            let h = self.hold[ch];
            let y1 = h - lp1(&mut self.hp1[ch], a_hp, h);
            let y2 = y1 - lp1(&mut self.hp2[ch], a_hp, y1);
            let wet = h * (1.0 - hp_mix) + y2 * hp_mix;
            out[ch] = x * (1.0 - wm) + wet * wm;
        }
        (out[0], out[1])
    }
}

// ── Sweep (rekordbox SWEEP) ───────────────────────────────────────────────────
//
// Evidence (Pioneer DJM manual): turn LEFT → gate effect (tighter sound, reduced
// sense of volume); turn RIGHT → band-pass filter whose bandwidth steadily
// narrows, centre frequency rising with the knob. Centre = off.
//   right: "trapezoid" band-pass = 12 dB/oct HP into 12 dB/oct LP (two TPT SVFs,
//          mild resonance). Centre sweeps 400 Hz → 5 kHz exponentially while the
//          half-bandwidth narrows 3.2 → 0.55 octaves, with up to +5 dB makeup.
//   left : envelope-follower gate. Threshold is RELATIVE to a slow-tracking recent
//          peak (-40 dB → -4 dB below it) so it bites on any track loudness; gate
//          gain opens in ~1 ms and closes in ~40 ms (no clicks), squared ratio
//          below threshold for a tight chopped feel, plus up to -5 dB overall level
//          ("reduced sense of volume" per the manual).
// The SVFs and envelope run every sample so their state is always valid.

pub struct SweepEffect {
    sr: u32,
    pub value: f32,
    pos: Smooth,
    hp: Svf,
    lp: Svf,
    env: f32,
    peak: f32,
    gate: f32,
    a_att: f32,
    a_rel: f32,
    p_rel: f32,
    g_att: f32,
    g_rel: f32,
    /// What the knob position works out to, kept until the position moves (see [`BitcrusherEffect`]).
    at: f32,
    set: SweepSet,
}

/// The sweep's settings at one knob position.
#[derive(Clone, Copy, Default)]
struct SweepSet {
    g_lo: f32,
    g_hi: f32,
    makeup: f32,
    m: f32,
    /// The gate's threshold as a share of the recent peak.
    thr: f32,
    gm: f32,
    /// The level left when the gate side is turned up.
    trim: f32,
}

impl SweepEffect {
    pub fn new(sr: u32) -> Self {
        Self {
            sr,
            value: 0.5,
            pos: Smooth::new(0.0, sr, 0.012),
            hp: Svf::default(),
            lp: Svf::default(),
            env: 0.0,
            peak: 0.0,
            gate: 1.0,
            a_att: smooth_coef(sr, 0.001),
            a_rel: smooth_coef(sr, 0.080),
            p_rel: smooth_coef(sr, 1.5),
            g_att: smooth_coef(sr, 0.001),
            g_rel: smooth_coef(sr, 0.040),
            at: f32::NAN,
            set: SweepSet::default(),
        }
    }

    fn settings(pos: f32, sr: f32) -> SweepSet {
        let r_amt = pos.max(0.0);
        let l_amt = (-pos).max(0.0);
        let fc = 400.0_f32 * (5000.0_f32 / 400.0).powf(r_amt);
        let bw = 3.2 - 2.65 * r_amt.powf(0.8);
        let f_lo = (fc / 2.0_f32.powf(bw)).max(20.0);
        let f_hi = (fc * 2.0_f32.powf(bw)).min(0.45 * sr);
        SweepSet {
            g_lo: (PI * f_lo / sr).tan(),
            g_hi: (PI * f_hi / sr).tan(),
            makeup: 10.0_f32.powf(5.0 * r_amt / 20.0),
            m: smoothstep(r_amt / 0.4),
            thr: 10.0_f32.powf((-40.0 + 36.0 * l_amt) / 20.0),
            gm: smoothstep(l_amt / 0.15),
            trim: 1.0 - 0.45 * l_amt,
        }
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = v.clamp(0.0, 1.0);
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        if sr != self.sr {
            let v = self.value;
            *self = Self::new(sr);
            self.value = v;
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let pos = self.pos.tick(knob_signed(self.value));
        if pos != self.at {
            self.at = pos;
            self.set = Self::settings(pos, self.sr as f32);
        }
        // band-pass (right): g_lo, g_hi, makeup and its mix m; gate (left): thr, its mix gm and trim
        let SweepSet { g_lo, g_hi, makeup, m, thr, gm, trim } = self.set;
        let k = 1.2_f32;

        // ── gate (left) ───────────────────────────────────────────────────
        let lvl = l.abs().max(r.abs());
        let a = if lvl > self.env { self.a_att } else { self.a_rel };
        self.env = flush(self.env + (lvl - self.env) * a);
        // slow recent-peak reference: fast up, 1.5 s down
        let pa = if lvl > self.peak { self.a_att } else { self.p_rel };
        self.peak = flush(self.peak + (lvl - self.peak) * pa);
        let thr = (self.peak * thr).max(1e-6);
        let ratio = (self.env / thr).min(1.0);
        let tgt = ratio * ratio;
        let ga = if tgt > self.gate { self.g_att } else { self.g_rel };
        self.gate += (tgt - self.gate) * ga;
        let gate_gain = (1.0 - gm * (1.0 - self.gate)) * trim;

        let ins = [l, r];
        let mut out = [0.0_f32; 2];
        for ch in 0..2 {
            let x = ins[ch];
            let (_, _, high) = self.hp.tick(ch, x, g_lo, k);
            let (low, _, _) = self.lp.tick(ch, high, g_hi, k);
            let bp = low * makeup;
            out[ch] = (x * (1.0 - m) + bp * m) * gate_gain;
        }
        (out[0], out[1])
    }
}

// ── Channel effect dispatcher ─────────────────────────────────────────────────
//
// Mode IDs (index into MODE_LABELS in app.rs; ids are stable, display order is
// separate):
//   0 = LP/HP Filter   1 = Echo   2 = Flanger   3 = Reverb
//   4 = Phaser         5 = Bitcrusher           6 = Sweep

pub enum DeckEffect {
    LpHp(FilterKnob),
    Echo(EchoEffect),
    Flanger(FlangerEffect),
    Reverb(ReverbEffect),
    Phaser(PhaserEffect),
    Bitcrusher(BitcrusherEffect),
    Sweep(SweepEffect),
}

impl DeckEffect {
    pub fn new(mode: u8, sr: u32) -> Self {
        match mode {
            1 => Self::Echo(EchoEffect::new(sr)),
            2 => Self::Flanger(FlangerEffect::new(sr)),
            3 => Self::Reverb(ReverbEffect::new(sr)),
            4 => Self::Phaser(PhaserEffect::new(sr)),
            5 => Self::Bitcrusher(BitcrusherEffect::new(sr)),
            6 => Self::Sweep(SweepEffect::new(sr)),
            _ => Self::LpHp(FilterKnob::new(sr)),
        }
    }

    pub fn mode_id(&self) -> u8 {
        match self {
            Self::LpHp(_) => 0,
            Self::Echo(_) => 1,
            Self::Flanger(_) => 2,
            Self::Reverb(_) => 3,
            Self::Phaser(_) => 4,
            Self::Bitcrusher(_) => 5,
            Self::Sweep(_) => 6,
        }
    }

    pub fn current_value(&self) -> f32 {
        match self {
            Self::LpHp(f) => f.value,
            Self::Echo(e) => e.value,
            Self::Flanger(f) => f.value,
            Self::Reverb(r) => r.value,
            Self::Phaser(p) => p.value,
            Self::Bitcrusher(b) => b.value,
            Self::Sweep(s) => s.value,
        }
    }

    pub fn set_value(&mut self, v: f32) {
        match self {
            Self::LpHp(f) => f.set_value(v),
            Self::Echo(e) => e.set_value(v),
            Self::Flanger(f) => f.set_value(v),
            Self::Reverb(r) => r.set_value(v),
            Self::Phaser(p) => p.set_value(v),
            Self::Bitcrusher(b) => b.set_value(v),
            Self::Sweep(s) => s.set_value(v),
        }
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        match self {
            Self::LpHp(f) => f.set_sample_rate(sr),
            Self::Echo(e) => e.set_sample_rate(sr),
            Self::Flanger(f) => f.set_sample_rate(sr),
            Self::Reverb(r) => r.set_sample_rate(sr),
            Self::Phaser(p) => p.set_sample_rate(sr),
            Self::Bitcrusher(b) => b.set_sample_rate(sr),
            Self::Sweep(s) => s.set_sample_rate(sr),
        }
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        match self {
            Self::LpHp(fx) => fx.process(l, r),
            Self::Echo(fx) => fx.process(l, r),
            Self::Flanger(fx) => fx.process(l, r),
            Self::Reverb(fx) => fx.process(l, r),
            Self::Phaser(fx) => fx.process(l, r),
            Self::Bitcrusher(fx) => fx.process(l, r),
            Self::Sweep(fx) => fx.process(l, r),
        }
    }

    /// 0.5 for non-filter effects so the waveform shows no frequency dimming.
    pub fn value_for_waveform(&self) -> f32 {
        match self {
            Self::LpHp(f) => f.value,
            _ => 0.5,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44100;

    fn noise(n: usize, amp: f32) -> Vec<f32> {
        let mut s: u32 = 0x1234_5678;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn sine(n: usize, f: f32, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / SR as f32).sin()).collect()
    }

    fn run(mode: u8, value: f32, x: &[f32]) -> Vec<f32> {
        let mut fx = DeckEffect::new(mode, SR);
        fx.set_value(value);
        x.iter().map(|&s| fx.process(s, s).0).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn max_step(x: &[f32]) -> f32 {
        x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn turning_an_eq_knob_does_not_click() {
        // a low sine through the EQ while the bass knob is turned from flat to cut in ten jumps, as knob messages
        // arrive: the output must never step by more than a sine of that level can
        let x = sine(SR as usize / 2, 80.0, 0.5);
        let mut eq = DeckEq::new(SR);
        let mut y = Vec::with_capacity(x.len());
        for (i, &v) in x.iter().enumerate() {
            if i % 700 == 0 {
                eq.set_lo(1.0 - (i / 700).min(10) as f32 * 0.1);
            }
            y.push(eq.process(v, v).0);
        }
        let dry_step = max_step(&x);
        assert!(max_step(&y) <= dry_step * 1.5, "step {} against a dry {}", max_step(&y), dry_step);
        // and the cut arrives: the last tenth of a second is far quieter than the first
        let n = SR as usize / 10;
        assert!(rms(&y[y.len() - n..]) < 0.2 * rms(&y[..n]), "the bass is cut");
    }

    #[test]
    fn the_eq_gets_to_where_the_knob_is_and_then_stops_working_at_it() {
        let mut eq = DeckEq::new(SR);
        eq.set_hi(0.0);
        for _ in 0..SR / 10 {
            eq.process(0.1, 0.1);
        }
        assert_eq!(eq.cur, eq.gains, "settled within a tenth of a second");
    }

    #[test]
    fn centre_is_bit_exact_bypass() {
        let x = noise(SR as usize / 2, 0.5);
        for mode in 0..=6u8 {
            let y = run(mode, 0.5, &x);
            assert_eq!(x, y, "mode {mode} not bit-exact at centre");
            // inside the dead zone too
            let y = run(mode, 0.5 + EFFECT_DEAD * 0.9, &x);
            assert_eq!(x, y, "mode {mode} not bit-exact inside dead zone");
        }
    }

    #[test]
    fn effects_are_clearly_audible_both_directions() {
        let x = noise(SR as usize * 2, 0.3);
        for mode in 1..=6u8 {
            for &v in &[0.12_f32, 0.88] {
                let y = run(mode, v, &x);
                let diff: Vec<f32> = x.iter().zip(&y).map(|(a, b)| b - a).collect();
                let ratio = rms(&diff[SR as usize..]) / rms(&x[SR as usize..]);
                assert!(ratio > 0.2, "mode {mode} value {v}: effect too subtle ({ratio:.3})");
                assert!(y.iter().all(|s| s.is_finite() && s.abs() < 6.0), "mode {mode} value {v}: unstable");
            }
        }
    }

    /// Sweeping the knob quickly must not introduce a step larger than the
    /// effect itself produces at a fixed setting (i.e. no knob-induced clicks).
    #[test]
    fn knob_sweeps_do_not_click() {
        let n = SR as usize * 2;
        let x = sine(n, 440.0, 0.5);
        // Bitcrusher has inherent quantisation steps, covered by the bounded check below.
        for mode in [1u8, 2, 3, 4, 6] {
            let mut worst_static = 0.0_f32;
            for &v in &[0.0_f32, 0.2, 0.5, 0.8, 1.0] {
                worst_static = worst_static.max(max_step(&run(mode, v, &x)[SR as usize / 2..]));
            }
            let mut fx = DeckEffect::new(mode, SR);
            let mut y = Vec::with_capacity(n);
            for (i, &s) in x.iter().enumerate() {
                // fast triangle sweep 0 → 1 → 0 several times, crossing centre
                let ph = (i as f32 / (SR as f32 * 0.25)) % 2.0;
                fx.set_value(if ph < 1.0 { ph } else { 2.0 - ph });
                y.push(fx.process(s, s).0);
            }
            let sweep = max_step(&y[SR as usize / 2..]);
            assert!(
                sweep <= worst_static * 1.3 + 0.01,
                "mode {mode}: sweep step {sweep:.3} vs static worst {worst_static:.3}"
            );
        }
        for mode in [5u8] {
            let mut fx = DeckEffect::new(mode, SR);
            for (i, &s) in x.iter().enumerate() {
                let ph = (i as f32 / (SR as f32 * 0.25)) % 2.0;
                fx.set_value(if ph < 1.0 { ph } else { 2.0 - ph });
                let (a, b) = fx.process(s, s);
                assert!(a.is_finite() && b.is_finite() && a.abs() <= 1.5);
            }
        }
    }

    #[test]
    fn flanger_delay_never_negative_or_clicky() {
        // Regression: old flanger swung the delay to -3 ms → extrapolated reads → cracks.
        let x = sine(SR as usize * 2, 3000.0, 0.5);
        let in_step = max_step(&x);
        let y = run(2, 1.0, &x);
        assert!(max_step(&y[SR as usize / 2..]) < in_step * 3.5);
        let y = run(2, 0.0, &x);
        assert!(max_step(&y[SR as usize / 2..]) < in_step * 3.5);
    }

    #[test]
    fn sweep_right_is_a_narrow_bandpass() {
        let n = SR as usize * 2;
        let low = run(6, 1.0, &sine(n, 50.0, 0.5));
        let mid = run(6, 1.0, &sine(n, 4000.0, 0.5));
        let (rl, rm) = (rms(&low[SR as usize..]), rms(&mid[SR as usize..]));
        assert!(rm > 0.25, "band centre should pass, got {rm}");
        assert!(rl < rm * 0.1, "50 Hz should be cut >20 dB vs band centre ({rl} vs {rm})");
    }

    #[test]
    fn sweep_left_gates_valleys_and_keeps_peaks() {
        // 100 ms loud bursts separated by 400 ms of quiet material.
        let blk = SR as usize / 10;
        let loud = noise(blk, 0.8);
        let quiet = noise(blk * 4, 0.04);
        let mut x = Vec::new();
        for _ in 0..6 {
            x.extend_from_slice(&loud);
            x.extend_from_slice(&quiet);
        }
        let y = run(6, 0.0, &x);
        // last cycle, after the peak tracker has settled
        let cyc = blk * 5;
        let base = cyc * 5;
        let (lo_in, lo_out) = (&x[base..base + blk], &y[base..base + blk]);
        let (q_in, q_out) = (&x[base + blk * 2..base + blk * 5], &y[base + blk * 2..base + blk * 5]);
        assert!(rms(q_out) < rms(q_in) * 0.2, "gate should cut valleys >14 dB ({} vs {})", rms(q_out), rms(q_in));
        assert!(rms(lo_out) > rms(lo_in) * 0.4, "gate should pass the loud hits");
    }

    #[test]
    fn reverb_has_a_tail() {
        let mut fx = DeckEffect::new(3, SR);
        fx.set_value(0.9);
        // let the knob settle, then a short burst followed by silence
        for _ in 0..SR / 2 {
            fx.process(0.0, 0.0);
        }
        for _ in 0..200 {
            fx.process(0.5, 0.5);
        }
        let tail: Vec<f32> = (0..SR / 2).map(|_| fx.process(0.0, 0.0).0).collect();
        assert!(rms(&tail[SR as usize / 10..]) > 1e-3, "no reverb tail after burst");
    }

    #[test]
    fn echo_repeats_at_expected_delay() {
        let mut fx = DeckEffect::new(1, SR);
        fx.set_value(1.0); // right: highs echo
        for _ in 0..SR / 2 {
            fx.process(0.0, 0.0);
        }
        let mut out = Vec::new();
        // broadband click (contains highs)
        for i in 0..SR {
            out.push(fx.process(if i == 0 { 1.0 } else { 0.0 }, 0.0).0);
        }
        let after = &out[(SR as f32 * 0.15) as usize..];
        let (peak_i, peak) =
            after.iter().enumerate().fold((0, 0.0_f32), |a, (i, v)| if v.abs() > a.1 { (i, v.abs()) } else { a });
        assert!(peak > 0.02, "no audible echo repeat (peak {peak})");
        let secs = (peak_i as f32 + SR as f32 * 0.15) / SR as f32;
        assert!(secs > 0.2 && secs < 0.5, "first repeat at unexpected time {secs}");
    }
}
