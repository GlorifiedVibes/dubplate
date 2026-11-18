//! Key lock (also called Master Tempo): plays a track at another tempo without the pitch moving with it.
//!
//! The deck plays by moving a playhead through the song. With key lock on and the deck at a steady forward tempo,
//! the audio instead goes through a time stretcher (Signalsmith Stretch, MIT) that is fed whatever part of the song
//! the playhead is about to reach. The stretcher answers later than it is asked, by an amount that depends on the
//! tempo, so it is fed that much further ahead and the sound that comes out lines up with the playhead: cue points,
//! loops and beat grids stay where they are. What comes out was fed `lin + lout * tempo` source frames ago (the
//! stretcher's input latency, plus its output latency at the speed it is being played), and a test checks that a
//! click in the song is heard where the playhead says it is, at several tempos and sample rates.

use signalsmith_stretch::Stretch;

/// Frames the stretcher is asked for at a time.
pub const BLOCK: usize = 256;

pub struct KeyLock {
    st: Stretch,
    lin: usize,
    lout: usize,
    out: Vec<f32>,
    out_at: usize,
    inbuf: Vec<f32>,
    /// Source-frame position of the next frame to feed.
    pos: f64,
    /// The playhead the next block should start at if play carries on undisturbed.
    expect: f64,
}

impl KeyLock {
    pub fn new(sample_rate: u32) -> Self {
        let st = Stretch::preset_default(2, sample_rate);
        let (lin, lout) = (st.input_latency(), st.output_latency());
        KeyLock {
            st,
            lin,
            lout,
            out: vec![0.0; BLOCK * 2],
            out_at: BLOCK,
            inbuf: vec![0.0; (BLOCK * 8 + lin * 2 + 16) * 2],
            pos: 0.0,
            expect: f64::NAN,
        }
    }

    /// Transposes everything by `semitones` (key shift). Time and pitch are independent in the stretcher, so the
    /// timing the rest of this file maintains is unaffected.
    pub fn set_shift(&mut self, semitones: f32) {
        self.st.set_transpose_factor_semitones(semitones, None);
    }

    /// How far ahead of the playhead the stretcher must be fed, in its own frames, at this tempo.
    fn lead(&self, tempo: f64) -> f64 {
        self.lin as f64 + self.lout as f64 * tempo
    }

    /// Starts again with the playhead at `heard` (source frames). `rs` = source frames per output frame at normal
    /// speed, `tempo` the playback speed, `read` reads one source frame.
    pub fn engage(&mut self, heard: f64, tempo: f64, rs: f64, read: &mut dyn FnMut(f64) -> (f32, f32)) {
        self.st.reset();
        let start = heard + self.lead(tempo) * rs;
        let pre = self.lin;
        for i in 0..pre {
            let (l, r) = read(start - (pre - i) as f64 * rs);
            self.inbuf[i * 2] = l;
            self.inbuf[i * 2 + 1] = r;
        }
        self.st.seek(&self.inbuf[..pre * 2], tempo);
        self.pos = start;
        self.out_at = BLOCK;
        self.expect = heard;
    }

    /// True when the playhead is not where undisturbed play would have put it (a jump, a loop wrap): `engage` again.
    pub fn lost(&self, heard: f64, per_frame: f64) -> bool {
        self.out_at >= BLOCK
            && self.expect.is_finite()
            && (heard - self.expect).abs() > (BLOCK as f64 * per_frame.abs()).max(1.0) * 2.0 + 64.0
    }

    /// The next output frame. `heard` is the playhead (source frames), `per_frame` source frames it moves per output
    /// frame, `tempo` the playback speed, `rs` as for `engage`.
    pub fn next(
        &mut self,
        heard: f64,
        per_frame: f64,
        tempo: f64,
        rs: f64,
        read: &mut dyn FnMut(f64) -> (f32, f32),
    ) -> (f32, f32) {
        if self.out_at >= BLOCK {
            let target = heard + BLOCK as f64 * per_frame + self.lead(tempo) * rs;
            let n = (((target - self.pos) / rs).round().max(1.0) as usize).min(BLOCK * 8);
            for i in 0..n {
                let (l, r) = read(self.pos + i as f64 * rs);
                self.inbuf[i * 2] = l;
                self.inbuf[i * 2 + 1] = r;
            }
            self.st.process(&self.inbuf[..n * 2], &mut self.out);
            self.pos += n as f64 * rs;
            self.out_at = 0;
            self.expect = heard + BLOCK as f64 * per_frame;
        }
        let f = (self.out[self.out_at * 2], self.out[self.out_at * 2 + 1]);
        self.out_at += 1;
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICK: usize = 64;

    /// A song of silence with a short click (a Hann-windowed 2 kHz tone, 64 frames) every `period` frames.
    fn burst_at(src_rate: f64, period: usize) -> impl Fn(f64) -> (f32, f32) {
        move |pos: f64| {
            if pos < 0.0 {
                return (0.0, 0.0);
            }
            let p = pos as usize % period;
            if p < CLICK {
                let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * p as f64 / CLICK as f64).cos();
                let v = ((2.0 * std::f64::consts::PI * 2000.0 * p as f64 / src_rate).sin() * w) as f32;
                (v, v)
            } else {
                (0.0, 0.0)
            }
        }
    }

    /// Plays `frames` output frames from `start` at `tempo`; returns the output.
    fn play(rate: u32, src_rate: f64, tempo: f64, start: f64, frames: usize, period: usize) -> Vec<f32> {
        let rs = src_rate / rate as f64;
        let song = burst_at(src_rate, period);
        let mut read = |p: f64| song(p);
        let mut kl = KeyLock::new(rate);
        kl.engage(start, tempo, rs, &mut read);
        let mut heard = start;
        let per = tempo * rs;
        let mut out = Vec::with_capacity(frames);
        for _ in 0..frames {
            let (l, _) = kl.next(heard, per, tempo, rs, &mut read);
            out.push(l);
            heard += per;
        }
        out
    }

    /// For each click heard in `out` (the stretcher smears it over a few ms, so its centre of energy is used):
    /// where the playhead said the song was minus where the click really is, in source frames.
    fn click_errors(out: &[f32], period: usize, start: f64, tempo: f64, rs: f64) -> Vec<f64> {
        let mut errs = Vec::new();
        let mut i = 0;
        while i < out.len() {
            if out[i].abs() > 0.01 {
                let mut end = i;
                let mut quiet = 0;
                while end < out.len() && quiet < 2000 {
                    if out[end].abs() > 0.01 {
                        quiet = 0
                    } else {
                        quiet += 1
                    }
                    end += 1;
                }
                let (mut e, mut m) = (0f64, 0f64);
                for j in i..end {
                    let x = (out[j] as f64).powi(2);
                    e += x;
                    m += x * j as f64;
                }
                let heard = start + (m / e) * tempo * rs;
                let centre = heard - CLICK as f64 / 2.0 * 0.0; // the click's own centre is CLICK/2 after its start
                let k = ((centre - CLICK as f64 / 2.0) / period as f64).round();
                errs.push(centre - (k * period as f64 + CLICK as f64 / 2.0));
                i = end;
            } else {
                i += 1;
            }
        }
        errs
    }

    #[test]
    fn sound_is_heard_where_the_playhead_says_it_is() {
        let mut bad: Vec<String> = Vec::new();
        for &(rate, src) in &[(44100u32, 44100.0f64), (48000, 48000.0), (48000, 44100.0)] {
            for &tempo in &[0.92f64, 0.97, 1.03, 1.08, 1.16] {
                let period = 7000;
                let start = 70_321.0;
                let out = play(rate, src, tempo, start, 60_000, period);
                let errs = click_errors(
                    &out[3000..],
                    period,
                    start + 3000.0 * tempo * src / rate as f64,
                    tempo,
                    src / rate as f64,
                );
                eprintln!(
                    "rate {rate} src {src} tempo {tempo}: errs {:?}",
                    errs.iter().map(|e| e.round() as i64).collect::<Vec<_>>()
                );
                assert!(errs.len() >= 4, "{rate} {src} {tempo}: only {} bursts heard", errs.len());
                for e in &errs {
                    // 1 ms is 44 frames: a loop or a cue lands within a millisecond of where the grid says
                    if e.abs() >= 44.0 * src / 44100.0 {
                        bad.push(format!("rate {rate} src {src} tempo {tempo}: {e:.0}"));
                        break;
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn the_pitch_does_not_move() {
        // A steady 1 kHz tone at 1.12x: zero crossings should stay about a millisecond apart.
        let rate = 44100u32;
        let tone = |p: f64| {
            let v = (2.0 * std::f64::consts::PI * 1000.0 * p / rate as f64).sin() as f32 * 0.5;
            (v, v)
        };
        let mut read = |p: f64| tone(p);
        let tempo = 1.12;
        let mut kl = KeyLock::new(rate);
        kl.engage(100_000.0, tempo, 1.0, &mut read);
        let mut heard = 100_000.0;
        let mut out = Vec::new();
        for _ in 0..44100 {
            out.push(kl.next(heard, tempo, tempo, 1.0, &mut read).0);
            heard += tempo;
        }
        let tail = &out[8000..];
        let crossings = tail.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let hz = crossings as f64 / (tail.len() as f64 / rate as f64);
        assert!((hz - 1000.0).abs() < 15.0, "{hz} Hz");
    }

    #[test]
    fn a_jump_is_noticed() {
        let rate = 44100u32;
        let song = burst_at(44100.0, 7000);
        let mut read = |p: f64| song(p);
        let mut kl = KeyLock::new(rate);
        kl.engage(10_000.0, 1.05, 1.0, &mut read);
        let mut heard = 10_000.0;
        for _ in 0..1000 {
            kl.next(heard, 1.05, 1.05, 1.0, &mut read);
            heard += 1.05;
        }
        // run to a block boundary
        while kl.out_at < BLOCK {
            kl.next(heard, 1.05, 1.05, 1.0, &mut read);
            heard += 1.05;
        }
        assert!(!kl.lost(heard, 1.05));
        assert!(kl.lost(heard + 30_000.0, 1.05));
        assert!(kl.lost(heard - 20_000.0, 1.05));
    }
}
