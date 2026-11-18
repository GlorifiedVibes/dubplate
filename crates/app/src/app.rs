use std::sync::{
    atomic::{AtomicU32, Ordering},
    mpsc, Arc,
};

use dubplate_engine::{
    create_deck_pair, decode_file, default_recording_path, is_stem_engine_available, new_sink, start_recording,
    AudioOutput, DeckCommand, DeckHandle, DeckShared, DecodedTrack, RecordSession, RecordSink, TrackStems,
};
use egui::Color32;

pub(crate) mod cue_loop;
mod learn;
mod prefs;
use cue_loop::CueState;
pub use cue_loop::DeckAction;

use crate::db::LibraryDb;
use crate::deck_view::{DeckView, GridEdit};
use crate::library::{DeckRef, Library, LibraryAction};
use crate::midi::MidiHub;
use crate::midi_mapper::AppCommand;
use crate::prepare::{PrepStatus, Prepare};
use crate::widgets::{filter_knob, horizontal_fader, knob, vertical_fader};

pub use crate::theme::palette;

const KNOB_R_MIXER: f32 = 12.0;
/// Tall enough for the mixer: its knobs, the crossfader and the curve / MASTER ▶ HP row under it.
const DECK_ROW_H: f32 = 346.0;
const MENU_BAR_H: f32 = 38.0;
const MIXER_W: f32 = 290.0;
/// A stopped, synced deck starts on the same position within this many beats
/// (one phrase) as the master's playhead.
const SYNC_QUANTUM_BEATS: f64 = 16.0;

/// Picks the whole-BPM tempo a follower should play at to match `want`, trying
/// x2 and x0.5 when the plain value is outside `[lo, hi]`.
/// Returns `(target, limited)` where `limited` means no candidate fits the range.
fn pick_sync_tempo(want: f32, lo: f32, hi: f32, nominal: f32) -> (f32, bool) {
    let dist = |c: f32| {
        if c < lo {
            lo - c
        } else if c > hi {
            c - hi
        } else {
            0.0
        }
    };
    let mut best = want.round();
    for c in [want * 2.0, want * 0.5] {
        let c = c.round();
        let better = dist(c) < dist(best) || (dist(c) == dist(best) && (c - nominal).abs() < (best - nominal).abs());
        if better {
            best = c;
        }
    }
    (best.clamp(lo, hi), dist(best) > 0.0)
}

/// Frames to wait before starting a stopped deck so that it begins at the same
/// position within a `quantum`-beat phrase as the master. `None` = start now.
/// `deck_beat` / `master_beat` are fractional beat positions on each deck's own grid.
fn phrase_start_delay_frames(
    deck_beat: f64,
    master_beat: f64,
    quantum: f64,
    master_bpm: f64,
    output_rate: f64,
) -> Option<u64> {
    let delta = (deck_beat - master_beat).rem_euclid(quantum);
    if delta <= 0.03 || delta >= quantum - 0.03 {
        return None;
    }
    Some((delta * 60.0 / master_bpm * output_rate) as u64)
}

/// A picture of the Prepare screen taken by the app itself, for checking a layout change without a person at the
/// mouse: `DUBPLATE_SHOT_TRACK=song.ogg DUBPLATE_SHOT_OUT=shot.ppm dubplate` opens Prepare on the track, waits for it
/// to settle, writes the window as a PPM image and quits. `DUBPLATE_SHOT_AT` (seconds) moves the playhead,
/// `DUBPLATE_SHOT_TAB=grid` shows the Grid tools, `DUBPLATE_SHOT_DEMO=1` puts cues and a loop on it.
/// `DUBPLATE_SHOT_DECK=1` puts the track on deck A and plays it with the library showing instead, for four seconds.
/// `DUBPLATE_SHOT_PREFS=1` has the Preferences window open too.
/// Either way the time the screen's own code took per frame is logged (`frame cpu`), which is how a change that is
/// meant to make drawing cheaper is checked.
struct Shot {
    track: std::path::PathBuf,
    out: std::path::PathBuf,
    at: f64,
    grid_tab: bool,
    demo: bool,
    deck: bool,
    prefs: bool,
    /// Frames drawn since Prepare opened; `None` before it is asked for.
    frames: Option<u32>,
    /// Seconds of processor time each frame took, once things have settled.
    cpu: Vec<f32>,
}

impl Shot {
    fn from_env() -> Option<Shot> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Some(Shot {
            track: var("DUBPLATE_SHOT_TRACK")?.into(),
            out: var("DUBPLATE_SHOT_OUT")?.into(),
            at: var("DUBPLATE_SHOT_AT").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            grid_tab: var("DUBPLATE_SHOT_TAB").as_deref() == Some("grid"),
            demo: var("DUBPLATE_SHOT_DEMO").is_some(),
            deck: var("DUBPLATE_SHOT_DECK").is_some(),
            prefs: var("DUBPLATE_SHOT_PREFS").is_some(),
            frames: None,
            cpu: Vec::new(),
        })
    }
}

/// Tracks being read for the decks, and what went wrong with the last one that failed: written by the loading threads,
/// read by the screen.
#[derive(Default)]
struct Loads {
    /// Per deck: the names of the tracks on their way (a second load can start before the first lands).
    coming: [Vec<String>; 2],
    failed: Option<String>,
}

/// Reads a track for a deck (or for Prepare) on the calling thread and hands it to the app, keeping `loads` up to date.
fn decode_for(
    slot: usize,
    path: std::path::PathBuf,
    tx: mpsc::SyncSender<(usize, std::path::PathBuf, Arc<DecodedTrack>)>,
    loads: Arc<std::sync::Mutex<Loads>>,
) {
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if slot < 2 {
        loads.lock().unwrap().coming[slot].push(name.clone());
    }
    log::info!("decoding [{slot}]: {}", path.display());
    let result = decode_file(&path);
    {
        let mut l = loads.lock().unwrap();
        if slot < 2 {
            if let Some(i) = l.coming[slot].iter().position(|n| *n == name) {
                l.coming[slot].remove(i);
            }
        }
        if let Err(e) = &result {
            log::error!("decode error: {e}");
            l.failed = Some(format!("Could not load {name}: {e}"));
        }
    }
    if let Ok(track) = result {
        let _ = tx.send((slot, path, Arc::new(track)));
    }
}

pub struct DubplateApp {
    /// The banner is saying something is under way, not that something was refused.
    flash_is_news: bool,
    loads: Arc<std::sync::Mutex<Loads>>,
    shot: Option<Shot>,
    audio_priority_logged: bool,
    decks: [DeckHandle; 2],
    deck_views: [DeckView; 2],
    crossfader: Arc<AtomicU32>,
    crossfader_curve: Arc<AtomicU32>,
    _audio: AudioOutput,
    audio_settings: crate::audio_settings::AudioSettings,
    monitor: Arc<dubplate_engine::MonitorControls>,

    decode_tx: mpsc::SyncSender<(usize, std::path::PathBuf, Arc<DecodedTrack>)>,
    decode_rx: mpsc::Receiver<(usize, std::path::PathBuf, Arc<DecodedTrack>)>,

    midi: MidiHub,
    midi_rx: mpsc::Receiver<AppCommand>,
    prefs: prefs::PrefsState,
    #[cfg(feature = "vst3-host")]
    master_fx: crate::master_fx::MasterFx,
    /// The effect on each deck's channel.
    #[cfg(feature = "vst3-host")]
    deck_fx: [crate::master_fx::MasterFx; 2],

    eq: [[f32; 3]; 2],
    pregain: [f32; 2],
    /// Auto gain: bring each track to the same loudness. `trim_db` is what it is applying now.
    auto_gain: [bool; 2],
    /// Key lock (Master Tempo) per deck.
    key_lock: [bool; 2],
    /// Key shift per deck in semitones.
    key_shift: [i32; 2],
    trim_db: [f32; 2],
    filter: [f32; 2],
    filter_mode: [u8; 2],

    // Stem separation
    stem_tx: mpsc::SyncSender<(usize, Result<Arc<TrackStems>, String>)>,
    stem_rx: mpsc::Receiver<(usize, Result<Arc<TrackStems>, String>)>,
    stem_analyzing: [bool; 2],
    stem_gains: [[f32; 4]; 2],
    /// Waveform summaries of each deck's stems, for the lanes; built in the background once the stems are there.
    stem_waves: [Option<Arc<dubplate_engine::StemWaves>>; 2],
    /// The stems each deck has (the same Arcs the decks hold), kept so the lanes' summaries can be made when wanted.
    deck_stems: [Option<Arc<TrackStems>>; 2],
    waves_building: [bool; 2],
    wave_tx: mpsc::Sender<(usize, std::path::PathBuf, Arc<dubplate_engine::StemWaves>)>,
    wave_rx: mpsc::Receiver<(usize, std::path::PathBuf, Arc<dubplate_engine::StemWaves>)>,
    stem_progress: [Arc<std::sync::atomic::AtomicU32>; 2],
    stem_available: bool,
    load_gen: [Arc<AtomicU32>; 2],

    // SYNC: deck `d` follows the other deck's tempo and beat while `sync_on[d]`.
    sync_on: [bool; 2],
    sync_limited: [bool; 2],
    sync_last_master: [f32; 2],
    sync_err_ema: [f64; 2],
    sync_nudge: [f32; 2],
    cue_state: [CueState; 2],

    load_flash_timer: f32,
    load_flash_msg: String,

    /// Output devices and the most channels each can open.
    audio_devices: Vec<(String, u16)>,
    selected_device: String,
    /// Set when the last output change failed.
    audio_message: Option<String>,

    // Session recording
    record_sink: RecordSink,
    record_session: Option<RecordSession>,
    record_start: Option<std::time::Instant>,

    library: Library,
    db: Option<LibraryDb>,

    /// The Prepare screen's own player, and the screen when it is open.
    prep: DeckHandle,
    prepare: Option<Prepare>,
    /// Whether a deck was playing when the prepare player last started (it is then heard in the headphones only).
    prep_started_over_decks: bool,
}

/// Index the decode thread uses for a track going to the Prepare screen instead of a deck.
const PREP_SLOT: usize = 2;

/// Starts the audio pipeline with fresh decks for the saved device choices.
fn open_audio(
    settings: &crate::audio_settings::AudioSettings,
    crossfader: &Arc<AtomicU32>,
    crossfader_curve: &Arc<AtomicU32>,
    monitor: &Arc<dubplate_engine::MonitorControls>,
    record_sink: &RecordSink,
) -> Result<(AudioOutput, [DeckHandle; 2], DeckHandle), String> {
    let (handle_a, player_a) = create_deck_pair(Arc::new(DeckShared::default()));
    let (handle_b, player_b) = create_deck_pair(Arc::new(DeckShared::default()));
    let (handle_p, player_p) = create_deck_pair(Arc::new(DeckShared::default()));
    dubplate_engine::start_audio_with_prep(
        player_a,
        player_b,
        Some(player_p),
        Arc::clone(crossfader),
        Arc::clone(crossfader_curve),
        settings.output.as_deref(),
        &settings.monitor.target(),
        Arc::clone(monitor),
        Arc::clone(record_sink),
        Vec::new(),
    )
    .map(|a| (a, [handle_a, handle_b], handle_p))
    .map_err(|e| e.to_string())
}

/// Which of two decks, given `(loaded, playing)` for each, takes a new track: an empty one that is not playing, then
/// any that is not playing, else B.
fn pick_deck(state: [(bool, bool); 2]) -> usize {
    (0..2).find(|&d| !state[d].0 && !state[d].1).or_else(|| (0..2).rev().find(|&d| !state[d].1)).unwrap_or(1)
}

impl DubplateApp {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        crate::wave_draw::load_settings();
        crate::theme::load();
        // tracks on a Reel whose bar line is in doubt are separated in the background, a deck's request always first
        std::thread::spawn(|| {
            if let Ok(db) = crate::db::LibraryDb::open() {
                crate::stem_queue::enqueue_all_reels(&db);
            }
        });
        let crossfader = Arc::new(AtomicU32::new(0.5f32.to_bits()));
        let record_sink = new_sink();
        let crossfader_curve = Arc::new(AtomicU32::new(dubplate_engine::CF_SMOOTH));
        let audio_settings = crate::audio_settings::AudioSettings::load();
        let monitor = Arc::new(dubplate_engine::MonitorControls::default());
        let (audio, [handle_a, handle_b], prep) =
            open_audio(&audio_settings, &crossfader, &crossfader_curve, &monitor, &record_sink)
                .or_else(|e| {
                    // The saved device may be unplugged: fall back to the system default.
                    log::warn!("saved audio output failed ({e}); using the system default");
                    open_audio(&Default::default(), &crossfader, &crossfader_curve, &monitor, &record_sink)
                })
                .expect("audio init failed");
        log::info!("audio output running @ {}Hz", audio.sample_rate);

        let audio_devices = dubplate_engine::list_output_devices_with_channels();
        let deck_settings = crate::deck_settings::DeckSettings::load();
        let selected_device = audio.device_name.clone();

        let (decode_tx, decode_rx) = mpsc::sync_channel::<(usize, std::path::PathBuf, Arc<DecodedTrack>)>(4);
        let (midi_tx, midi_rx) = mpsc::sync_channel(64);
        let (stem_tx, stem_rx) = mpsc::sync_channel::<(usize, Result<Arc<TrackStems>, String>)>(2);
        let (wave_tx, wave_rx) = mpsc::channel();
        let stem_available = is_stem_engine_available();
        match dubplate_engine::stem_engine_description() {
            Some(d) => log::info!("stem engine: {d}"),
            None => log::info!("stem engine: none — {}", dubplate_engine::stem_engine_help()),
        }

        let midi = MidiHub::new(midi_tx);

        let db = match LibraryDb::open() {
            Ok(d) => {
                log::info!("library DB opened");
                Some(d)
            }
            Err(e) => {
                log::warn!("library DB unavailable: {e}");
                None
            }
        };

        Self {
            decks: [handle_a, handle_b],
            deck_views: [DeckView::new(0), DeckView::new(1)],
            crossfader,
            crossfader_curve,
            _audio: audio,
            decode_tx,
            decode_rx,
            midi,
            midi_rx,
            prefs: prefs::PrefsState::default(),
            #[cfg(feature = "vst3-host")]
            master_fx: crate::master_fx::MasterFx::new(&monitor.insert),
            #[cfg(feature = "vst3-host")]
            deck_fx: [
                crate::master_fx::MasterFx::new_in(crate::master_fx::DECK_A, &monitor.deck_inserts[0]),
                crate::master_fx::MasterFx::new_in(crate::master_fx::DECK_B, &monitor.deck_inserts[1]),
            ],
            eq: [[1.0; 3]; 2],
            pregain: [1.0; 2],
            auto_gain: deck_settings.auto_gain,
            key_lock: deck_settings.key_lock,
            key_shift: [0; 2],
            trim_db: [0.0; 2],
            filter: [0.5; 2],
            filter_mode: [0; 2],
            stem_tx,
            stem_rx,
            stem_analyzing: [false; 2],
            stem_gains: [[1.0; 4]; 2],
            stem_waves: [None, None],
            deck_stems: [None, None],
            waves_building: [false; 2],
            wave_tx,
            wave_rx,
            stem_progress: [
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            ],
            stem_available,
            load_gen: [Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0))],
            sync_on: [false; 2],
            sync_limited: [false; 2],
            sync_last_master: [0.0; 2],
            sync_err_ema: [0.0; 2],
            sync_nudge: [0.0; 2],
            cue_state: [CueState::default(); 2],
            load_flash_timer: 0.0,
            load_flash_msg: String::new(),
            audio_devices,
            audio_message: None,
            audio_settings,
            monitor,
            selected_device,
            record_sink,
            record_session: None,
            record_start: None,
            library: Library::new(),
            db,
            prep,
            flash_is_news: false,
            loads: Default::default(),
            shot: Shot::from_env(),
            audio_priority_logged: false,
            prepare: None,
            prep_started_over_decks: false,
        }
    }

    /// Applies a manual beat-grid edit to a deck and persists it (or clears it on reset).
    /// BPM is whole-number: the grid keeps the true (possibly fractional) tempo and
    /// the deck plays at the rounded value.
    fn apply_grid_edit(&mut self, idx: usize, edit: GridEdit) {
        let deck = &mut self.decks[idx];
        let Some(path) = deck.track_path.clone() else { return };
        let mut grid = deck.grid;
        let analysed = deck.analyzed_grid;
        match edit {
            GridEdit::SetBpm(bpm) => {
                let whole = bpm.round().clamp(40.0, 250.0);
                if (whole - grid.bpm.round()).abs() >= 0.5 {
                    // Typing the analysed tempo's own whole number keeps its precision.
                    grid.bpm = if analysed.is_valid() && (whole - analysed.bpm.round()).abs() < 0.5 {
                        analysed.bpm
                    } else {
                        whole
                    };
                }
            }
            GridEdit::Scale(f) => grid.bpm = (grid.bpm * f).clamp(40.0, 250.0),
            GridEdit::SetDownbeat(offset) => grid.offset = offset.max(0.0),
            GridEdit::Reset => grid = analysed,
        }
        deck.set_grid(grid);
        let reset = matches!(edit, GridEdit::Reset);
        if let Some(db) = &self.db {
            let res = if reset { db.clear_grid(&path) } else { db.save_grid(&path, grid) };
            if let Err(e) = res {
                log::warn!("saving beat grid failed: {e}");
            }
        }
        // Followers re-match when this deck's tempo changed; this deck's own sync is stale.
        self.release_sync(idx);
    }

    // ── SYNC ─────────────────────────────────────────────────────────────────
    // Beat sync: matches tempo and beat positions to the other
    // (master) deck and stays locked to its tempo. Phrase alignment is our extension:
    // a stopped synced deck waits so it starts at the same position within a 16-beat
    // phrase as the master.

    fn can_sync(&self, d: usize) -> bool {
        let m = 1 - d;
        self.decks[d].track.is_some()
            && self.decks[m].track.is_some()
            && self.decks[d].grid.is_valid()
            && self.decks[m].grid.is_valid()
    }

    /// The deck's key as the audience hears it: the track's key moved by its key shift (key lock on) or by the
    /// pitch change the tempo makes (key lock off).
    fn effective_key(&self, d: usize) -> Option<dubplate_engine::meta::Key> {
        let k = self.decks[d].track.as_ref()?.key?;
        let semis = if self.key_lock[d] {
            self.key_shift[d]
        } else {
            (12.0 * (self.decks[d].shared.tempo_ratio() as f64).log2()).round() as i32
        };
        Some(k.shifted(semis))
    }

    /// Key lock and auto gain are kept for next time (`decks.toml`).
    fn save_deck_settings(&self) {
        crate::deck_settings::DeckSettings { key_lock: self.key_lock, auto_gain: self.auto_gain }.save();
    }

    fn toggle_key_lock(&mut self, d: usize) {
        self.key_lock[d] = !self.key_lock[d];
        self.decks[d].send(DeckCommand::SetKeyLock(self.key_lock[d]));
        self.save_deck_settings();
    }

    fn toggle_auto_gain(&mut self, d: usize) {
        self.auto_gain[d] = !self.auto_gain[d];
        self.apply_auto_gain(d);
        self.save_deck_settings();
    }

    fn set_key_shift(&mut self, d: usize, semitones: i32) {
        self.key_shift[d] = semitones.clamp(-6, 6);
        if self.key_shift[d] != 0 && !self.key_lock[d] {
            // A shift is made by the stretcher, so it needs key lock.
            self.key_lock[d] = true;
            self.decks[d].send(DeckCommand::SetKeyLock(true));
            self.save_deck_settings();
        }
        self.decks[d].send(DeckCommand::SetKeyShift(self.key_shift[d] as f32));
    }

    /// Key Sync: shifts this deck the smallest amount that makes its key mix with the other deck's.
    fn key_sync(&mut self, d: usize) {
        let (Some(mine), Some(theirs)) = (self.decks[d].track.as_ref().and_then(|t| t.key), self.effective_key(1 - d))
        else {
            return;
        };
        // Compare from the key lock's reference: the track's own key, whatever its tempo does.
        let s = mine.shift_to_match(&theirs);
        self.set_key_shift(d, s);
    }

    fn deck_playing(&self, d: usize) -> bool {
        self.decks[d].shared.is_playing.load(Ordering::Relaxed)
    }

    /// Sets the deck's track gain from the track's measured loudness (or removes it).
    fn apply_auto_gain(&mut self, d: usize) {
        let db = match (self.auto_gain[d], self.decks[d].track.as_ref().and_then(|t| t.loudness)) {
            (true, Some(m)) => dubplate_engine::loudness::auto_trim_db(m, dubplate_engine::loudness::TARGET_LUFS),
            _ => 0.0,
        };
        self.trim_db[d] = db;
        self.decks[d].send(DeckCommand::SetTrim(10f32.powf(db / 20.0)));
    }

    fn release_sync(&mut self, d: usize) {
        if self.sync_on[d] {
            self.sync_on[d] = false;
            self.sync_limited[d] = false;
            self.sync_nudge[d] = 0.0;
            self.decks[d].send(DeckCommand::JogNudge(0.0));
        }
    }

    /// Phase error in beats (-0.5..0.5]: positive = this deck is behind the master.
    fn phase_error_beats(&self, d: usize) -> Option<f64> {
        let bm = self.decks[1 - d].beat_now()?;
        let bs = self.decks[d].beat_now()?;
        let diff = (bm - bs).rem_euclid(1.0);
        Some(if diff > 0.5 { diff - 1.0 } else { diff })
    }

    /// Match this deck's whole-BPM tempo to the master, trying x2 / x0.5 when the
    /// plain value is outside the tempo range.
    fn sync_tempo(&mut self, d: usize) {
        let m = 1 - d;
        let want = self.decks[m].target_bpm;
        let (lo, hi) = self.decks[d].tempo_bounds();
        let nominal = self.decks[d].nominal_bpm();
        let (best, limited) = pick_sync_tempo(want, lo, hi, nominal);
        self.sync_limited[d] = limited;
        self.decks[d].set_target_bpm(best);
        self.sync_last_master[d] = want;
    }

    fn toggle_sync(&mut self, d: usize) {
        if self.sync_on[d] {
            self.release_sync(d);
            return;
        }
        if !self.can_sync(d) {
            self.load_flash_timer = 2.5;
            self.load_flash_msg = "SYNC needs a track with a beat grid on both decks".into();
            return;
        }
        let m = 1 - d;
        self.sync_on[m] = false; // the master never follows
        self.sync_on[d] = true;
        self.sync_tempo(d);

        // Playing against a playing master: close big phase gaps immediately,
        // the lock below trims the remainder smoothly.
        if self.deck_playing(d) && self.deck_playing(m) {
            if let (Some(e), Some(track)) = (self.phase_error_beats(d), self.decks[d].track.clone()) {
                if e.abs() > 0.12 {
                    let shift = e * 60.0 / self.decks[d].grid.bpm as f64 * track.sample_rate as f64;
                    let pos = self.decks[d].shared.position_frames.load(Ordering::Relaxed) as f64 + shift;
                    self.decks[d].send(DeckCommand::Seek(pos / track.duration_frames.max(1) as f64));
                }
            }
        }
    }

    /// Per-frame: follow the master's tempo and keep the beat phase locked.
    fn update_sync(&mut self) {
        for d in 0..2 {
            if !self.sync_on[d] {
                continue;
            }
            let m = 1 - d;
            if !self.can_sync(d) {
                self.release_sync(d);
                continue;
            }
            if self.decks[m].target_bpm != self.sync_last_master[d] {
                self.sync_tempo(d);
            }

            if self.deck_playing(d) && self.deck_playing(m) && !self.deck_views[d].is_jogging() {
                if let Some(e) = self.phase_error_beats(d) {
                    // Smooth the estimate: the two playheads are read a moment apart.
                    self.sync_err_ema[d] += 0.15 * (e - self.sync_err_ema[d]);
                    let err = self.sync_err_ema[d];
                    let ms = err * 60_000.0 / self.decks[m].target_bpm as f64;
                    let nudge = if ms.abs() < 4.0 { 0.0 } else { (err * 0.35).clamp(-0.04, 0.04) as f32 };
                    if (nudge - self.sync_nudge[d]).abs() > 1e-4 {
                        self.decks[d].send(DeckCommand::JogNudge(nudge));
                        self.sync_nudge[d] = nudge;
                    }
                }
            } else if self.sync_nudge[d] != 0.0 {
                self.decks[d].send(DeckCommand::JogNudge(0.0));
                self.sync_nudge[d] = 0.0;
            }
        }
    }

    /// Play, with a phrase-quantised start when this deck is synced to a playing master.
    fn request_play(&mut self, d: usize) {
        self.cancel_previews(d);
        let m = 1 - d;
        if self.sync_on[d] && self.can_sync(d) && self.deck_playing(m) {
            if let (Some(bm), Some(bs)) = (self.decks[m].beat_now(), self.decks[d].beat_now()) {
                if let Some(frames) = phrase_start_delay_frames(
                    bs,
                    bm,
                    SYNC_QUANTUM_BEATS,
                    self.decks[m].target_bpm as f64,
                    self._audio.sample_rate as f64,
                ) {
                    self.decks[d].send(DeckCommand::PlayAfter(frames));
                    return;
                }
            }
        }
        self.decks[d].send(DeckCommand::Play);
    }

    fn crossfader_val(&self) -> f32 {
        f32::from_bits(self.crossfader.load(Ordering::Relaxed))
    }

    fn set_crossfader(&self, v: f32) {
        self.crossfader.store(v.to_bits(), Ordering::Relaxed);
    }

    fn poll_decoded(&mut self) {
        while let Ok((idx, path, track)) = self.decode_rx.try_recv() {
            if idx == PREP_SLOT {
                self.open_prepare(path, track);
                continue;
            }
            // Never interrupt a playing deck — the usual DJ-software behaviour.
            if self.decks[idx].shared.is_playing.load(Ordering::Relaxed) {
                log::warn!("deck {idx} is playing — load blocked");
                continue;
            }
            // Persist track metadata and load back any saved cues.
            let track = if let Some(db) = &self.db {
                if let Err(e) = db.upsert_track(&path, track.duration_secs, track.bpm) {
                    log::warn!("db upsert failed: {e}");
                }
                // Keep the browser row current (tempo, key, preview) for anything loaded.
                let preview = track.wave.preview(160);
                let key = track.key.map(|k| (k.tonic, k.minor, k.confidence));
                if let Err(e) = db.save_analysis(&path, track.bpm, track.duration_secs, key, &preview) {
                    log::warn!("saving analysis failed: {e}");
                }
                self.library.reload_soon();
                let saved_cues = db.load_cues(&path);
                if !saved_cues.is_empty() {
                    // Unwrap the just-created Arc (only one reference exists here).
                    match Arc::try_unwrap(track) {
                        Ok(mut t) => {
                            t.cues = saved_cues;
                            Arc::new(t)
                        }
                        Err(t) => t,
                    }
                } else {
                    track
                }
            } else {
                track
            };
            crate::sections::set_drops(&track, self.db.as_ref().and_then(|db| db.load_drops(&path)).as_deref());
            self.decks[idx].load_track(track, path.clone());
            if let Some(g) = self.db.as_ref().and_then(|db| db.load_grid(&path)) {
                self.decks[idx].set_grid(g);
            }
            self.decks[idx].cue_point = self.decks[idx].default_cue_point();
            self.restore_cues(idx);
            self.apply_auto_gain(idx);
            self.decks[idx].send(DeckCommand::SetKeyLock(self.key_lock[idx]));
            self.key_shift[idx] = 0;
            self.decks[idx].send(DeckCommand::SetKeyShift(0.0));
            self.stem_gains[idx] = [1.0; 4];
            self.stem_waves[idx] = None;
            self.deck_stems[idx] = None;
            self.release_sync(idx);

            // Previously separated stems are restored from the on-disk cache
            // automatically; the generation counter drops stale results if the
            // deck is reloaded before the cache read finishes.
            let gen = self.load_gen[idx].fetch_add(1, Ordering::Relaxed) + 1;
            let gen_ref = Arc::clone(&self.load_gen[idx]);
            let tx = self.stem_tx.clone();
            std::thread::spawn(move || {
                dubplate_engine::threads::background();
                if let Some(stems) = dubplate_engine::stems::load_stems_cache(&path) {
                    if gen_ref.load(Ordering::Relaxed) == gen {
                        let _ = tx.send((idx, Ok(stems)));
                    }
                }
            });
        }
    }

    /// Tell each deck's view what to draw as stem lanes this frame: the summaries and gains when the lanes are on and
    /// the deck has stems and the summaries are built, else nothing.
    fn sync_lane_state(&mut self) {
        for d in 0..2 {
            let ready = self.deck_views[d].lanes_on && self.decks[d].shared.stems_loaded.load(Ordering::Relaxed);
            self.deck_views[d].lane_state =
                ready.then(|| self.stem_waves[d].clone().map(|w| (w, self.stem_gains[d]))).flatten();
        }
    }

    /// Take the lanes' summaries that have been built, if they still belong to the track on the deck.
    fn poll_stem_waves(&mut self) {
        while let Ok((idx, path, waves)) = self.wave_rx.try_recv() {
            self.waves_building[idx] = false;
            if self.decks[idx].track_path.as_deref() == Some(path.as_path()) {
                self.stem_waves[idx] = Some(waves);
            }
        }
        // The summaries are only made for a deck that is showing its lanes, one thread, one deck at a time: a track
        // loaded without lanes costs nothing, and the audio is not competing with a burst of work.
        if self.waves_building.iter().any(|&b| b) {
            return;
        }
        for d in 0..2 {
            if !self.deck_views[d].lanes_on || self.stem_waves[d].is_some() {
                continue;
            }
            let (Some(stems), Some(path)) = (self.deck_stems[d].clone(), self.decks[d].track_path.clone()) else {
                continue;
            };
            self.waves_building[d] = true;
            let tx = self.wave_tx.clone();
            std::thread::spawn(move || {
                dubplate_engine::threads::background();
                let _ = tx.send((d, path, Arc::new(dubplate_engine::StemWaves::build(&stems))));
            });
            break;
        }
    }

    /// Receive completed stem analyses from background threads and load them into the deck.
    fn poll_stems(&mut self) {
        while let Ok((deck_idx, result)) = self.stem_rx.try_recv() {
            self.stem_analyzing[deck_idx] = false;
            match result {
                Ok(stems) => {
                    self.deck_stems[deck_idx] = Some(Arc::clone(&stems));
                    self.decks[deck_idx].send(DeckCommand::LoadStems(stems));
                    log::info!("stems loaded into deck {deck_idx}");
                }
                Err(e) => {
                    self.load_flash_timer = 3.0;
                    self.load_flash_msg = format!("Stem analysis failed: {e}");
                    log::error!("stem separation error: {e}");
                }
            }
        }
    }

    /// Kick off stem separation for `deck_idx` in a background thread.
    /// Uses the ONNX model if present (with live progress), else falls back to demucs CLI.
    pub fn analyze_stems(&mut self, deck_idx: usize) {
        if self.stem_analyzing[deck_idx] {
            return;
        }

        let path = match self.decks[deck_idx].track_path.clone() {
            Some(p) => p,
            None => {
                self.load_flash_timer = 2.0;
                self.load_flash_msg = "Load a track first".into();
                return;
            }
        };
        let track = match self.decks[deck_idx].track.clone() {
            Some(t) => t,
            None => {
                self.load_flash_timer = 2.0;
                self.load_flash_msg = "Load a track first".into();
                return;
            }
        };

        self.stem_analyzing[deck_idx] = true;
        self.stem_gains[deck_idx] = [1.0; 4];

        let progress = Arc::clone(&self.stem_progress[deck_idx]);
        progress.store(0, std::sync::atomic::Ordering::Relaxed);

        // one separation at a time: this goes ahead of any background job and behind the other deck's, if it asked first
        crate::stem_queue::queue().request_deck(crate::stem_queue::DeckJob {
            deck: deck_idx,
            track,
            path,
            progress,
            tx: self.stem_tx.clone(),
        });
    }

    /// The deck a track opened without choosing one goes to: an empty deck first, then one that is not playing (never
    /// the one that is), and deck B when both are idle and loaded. When both are playing it is B, and the load is
    /// refused with the usual message.
    fn deck_for_new_track(&self) -> usize {
        let playing = |d: usize| self.decks[d].shared.is_playing.load(Ordering::Relaxed);
        let loaded = |d: usize| self.decks[d].shared.is_loaded.load(Ordering::Relaxed);
        pick_deck([(loaded(0), playing(0)), (loaded(1), playing(1))])
    }

    /// Load into `deck_idx`, showing the denied flash if the deck is currently playing.
    fn try_load(&mut self, deck_idx: usize, path: std::path::PathBuf) {
        if self.decks[deck_idx].shared.is_playing.load(Ordering::Relaxed) {
            let label = if deck_idx == 0 { "A" } else { "B" };
            self.load_flash_timer = 2.5;
            self.load_flash_msg = format!("Deck {} is playing — load blocked", label);
        } else {
            self.load_path(deck_idx, path);
        }
    }

    fn load_path(&self, deck_idx: usize, path: std::path::PathBuf) {
        // Guard at call-site so we don't burn CPU decoding a file that will be dropped.
        if self.decks[deck_idx].shared.is_playing.load(Ordering::Relaxed) {
            log::warn!("deck {deck_idx} is playing — load blocked");
            return;
        }
        let (tx, loads) = (self.decode_tx.clone(), Arc::clone(&self.loads));
        std::thread::spawn(move || decode_for(deck_idx, path, tx, loads));
    }

    fn poll_midi(&mut self) {
        while let Ok(cmd) = self.midi_rx.try_recv() {
            match cmd {
                AppCommand::DeckPlayPause(d) => {
                    let playing = self.decks[d].shared.is_playing.load(Ordering::Relaxed)
                        || self.decks[d].shared.armed.load(Ordering::Relaxed);
                    if playing {
                        self.decks[d].send(DeckCommand::Pause);
                    } else {
                        self.request_play(d);
                    }
                }
                AppCommand::Action(d, action) => self.handle_action(d, action),
                AppCommand::DeckLoad(d) => {
                    if self.decks[d].shared.is_playing.load(Ordering::Relaxed) {
                        let label = if d == 0 { "A" } else { "B" };
                        self.load_flash_timer = 2.5;
                        self.load_flash_msg = format!("Deck {} is playing — load blocked", label);
                    } else if let Some(path) = self.library.selected_path() {
                        self.load_path(d, path);
                    } else {
                        self.load_flash_timer = 2.0;
                        self.load_flash_msg = "Select a track in the library first".into();
                    }
                }
                AppCommand::DeckSetBrake(d, brake) => {
                    self.decks[d].send(DeckCommand::SetBrake(brake));
                }
                AppCommand::DeckSetVolume(d, v) => {
                    self.decks[d].send(DeckCommand::SetGain(v));
                }
                AppCommand::DeckSetGain(d, g) => {
                    self.pregain[d] = g;
                    self.decks[d].send(DeckCommand::SetPregain(g));
                }
                AppCommand::DeckSetTempo(d, r) => {
                    let target = self.decks[d].nominal_bpm() * r;
                    self.decks[d].set_target_bpm(target);
                    self.release_sync(d);
                }
                AppCommand::DeckJogBend(d, b) => {
                    self.decks[d].send(DeckCommand::JogBend(b));
                }
                AppCommand::DeckEqHigh(d, v) => {
                    self.eq[d][0] = v;
                    self.decks[d].send(DeckCommand::SetEqHi(v));
                }
                AppCommand::DeckEqMid(d, v) => {
                    self.eq[d][1] = v;
                    self.decks[d].send(DeckCommand::SetEqMid(v));
                }
                AppCommand::DeckEqLow(d, v) => {
                    self.eq[d][2] = v;
                    self.decks[d].send(DeckCommand::SetEqLo(v));
                }
                AppCommand::DeckSetFilter(d, v) => {
                    self.filter[d] = v;
                    self.decks[d].send(DeckCommand::SetFilter(v));
                }
                AppCommand::SetCrossfader(v) => {
                    self.set_crossfader(v);
                }
                AppCommand::DeckSearch(d, steps) => {
                    if let Some(track) = self.decks[d].track.clone() {
                        // Each jog step moves a quarter of a second.
                        let secs = steps as f64 * 0.25;
                        let frames = secs * track.sample_rate as f64;
                        let to = (self.decks[d].position() + frames).clamp(0.0, track.duration_frames as f64 - 1.0);
                        self.decks[d].send(DeckCommand::Seek(to / track.duration_frames.max(1) as f64));
                    }
                }
                AppCommand::ToggleCue(d) => self.toggle_cue(d),
                AppCommand::ToggleMasterMonitor => self.toggle_master_monitor(),
                AppCommand::DeckSync(d) => self.toggle_sync(d),
                AppCommand::DeckKeyLock(d) if d < 2 => self.toggle_key_lock(d),
                AppCommand::DeckAutoGain(d) if d < 2 => self.toggle_auto_gain(d),
                AppCommand::DeckKeyLock(_) | AppCommand::DeckAutoGain(_) => {}
                AppCommand::Browse(n) => self.library.move_selection(n),
                AppCommand::DeckStemToggle(d, s) => {
                    if d < 2 && s < 4 {
                        let g = &mut self.stem_gains[d][s];
                        *g = if *g > 0.5 { 0.0 } else { 1.0 };
                        let new_g = *g;
                        self.decks[d].send(DeckCommand::SetStemGain(s, new_g));
                    }
                }
            }
        }
    }

    /// Drives a [`Shot`]: open Prepare, set it up, ask for the picture, write it and quit.
    fn shot_step(&mut self, ctx: &egui::Context, cpu: Option<f32>) {
        let Some(mut shot) = self.shot.take() else { return };
        ctx.request_repaint();
        if shot.prefs {
            self.prefs.open = true;
        }
        if let (Some(n), Some(c)) = (shot.frames, cpu) {
            // from when things have settled until the picture is asked for (taking it is slow in itself)
            if n > 30 && n <= if shot.deck { 240 } else { 200 } {
                shot.cpu.push(c);
            }
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let mut ppm = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            ppm.extend(image.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]));
            if let Err(e) = std::fs::write(&shot.out, ppm) {
                log::error!("writing {}: {e}", shot.out.display());
            }
            let mut c = shot.cpu.clone();
            c.sort_by(f32::total_cmp);
            if !c.is_empty() {
                let ms = |i: usize| c[i.min(c.len() - 1)] * 1000.0;
                log::info!(
                    "frame cpu over {} frames: median {:.2} ms, 95th {:.2} ms, worst three {:.2} {:.2} {:.2} ms",
                    c.len(),
                    ms(c.len() / 2),
                    ms(c.len() * 95 / 100),
                    ms(c.len().saturating_sub(3)),
                    ms(c.len().saturating_sub(2)),
                    ms(c.len() - 1)
                );
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        match shot.frames {
            None if shot.deck => {
                self.load_path(0, shot.track.clone());
                shot.frames = Some(0);
            }
            Some(n) if shot.deck => {
                if self.decks[0].track.is_some() {
                    if n == 0 {
                        let sr = self.decks[0].track.as_ref().map_or(44100, |t| t.sample_rate);
                        self.decks[0].send(DeckCommand::Jump(shot.at * sr as f64));
                        if shot.demo {
                            // a few cues, so the platter's ticks and the waveform's flags are in the picture
                            for (pad, secs, color) in
                                [(1u8, 12.0, [230, 60, 60]), (2, 72.0, [60, 200, 90]), (3, 121.0, [70, 130, 240])]
                            {
                                let cue = dubplate_engine::Cue::new(Some(pad), secs * sr as f64, None, sr, color);
                                self.decks[0].cues.push(cue);
                            }
                        }
                        self.decks[0].send(DeckCommand::SetGain(0.0)); // silently: nobody asked to hear it
                        self.decks[0].send(DeckCommand::Play);
                    }
                    if n == 240 {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                    }
                    shot.frames = Some(n + 1);
                }
            }
            None => {
                self.start_prepare(shot.track.clone());
                shot.frames = Some(0);
            }
            Some(n) if self.prepare.is_some() => {
                if n == 0 {
                    let sr = self.prepare.as_ref().map_or(44100, |p| p.track.sample_rate);
                    self.prep.send(DeckCommand::Jump(shot.at * sr as f64));
                }
                if n == 20 {
                    if let Some(p) = self.prepare.as_mut() {
                        p.set_up_for_a_picture(shot.grid_tab, shot.demo);
                    }
                }
                if n == 30 {
                    self.prep.send(DeckCommand::SetGain(0.0)); // silently: nobody asked to hear it
                    self.prep_toggle_play();
                }
                if n == 200 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                }
                shot.frames = Some(n + 1);
            }
            Some(_) => {}
        }
        self.shot = Some(shot);
    }

    // ── Prepare ──────────────────────────────────────────────────────────────

    /// Opens the Prepare screen for a track: from a deck that already holds it, or by decoding it.
    fn start_prepare(&mut self, path: std::path::PathBuf) {
        for d in 0..2 {
            if self.decks[d].track_path.as_deref() == Some(path.as_path()) {
                if let Some(t) = self.decks[d].track.clone() {
                    self.open_prepare(path, t);
                    return;
                }
            }
        }
        let tx = self.decode_tx.clone();
        self.load_flash_msg = format!(
            "Opening {} in Prepare …",
            path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        );
        self.load_flash_timer = 10.0;
        self.flash_is_news = true;
        let loads = Arc::clone(&self.loads);
        std::thread::spawn(move || decode_for(PREP_SLOT, path, tx, loads));
    }

    fn open_prepare(&mut self, path: std::path::PathBuf, track: Arc<DecodedTrack>) {
        if self.prepare.is_some() {
            self.close_prepare();
        }
        let saved = self.db.as_ref().and_then(|db| db.load_grid(&path));
        // What the library held before this look, then bring the library up to date with the analysis just made.
        let library_bpm = self.db.as_ref().and_then(|db| db.track_bpm(&path));
        if let Some(db) = &self.db {
            let preview = track.wave.preview(160);
            let key = track.key.map(|k| (k.tonic, k.minor, k.confidence));
            if db.save_analysis(&path, track.bpm, track.duration_secs, key, &preview).is_ok() {
                self.library.reload_soon();
            }
        }
        self.prep.load_track(Arc::clone(&track), path.clone());
        // Always normal speed, whatever the grid says.
        self.prep.send(DeckCommand::SetTempo(1.0));
        self.monitor.prep_exited.store(false, Ordering::Relaxed);
        self.monitor.prep_blocked.store(false, Ordering::Relaxed);
        let (cues, main_cue) = match &self.db {
            Some(db) => (db.load_cues(&path), db.load_main_cue(&path)),
            None => (Vec::new(), None),
        };
        let drops = self.db.as_ref().and_then(|db| db.load_drops(&path));
        let mut p = Prepare::new(path, track, saved, cues, main_cue, drops);
        p.check_tempo(library_bpm);
        p.sync_click(&self.monitor.click);
        self.prepare = Some(p);
        self.load_flash_msg.clear();
        self.load_flash_timer = 0.0;
        self.flash_is_news = false;
    }

    fn close_prepare(&mut self) {
        self.prep.send(DeckCommand::Pause);
        self.prep.send(DeckCommand::SetLoop(None));
        self.monitor.click.on.store(false, Ordering::Relaxed);
        if let Some(p) = &self.prepare {
            p.forget_unsaved_drops();
        }
        self.prepare = None;
    }

    fn prep_toggle_play(&mut self) {
        let playing = self.prep.shared.is_playing.load(Ordering::Relaxed);
        if playing {
            self.prep.send(DeckCommand::Pause);
        } else {
            self.prep_started_over_decks = self.deck_playing(0) || self.deck_playing(1);
            self.monitor.prep_blocked.store(false, Ordering::Relaxed);
            self.prep.send(DeckCommand::SetTempo(1.0));
            self.prep.send(DeckCommand::Play);
        }
    }

    /// What to tell the user about where the prepare player is (or is not) being heard.
    fn prep_status(&self) -> PrepStatus {
        let decks = self.deck_playing(0) || self.deck_playing(1);
        let playing = self.prep.shared.is_playing.load(Ordering::Relaxed);
        let phones = self._audio.monitor_active;
        let (note, warn) = if self.monitor.prep_blocked.load(Ordering::Relaxed) {
            (format!("Can't play: a deck is playing and there is no working headphone output ({}). Set one in the audio settings, or stop the deck.", self._audio.monitor_status), true)
        } else if playing && self.prep_started_over_decks {
            ("Playing in the headphones only.".to_owned(), false)
        } else if playing {
            ("Playing on the main output. It stops if a deck starts.".to_owned(), false)
        } else if decks && phones {
            ("A deck is playing: Prepare will play in the headphones only.".to_owned(), false)
        } else if decks {
            (
                format!(
                    "A deck is playing and there is no working headphone output ({}), so Prepare can't play yet.",
                    self._audio.monitor_status
                ),
                true,
            )
        } else {
            ("Prepare plays on the main output while no deck is playing.".to_owned(), false)
        };
        let deck_playing_it = (0..2).any(|d| {
            self.deck_playing(d)
                && self.decks[d].track_path.as_deref() == self.prepare.as_ref().map(|p| p.path.as_path())
        });
        PrepStatus { note, warn, deck_playing_it }
    }

    fn save_prepare(&mut self) {
        let Some(p) = self.prepare.as_mut() else { return };
        let grid = p.grid;
        let analysed = p.analysed;
        let path = p.path.clone();
        if let Some(db) = &self.db {
            let res = if grid == analysed { db.clear_grid(&path) } else { db.save_grid(&path, grid) };
            if let Err(e) = res {
                log::warn!("saving beat grid failed: {e}");
                p.note = format!("Not saved: {e}");
                return;
            }
        } else {
            p.note = "Not saved: the library database is not available".into();
            return;
        }
        // Cue point and hot cues.
        let (cue_point, cues) = (p.cue_point, p.cues.clone());
        if let Some(db) = &self.db {
            let mut failed = None;
            if let Err(e) = db.save_main_cue(&path, cue_point) {
                failed = Some(e);
            }
            for pad in 1..=8u8 {
                let r = match cues.iter().find(|c| c.hot_cue == Some(pad)) {
                    Some(c) => db.save_cue(&path, c),
                    None => db.delete_cue(&path, pad),
                };
                if let Err(e) = r {
                    failed = Some(e);
                }
            }
            if let Err(e) = db.save_drops(&path, p.drops.as_deref()) {
                failed = Some(e);
            }
            if let Some(e) = failed {
                log::warn!("saving cues failed: {e}");
                p.note = format!("Cues not saved: {e}");
                return;
            }
        }
        p.mark_saved();
        p.note = "Saved".into();
        self.library.invalidate_cues();
        // A deck holding this track shows the drops as saved (its copy of the track may be a different one).
        let drops = self.prepare.as_ref().and_then(|p| p.drops.clone());
        for d in 0..2 {
            if self.decks[d].track_path.as_deref() == Some(path.as_path()) {
                if let Some(t) = &self.decks[d].track {
                    crate::sections::set_drops(t, drops.as_deref());
                }
            }
        }
        // A deck holding this track, and not playing, takes the new grid now.
        for d in 0..2 {
            if self.decks[d].track_path.as_deref() == Some(path.as_path()) && !self.deck_playing(d) {
                self.decks[d].set_grid(grid);
                self.decks[d].cues = cues.clone();
                self.decks[d].cue_point = cue_point;
                self.release_sync(d);
            }
        }
        self.library.reload_soon();
    }

    fn open_file(&self, deck_idx: usize) {
        if self.decks[deck_idx].shared.is_playing.load(Ordering::Relaxed) {
            log::warn!("deck {deck_idx} is playing — open-file blocked");
            return;
        }
        let (tx, loads) = (self.decode_tx.clone(), Arc::clone(&self.loads));
        std::thread::spawn(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Audio", &["mp3", "flac", "wav", "aiff", "aif", "m4a", "ogg", "opus"])
                .pick_file()
            else {
                return;
            };
            decode_for(deck_idx, path, tx, loads);
        });
    }

    /// Applies the saved output / monitor choices. Saves the current tracks, recreates the whole
    /// audio pipeline, then restores the tracks. On failure the old pipeline keeps running.
    fn restart_audio(&mut self) {
        let tracks: [(Option<Arc<DecodedTrack>>, Option<std::path::PathBuf>); 2] = [
            (self.decks[0].track.clone(), self.decks[0].track_path.clone()),
            (self.decks[1].track.clone(), self.decks[1].track_path.clone()),
        ];
        match open_audio(
            &self.audio_settings,
            &self.crossfader,
            &self.crossfader_curve,
            &self.monitor,
            &self.record_sink,
        ) {
            Ok((audio, decks, prep)) => {
                log::info!("audio: {} / {}", audio.device_name, audio.monitor_status);
                let prepared = self.prepare.as_ref().map(|p| (Arc::clone(&p.track), p.path.clone()));
                self.prep = prep;
                if let Some((t, p)) = prepared {
                    self.prep.load_track(t, p);
                    self.prep.send(DeckCommand::SetTempo(1.0));
                }
                self.selected_device = audio.device_name.clone();
                self.audio_message = None;
                self.decks = decks;
                self.deck_views = [DeckView::new(0), DeckView::new(1)];
                self._audio = audio;
                self.audio_settings.save();
                for (i, (track, path)) in tracks.into_iter().enumerate() {
                    if let Some(t) = track {
                        let p = path.unwrap_or_default();
                        self.decks[i].load_track(t, p);
                    }
                    self.apply_auto_gain(i);
                    self.decks[i].send(DeckCommand::SetKeyLock(self.key_lock[i]));
                    self.decks[i].send(DeckCommand::SetKeyShift(self.key_shift[i] as f32));
                }
            }
            Err(e) => {
                log::error!("audio restart failed: {e}");
                self.audio_message = Some(format!("Couldn't open that output: {e}"));
                // Show what is really in use again.
                self.audio_settings.output = Some(self.selected_device.clone());
            }
        }
    }

    /// Cue / master-monitor toggles shared by the on-screen buttons and MIDI.
    fn toggle_cue(&self, d: usize) {
        if let Some(c) = self.monitor.cue.get(d) {
            c.fetch_xor(true, Ordering::Relaxed);
        }
    }

    fn toggle_master_monitor(&self) {
        self.monitor.master.fetch_xor(true, Ordering::Relaxed);
    }

    fn apply_visuals(&self, ctx: &egui::Context) {
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = palette::background();
        vis.window_fill = palette::surface();
        vis.faint_bg_color = palette::surface();
        vis.extreme_bg_color = palette::background();
        vis.widgets.noninteractive.bg_fill = palette::surface();
        vis.widgets.inactive.bg_fill = palette::surface();
        vis.widgets.hovered.bg_fill = palette::hover();
        vis.widgets.active.bg_fill = palette::pressed();
        vis.selection.bg_fill = palette::deck_a().linear_multiply(0.28);
        vis.widgets.noninteractive.fg_stroke.color = palette::label();
        vis.widgets.inactive.fg_stroke.color = palette::button_text();
        ctx.set_visuals(vis);
    }
}

impl eframe::App for DubplateApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.shot_step(ctx, _frame.info().cpu_usage);
        // what the audio thread is done with (old tracks, stems, effects) is freed here, not there
        for d in self.decks.iter_mut() {
            d.empty_trash();
        }
        self.prep.empty_trash();
        self.library.set_live(self.deck_playing(0) || self.deck_playing(1));
        {
            let mut l = self.loads.lock().unwrap();
            for d in 0..2 {
                self.deck_views[d].loading = l.coming[d].last().cloned();
            }
            if let Some(why) = l.failed.take() {
                self.load_flash_msg = why;
                self.load_flash_timer = 5.0;
            }
        }
        if !self.audio_priority_logged {
            if let Some(got) = dubplate_engine::threads::audio_thread_status() {
                log::info!("audio thread: {got}");
                self.audio_priority_logged = true;
            }
        }
        self.poll_decoded();
        self.poll_midi();
        self.poll_stems();
        self.poll_stem_waves();
        self.sync_lane_state();
        crate::theme::frame(ctx);
        self.apply_visuals(ctx);
        for i in 0..2 {
            let enabled = self.can_sync(i);
            let key_info = self.decks[i].track.as_ref().and_then(|t| t.key).map(|k| {
                let eff = self.effective_key(i).unwrap_or(k);
                (k.camelot_label(), self.key_shift[i], eff.camelot_label(), eff.name())
            });
            let key_sync_enabled =
                self.decks[i].track.as_ref().is_some_and(|t| t.key.is_some()) && self.effective_key(1 - i).is_some();
            // locked = the smoothed beat error against the other deck is under 8 ms
            let sync_lock = (self.sync_on[i] && self.deck_playing(i) && self.deck_playing(1 - i)).then(|| {
                let ms = self.sync_err_ema[i] * 60_000.0 / self.decks[1 - i].target_bpm.max(1.0) as f64;
                ms.abs() < 8.0
            });
            let v = &mut self.deck_views[i];
            v.key_info = key_info;
            v.key_sync_enabled = key_sync_enabled;
            v.sync_on = self.sync_on[i];
            v.key_lock_on = self.key_lock[i];
            v.sync_enabled = enabled;
            v.sync_limited = self.sync_limited[i];
            v.sync_lock = sync_lock;
        }

        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        for (n, file) in dropped.into_iter().enumerate() {
            if let Some(path) = file.path {
                // Pick the first non-playing deck; load_path guards is_playing anyway.
                let idx = if n == 0 {
                    if !self.decks[0].shared.is_loaded.load(Ordering::Relaxed) {
                        0
                    } else {
                        1
                    }
                } else {
                    1
                };
                self.load_path(idx, path);
            }
        }

        let mut open_file_for: Option<usize> = None;
        let mut wf_dnd_a: Option<std::path::PathBuf> = None;
        let mut wf_dnd_b: Option<std::path::PathBuf> = None;

        // ── Menu bar — thin top strip: REC, MIDI, audio output ──────────────
        self.show_menubar(ctx);
        #[cfg(feature = "vst3-host")]
        self.master_fx.window(ctx, &self.monitor.insert);
        #[cfg(feature = "vst3-host")]
        for d in 0..2 {
            self.deck_fx[d].window(ctx, &self.monitor.deck_inserts[d]);
        }
        self.show_prefs(ctx);

        // ── Stacked waveforms — Deck A top, Deck B bottom, full-width ────────
        // Prepare needs the room, so the deck waveforms start smaller while it is open (they stay resizable).
        let preparing = self.prepare.is_some();
        // Each deck that shows its stem lanes needs a strip more, so the panel gets taller (and is a different panel,
        // so egui starts it at the new height instead of squeezing the lanes into the old one).
        let lane_decks = self.deck_views.iter().filter(|v| v.lane_state.is_some()).count();
        let extra = lane_decks as f32 * (crate::deck_view::LANE_H * 4.0 + 2.0);
        // In a window too short for the deck waveforms, the decks and Prepare together (the default 1280x800 is),
        // Prepare gets the waveforms' room while it is open: its own waveform is the one being worked on, and the
        // decks still show where they are on their platters.
        let deck_row_h = DECK_ROW_H + crate::deck_view::extra_height();
        let needed = MENU_BAR_H + 120.0 + extra + deck_row_h + crate::prepare::MIN_HEIGHT;
        let room_for_waveforms = !preparing || ctx.screen_rect().height() >= needed;
        egui::TopBottomPanel::top(
            egui::Id::new(if preparing { "waveforms_prepare" } else { "waveforms" }).with(lane_decks),
        )
        .resizable(true)
        .default_height(if preparing { 120.0 } else { 230.0 } + extra)
        .height_range(120.0..=340.0 + extra)
        .frame(egui::Frame::none().fill(palette::background()))
        .show_animated(ctx, room_for_waveforms, |ui| {
            let full = ui.available_rect_before_wrap();
            let half_h = full.height() / 2.0;
            let rect_a = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), half_h - 1.0));
            let rect_b = egui::Rect::from_min_size(
                full.min + egui::vec2(0.0, half_h + 1.0),
                egui::vec2(full.width(), half_h - 1.0),
            );

            wf_dnd_a = self.deck_views[0].show_waveform_rect(ui, rect_a, &self.decks[0]);
            wf_dnd_b = self.deck_views[1].show_waveform_rect(ui, rect_b, &self.decks[1]);

            // Horizontal divider between the two waveforms
            let cy = full.center().y;
            ui.painter().line_segment(
                [egui::pos2(full.left(), cy), egui::pos2(full.right(), cy)],
                egui::Stroke::new(1.0_f32, palette::divider()),
            );
            ui.allocate_rect(full, egui::Sense::hover());
        });

        if let Some(p) = wf_dnd_a {
            self.load_path(0, p);
        }
        if let Some(p) = wf_dnd_b {
            self.load_path(1, p);
        }

        // ── Three-column layout: Deck A | Mixer (fixed 220px) | Deck B ──────────
        // CentralPanel fills ALL remaining space; we manually divide it into three
        // columns so the mixer stays narrow regardless of window width.
        let mut analyze_stems_for: Option<usize> = None;
        let mut stem_gain_changes: [Option<(usize, f32)>; 4] = [None; 4];

        egui::TopBottomPanel::top("decks")
            .exact_height(deck_row_h)
            .frame(egui::Frame::none().fill(palette::background()))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let mixer_w = MIXER_W;
                let deck_w = ((avail.width() - mixer_w) / 2.0).max(0.0);
                let left = avail.left();
                let top = avail.top();
                let h = avail.height();

                let rect_a = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(deck_w, h));
                let rect_mix = egui::Rect::from_min_size(egui::pos2(left + deck_w, top), egui::vec2(mixer_w, h));
                let rect_b = egui::Rect::from_min_size(egui::pos2(left + deck_w + mixer_w, top), egui::vec2(deck_w, h));

                // Section backgrounds
                ui.painter().rect_filled(rect_a, 0.0, palette::surface());
                ui.painter().rect_filled(rect_mix, 0.0, palette::background());
                ui.painter().rect_filled(rect_b, 0.0, palette::surface());

                // Vertical dividers
                for &dx in &[deck_w, deck_w + mixer_w] {
                    let px = left + dx;
                    ui.painter().line_segment(
                        [egui::pos2(px, top), egui::pos2(px, top + h)],
                        egui::Stroke::new(1.0_f32, palette::divider()),
                    );
                }

                // Deck A
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect_a), |ui| {
                    let stems_loaded = self.decks[0].shared.stems_loaded.load(Ordering::Relaxed);
                    let stem_pct = self.stem_progress[0].load(Ordering::Relaxed);
                    let (open, analyze, gc) = self.deck_views[0].show(
                        ui,
                        &self.decks[0],
                        stems_loaded,
                        self.stem_analyzing[0],
                        stem_pct,
                        &mut self.stem_gains[0],
                        self.stem_available,
                    );
                    if open {
                        open_file_for = Some(0);
                    }
                    if analyze {
                        analyze_stems_for = Some(0);
                    }
                    for (i, g) in gc.into_iter().enumerate() {
                        if let Some(v) = g {
                            stem_gain_changes[i] = Some((0, v));
                        }
                    }
                });

                // Mixer (narrow fixed column)
                self.show_mixer(ui, rect_mix);

                // Deck B
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect_b), |ui| {
                    let stems_loaded = self.decks[1].shared.stems_loaded.load(Ordering::Relaxed);
                    let stem_pct = self.stem_progress[1].load(Ordering::Relaxed);
                    let (open, analyze, gc) = self.deck_views[1].show(
                        ui,
                        &self.decks[1],
                        stems_loaded,
                        self.stem_analyzing[1],
                        stem_pct,
                        &mut self.stem_gains[1],
                        self.stem_available,
                    );
                    if open {
                        open_file_for = Some(1);
                    }
                    if analyze {
                        analyze_stems_for = Some(1);
                    }
                    for (i, g) in gc.into_iter().enumerate() {
                        if let Some(v) = g {
                            stem_gain_changes[i] = Some((1, v));
                        }
                    }
                });
            });

        // ── Library browser — fills everything below the deck row ──────────
        let mut lib_action: Option<LibraryAction> = None;
        let mut prep_events: Option<crate::prepare::PrepEvents> = None;
        let prep_status = if self.prepare.is_some() {
            self.prep_status()
        } else {
            PrepStatus { note: String::new(), warn: false, deck_playing_it: false }
        };
        let deck_refs: [DeckRef; 2] = [0usize, 1].map(|i| DeckRef {
            path: self.decks[i].track_path.clone(),
            bpm: if self.decks[i].grid.is_valid() { self.decks[i].target_bpm } else { 0.0 },
            playing: self.deck_playing(i),
        });
        let dt = ctx.input(|i| i.unstable_dt);
        self.load_flash_timer = (self.load_flash_timer - dt).max(0.0);
        if self.load_flash_timer == 0.0 {
            self.flash_is_news = false;
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(palette::background())).show(ctx, |ui| {
            // Denied-load flash banner — fades out over the flash duration
            if self.load_flash_timer > 0.0 {
                let alpha = ((self.load_flash_timer * 100.0).min(200.0)) as u8;
                let bw = ui.available_width();
                let (br, _) = ui.allocate_exact_size(egui::vec2(bw, 22.0), egui::Sense::hover());
                // red for "no", blue for "working on it"
                let fill = if self.flash_is_news { [20, 80, 130] } else { [160, 30, 30] };
                ui.painter_at(br).rect_filled(
                    br,
                    0.0,
                    Color32::from_rgba_unmultiplied(fill[0], fill[1], fill[2], alpha),
                );
                ui.painter_at(br).text(
                    br.center(),
                    egui::Align2::CENTER_CENTER,
                    self.load_flash_msg.clone(),
                    egui::FontId::proportional(crate::theme::sz(11.0)),
                    Color32::from_rgba_unmultiplied(255, 220, 220, alpha),
                );
            }
            if let Some(prepare) = self.prepare.as_mut() {
                prep_events = Some(
                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false; 2])
                                .show(ui, |ui| prepare.show(ui, &self.prep, &prep_status))
                                .inner
                        })
                        .inner,
                );
            } else {
                lib_action = self.library.show(ui, &deck_refs);
            }
        });

        match lib_action {
            Some(LibraryAction::LoadAny(path)) => {
                let idx = self.deck_for_new_track();
                if self.decks.iter().all(|d| d.shared.is_playing.load(Ordering::Relaxed)) {
                    self.load_flash_timer = 2.5;
                    self.load_flash_msg = "Both decks are playing — load blocked".into();
                } else {
                    self.try_load(idx, path);
                }
            }
            Some(LibraryAction::LoadDeck(d, path)) => {
                self.try_load(d, path);
            }
            Some(LibraryAction::Prepare(path)) => self.start_prepare(path),
            None => {}
        }

        // The Prepare screen: its buttons, and a deck starting under it.
        if let Some(ev) = prep_events {
            if ev.toggle_play {
                self.prep_toggle_play();
            }
            if ev.pause {
                self.prep.send(DeckCommand::Pause);
            }
            if ev.restart {
                self.prep.send(DeckCommand::Seek(0.0));
            }
            if let Some(f) = ev.seek {
                self.prep.send(DeckCommand::Seek(f));
            }
            if let Some(frame) = ev.jump {
                self.prep.send(DeckCommand::Jump(frame));
            }
            if let Some(l) = ev.set_loop {
                self.prep.send(DeckCommand::SetLoop(l));
            }
            // after the jump and the loop, so it starts from the right place
            if ev.play && !self.prep.shared.is_playing.load(Ordering::Relaxed) {
                self.prep_toggle_play();
            }
            if ev.save {
                self.save_prepare();
            }
            // "Save and close" stays open when the save failed, so nothing is lost without a word
            let unsaved = ev.save && self.prepare.as_ref().is_some_and(|p| p.dirty());
            if ev.close && !unsaved {
                self.close_prepare();
            }
        }
        if let Some(p) = &self.prepare {
            p.sync_click(&self.monitor.click);
        }
        if self.monitor.prep_exited.swap(false, Ordering::Relaxed) && self.prepare.is_some() {
            self.close_prepare();
            self.load_flash_msg = "A deck started, so Prepare was closed. Open it again when you are ready.".into();
            self.load_flash_timer = 4.0;
        }

        if self.prefs.audio_apply {
            self.prefs.audio_apply = false;
            self.restart_audio();
        }

        if let Some(idx) = open_file_for {
            self.open_file(idx);
        }

        if let Some(d) = analyze_stems_for {
            self.analyze_stems(d);
        }

        for (stem_idx, entry) in stem_gain_changes.into_iter().enumerate() {
            if let Some((deck_idx, gain)) = entry {
                self.decks[deck_idx].send(DeckCommand::SetStemGain(stem_idx, gain));
            }
        }

        for i in 0..2 {
            let cmds = self.deck_views[i].drain_commands();
            for cmd in cmds {
                match cmd {
                    DeckCommand::Play => self.request_play(i),
                    other => self.decks[i].send(other),
                }
            }
        }
        for i in 0..2 {
            let ev = self.deck_views[i].take_events();
            if let Some(edit) = ev.grid_edit {
                self.apply_grid_edit(i, edit);
            }
            if let Some(t) = ev.tempo_target {
                self.decks[i].set_target_bpm(t);
                self.release_sync(i); // moving the tempo fader leaves sync, keeping the tempo
            }
            if ev.sync_click {
                self.toggle_sync(i);
            }
            if ev.key_lock_click {
                self.toggle_key_lock(i);
            }
            if let Some(delta) = ev.key_shift {
                let now = if delta == 0 { 0 } else { self.key_shift[i] + delta };
                self.set_key_shift(i, now);
            }
            if ev.key_sync_click {
                self.key_sync(i);
            }
            if ev.prepare {
                if let Some(p) = self.decks[i].track_path.clone() {
                    self.start_prepare(p);
                }
            }
            for a in ev.actions {
                self.handle_action(i, a);
            }
        }
        self.update_sync();

        // Always repaint: keeps MIDI events and waveform playhead responsive.
        ctx.request_repaint();
    }
}

// ── Mixer ──────────────────────────────────────────────────────────────────────

impl DubplateApp {
    /// Pure mixing controls laid out in explicit rects so nothing can drift or overflow:
    /// two centred channel columns (CH label, GAIN/HI/MID/LO/FILTER, mode dropdown)
    /// with a centred crossfader anchored below them.
    fn show_mixer(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        use egui::{pos2, vec2, Align, Layout, Rect, UiBuilder};

        let inner = rect.shrink2(vec2(12.0, 8.0));
        let vol_w = 34.0_f32;
        let side = vol_w + 6.0;
        let knob_area_w = inner.width() - 2.0 * side;
        let knob_left = inner.left() + side;
        let col_w = 90.0_f32.min((knob_area_w - 8.0) / 2.0);
        let gap = knob_area_w - col_w * 2.0;
        let cf_h = 66.0;
        let cols_h = inner.height();
        let mut cols_bottom = inner.top();

        let mut pregain_changed: [Option<f32>; 2] = [None; 2];
        let mut auto_toggled: [bool; 2] = [false; 2];
        let mut eq_changed: [[Option<f32>; 3]; 2] = [[None; 3]; 2];
        let mut filt_changed: [Option<f32>; 2] = [None; 2];
        let mut mode_changed: [Option<u8>; 2] = [None; 2];

        for d in 0..2usize {
            let col = Rect::from_min_size(pos2(knob_left + d as f32 * (col_w + gap), inner.top()), vec2(col_w, cols_h));
            let accent = palette::deck(d);
            let [mut hi, mut mid, mut lo] = self.eq[d];
            let mut fv = self.filter[d];
            let mut pg = self.pregain[d];
            let mode = self.filter_mode[d];

            let col_resp = ui.allocate_new_ui(
                UiBuilder::new().max_rect(col).layout(Layout::top_down(Align::Center)),
                |ui| {
                    ui.set_clip_rect(col.intersect(ui.clip_rect()));
                    ui.spacing_mut().item_spacing.y = 2.0;

                    ui.label(egui::RichText::new(if d == 0 { "CH A" } else { "CH B" })
                        .size(crate::theme::sz(11.0)).color(accent).monospace().strong());
                    ui.add_space(2.0);

                    if knob(ui, &mut pg,  0.0, 2.0, 1.0, "GAIN", accent, KNOB_R_MIXER).changed() { pregain_changed[d] = Some(pg); }
                    {
                        let measured = self.decks[d].track.as_ref().map(|t| t.loudness.is_some());
                        let on = self.auto_gain[d];
                        let label = if on && measured == Some(true) { format!("AUTO {:+.1}", self.trim_db[d]) } else { "AUTO".to_string() };
                        let r = ui.selectable_label(on, egui::RichText::new(label).size(crate::theme::sz(9.0)).monospace());
                        if r.clicked() { auto_toggled[d] = true; }
                        r.on_hover_text(match (on, self.decks[d].track.as_ref().and_then(|t| t.loudness)) {
                            (true, Some(m)) => format!("Auto gain is on: this track measures {:.1} LUFS (true peak {:.1} dB), so it is trimmed {:+.1} dB to match the target of {:.0} LUFS. Click to turn off.",
                                m.lufs, m.true_peak_db, self.trim_db[d], dubplate_engine::loudness::TARGET_LUFS),
                            (true, None) => "Auto gain is on, but this track has not been measured (it may be silent).".to_string(),
                            (false, _) => "Auto gain is off. Click to level this track to the same loudness as the others.".to_string(),
                        });
                    }
                    if knob(ui, &mut hi,  0.0, 2.0, 1.0, "HI",   accent, KNOB_R_MIXER).changed() { eq_changed[d][0] = Some(hi); }
                    if knob(ui, &mut mid, 0.0, 2.0, 1.0, "MID",  accent, KNOB_R_MIXER).changed() { eq_changed[d][1] = Some(mid); }
                    if knob(ui, &mut lo,  0.0, 2.0, 1.0, "LO",   accent, KNOB_R_MIXER).changed() { eq_changed[d][2] = Some(lo); }
                    if filter_knob(ui, &mut fv, 0, KNOB_R_MIXER).changed() { filt_changed[d] = Some(fv); }

                    ui.add_space(2.0);
                    // Indexed by engine mode id; MODE_ORDER is the dropdown display order.
                    const MODE_LABELS: [&str; 7] =
                        ["LP/HP", "Echo", "Flanger", "Reverb", "Phaser", "Bit Crsh", "Sweep"];
                    const MODE_ORDER: [usize; 7] = [0, 6, 1, 3, 4, 2, 5];
                    let mut local_mode = mode;
                    let cb_w = col_w - 8.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(4.0);
                        egui::ComboBox::from_id_salt(egui::Id::new("fmode").with(d))
                            .width(cb_w)
                            .selected_text(egui::RichText::new(MODE_LABELS[mode as usize]).size(crate::theme::sz(11.0)))
                            .show_ui(ui, |ui| {
                                ui.set_min_width(cb_w);
                                for &i in MODE_ORDER.iter() {
                                    if ui.selectable_value(&mut local_mode, i as u8, MODE_LABELS[i]).changed() {
                                        mode_changed[d] = Some(local_mode);
                                    }
                                }
                            });
                    });
                },
            );
            cols_bottom = cols_bottom.max(col_resp.response.rect.bottom());
        }

        // ── Channel volume faders on the outer sides: [vol A][ch A][ch B][vol B] ──
        let vol_h = (cols_bottom - inner.top()).max(60.0);
        let mut cue_clicked = [false; 2];
        for d in 0..2usize {
            let x = if d == 0 { inner.left() } else { inner.right() - vol_w };
            let r = Rect::from_min_size(pos2(x, inner.top()), vec2(vol_w, vol_h));
            let accent = palette::deck(d);
            let mut gain = self.decks[d].shared.gain();
            let mut changed = false;
            ui.allocate_new_ui(UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Center)), |ui| {
                ui.set_clip_rect(r.intersect(ui.clip_rect()));
                ui.spacing_mut().item_spacing.y = 3.0;
                let cue_on = self.monitor.cue[d].load(Ordering::Relaxed);
                let cue_btn = egui::Button::new(
                    egui::RichText::new("CUE").size(crate::theme::sz(10.0)).monospace().color(if cue_on {
                        palette::background()
                    } else {
                        palette::text_dim()
                    }),
                )
                .fill(if cue_on { accent } else { palette::raised() })
                .min_size(vec2(vol_w - 6.0, 20.0));
                if ui
                    .add(cue_btn)
                    .on_hover_text("Headphone cue: hear this deck in your headphones, before its fader")
                    .clicked()
                {
                    cue_clicked[d] = true;
                }
                ui.label(
                    egui::RichText::new("VOL").size(crate::theme::sz(10.0)).color(palette::text_dim()).monospace(),
                );
                changed =
                    vertical_fader(ui, &mut gain, 0.0, 1.5, (vol_h - 60.0).max(40.0), "", accent, Some(1.0)).changed();
                ui.label(
                    egui::RichText::new(format!("{:.0}%", gain * 100.0))
                        .size(crate::theme::sz(10.0))
                        .color(palette::text_dim())
                        .monospace(),
                );
            });
            if changed {
                self.decks[d].send(DeckCommand::SetGain(gain));
            }
        }

        for d in 0..2 {
            if cue_clicked[d] {
                self.toggle_cue(d);
            }
            if auto_toggled[d] {
                self.toggle_auto_gain(d);
            }
            if let Some(g) = pregain_changed[d] {
                self.pregain[d] = g;
                self.decks[d].send(DeckCommand::SetPregain(g));
            }
            if let Some(v) = eq_changed[d][0] {
                self.eq[d][0] = v;
                self.decks[d].send(DeckCommand::SetEqHi(v));
            }
            if let Some(v) = eq_changed[d][1] {
                self.eq[d][1] = v;
                self.decks[d].send(DeckCommand::SetEqMid(v));
            }
            if let Some(v) = eq_changed[d][2] {
                self.eq[d][2] = v;
                self.decks[d].send(DeckCommand::SetEqLo(v));
            }
            if let Some(v) = filt_changed[d] {
                self.filter[d] = v;
                self.decks[d].send(DeckCommand::SetFilter(v));
            }
            if let Some(m) = mode_changed[d] {
                self.filter_mode[d] = m;
                self.filter[d] = 0.5;
                self.decks[d].send(DeckCommand::SetEffectMode(m));
                self.decks[d].send(DeckCommand::SetFilter(0.5));
            }
        }

        // ── Crossfader: centred, directly under the filter dropdowns ────────
        let cf_rect = Rect::from_min_size(pos2(inner.left(), cols_bottom + 10.0), vec2(inner.width(), cf_h));
        ui.painter().line_segment(
            [pos2(inner.left(), cf_rect.top() - 4.0), pos2(inner.right(), cf_rect.top() - 4.0)],
            egui::Stroke::new(1.0_f32, palette::divider()),
        );
        ui.allocate_new_ui(
            UiBuilder::new().max_rect(cf_rect).layout(Layout::top_down(Align::Center)),
            |ui| {
                ui.set_clip_rect(cf_rect.intersect(ui.clip_rect()));
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(egui::RichText::new("A  ◀  CROSSFADER  ▶  B")
                    .size(crate::theme::sz(10.0)).color(palette::text_dim()).monospace());
                let mut cf = self.crossfader_val();
                let r = horizontal_fader(ui, &mut cf, (knob_area_w - 16.0).max(60.0), palette::deck_a());
                if r.changed() { self.set_crossfader(cf); }

                // One row: the crossfader curve and master-in-headphones.
                // both decks are at full volume in the centre either way.
                let fast = self.crossfader_curve.load(Ordering::Relaxed) == dubplate_engine::CF_FAST;
                let label = if fast { "FAST CURVE" } else { "SMOOTH CURVE" };
                let master_on = self.monitor.master.load(Ordering::Relaxed);
                let hp = self._audio.monitor_active;
                let mut curve_clicked = false;
                let mut master_clicked = false;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_space(((knob_area_w - 190.0) / 2.0).max(0.0));
                    curve_clicked = ui.add(egui::Button::new(egui::RichText::new(label).size(crate::theme::sz(9.0)).color(palette::text_dim()).monospace())
                        .small().frame(false))
                        .on_hover_text("Both decks play at full volume at the centre.\nSmooth: the far deck fades gradually.\nFast: wide plateau, then a sharp cut at the ends (scratching).")
                        .clicked();
                    let btn = egui::Button::new(egui::RichText::new("MASTER ▶ HP").size(crate::theme::sz(9.0)).monospace()
                        .color(if master_on { palette::background() } else { palette::text_dim() }))
                        .small()
                        .fill(if master_on { palette::deck_a() } else { palette::raised() });
                    let tip = if hp { "Adds the main mix to your headphones, on top of any cued decks." }
                        else { "No headphone output is set up: choose one under OUTPUT in the top bar." };
                    master_clicked = ui.add(btn).on_hover_text(tip).clicked();
                    // Headphone level: moves when something is cued or master is on.
                    let peak = f32::from_bits(self.monitor.peak.load(Ordering::Relaxed)).clamp(0.0, 1.0);
                    let (bar, _) = ui.allocate_exact_size(vec2(40.0, 8.0), egui::Sense::hover());
                    ui.painter().rect_filled(bar, 2.0, palette::raised());
                    let mut fill = bar;
                    fill.set_right(bar.left() + bar.width() * peak.sqrt());
                    ui.painter().rect_filled(fill, 2.0, palette::deck_a());
                    ui.ctx().request_repaint();
                });
                if curve_clicked {
                    let next = if fast { dubplate_engine::CF_SMOOTH } else { dubplate_engine::CF_FAST };
                    self.crossfader_curve.store(next, Ordering::Relaxed);
                }
                if master_clicked { self.toggle_master_monitor(); }
            },
        );
    }

    /// Thin menu bar at the very top: app name, REC, MIDI status, audio output.
    fn show_menubar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menubar")
            .exact_height(MENU_BAR_H)
            .frame(egui::Frame::none()
                .fill(palette::inset())
                .inner_margin(egui::Margin::symmetric(8.0, 5.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // App identity
                    ui.label(egui::RichText::new("DUBPLATE")
                        .size(crate::theme::sz(15.0)).color(palette::deck_a()).strong());
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Recording
                    let is_recording = self.record_session.is_some();
                    if is_recording {
                        let elapsed = self.record_start.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                        let m = elapsed / 60;
                        let s = elapsed % 60;
                        let pulse = ((ui.input(|i| i.time) * 2.0).sin() * 0.5 + 0.5) as f32;
                        let red = egui::Color32::from_rgb(255, (60.0 + pulse * 40.0) as u8, 60);
                        ui.label(egui::RichText::new(format!("● REC  {m}:{s:02}"))
                            .size(crate::theme::sz(13.0)).color(red).monospace().strong());
                        ui.add_space(4.0);
                        if ui.add(egui::Button::new(
                            egui::RichText::new("■ STOP").size(crate::theme::sz(13.0)).color(palette::text_bright()))
                            .fill(palette::record().linear_multiply(0.8))).clicked() {
                            if let Some(mut session) = self.record_session.take() {
                                session.stop(&self.record_sink);
                                log::info!("recording stopped");
                            }
                            self.record_start = None;
                        }
                    } else if ui.add(egui::Button::new(
                        egui::RichText::new("● REC").size(crate::theme::sz(13.0)).color(palette::text_bright()))
                        .fill(palette::record())).clicked() {
                        let path = default_recording_path();
                        match start_recording(path.clone(), self._audio.sample_rate, &self.record_sink) {
                            Ok(session) => {
                                log::info!("recording started → {}", path.display());
                                self.record_session = Some(session);
                                self.record_start   = Some(std::time::Instant::now());
                            }
                            Err(e) => log::error!("recording failed: {e}"),
                        }
                    }

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // MIDI status — click to open Preferences
                    let active = self.midi.ports.iter().find(|p| p.profile_name.is_some());
                    let (midi_text, midi_col) = match active {
                        Some(p) => (p.profile_name.clone().unwrap_or_default(), palette::text()),
                        None if self.midi.ports.is_empty() => ("no controller".to_owned(), palette::text_dim()),
                        None => ("controller needs a profile".to_owned(), palette::warn()),
                    };
                    let midi_text = if midi_text.chars().count() > 34 {
                        format!("{}…", midi_text.chars().take(33).collect::<String>())
                    } else { midi_text };
                    if ui.add(egui::Button::new(
                        egui::RichText::new(format!("MIDI  {midi_text}")).size(crate::theme::sz(13.0)).color(midi_col))
                        .frame(false))
                        .on_hover_text("MIDI controller settings")
                        .clicked()
                    {
                        self.prefs.open = true;
                    }

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.menu_button(egui::RichText::new(format!("WAVE  {}", crate::wave_draw::palette().label())).size(crate::theme::sz(13.0)), |ui| {
                        for p in crate::wave_draw::Palette::ALL {
                            let chosen = crate::wave_draw::palette() == p;
                            if ui.selectable_label(chosen, format!("{}   {}", p.label(), p.hint())).clicked() {
                                crate::wave_draw::set_palette(p);
                                crate::wave_draw::save_settings();
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        let mut lanes = crate::wave_draw::lanes_default();
                        if ui.checkbox(&mut lanes, "Stem lanes on by default").on_hover_text("Each deck shows a row per stem (drums, bass, other, vocals) under its waveform once the track has stems. A deck's own LANES button overrides this.").changed() {
                            crate::wave_draw::set_lanes_default(lanes);
                            crate::wave_draw::save_settings();
                        }
                    }).response.on_hover_text("How the waveform is coloured");

                    #[cfg(feature = "vst3-host")]
                    {
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);
                        self.master_fx.menubar(ui, &self.monitor.insert);
                        for d in 0..2 {
                            self.deck_fx[d].menubar(ui, &self.monitor.deck_inserts[d]);
                        }
                    }

                    // Audio output (right-aligned): click to choose devices in Preferences.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = &self.selected_device;
                        let short = if name.chars().count() > 30 {
                            format!("…{}", name.chars().skip(name.chars().count() - 27).collect::<String>())
                        } else { name.clone() };
                        let hp = if self._audio.monitor_active { "  +  headphones" } else { "" };
                        if ui.add(egui::Button::new(
                            egui::RichText::new(format!("OUTPUT  {short}{hp}")).size(crate::theme::sz(13.0)).color(palette::text()))
                            .frame(false))
                            .on_hover_text("Choose the output and headphone devices")
                            .clicked()
                        {
                            self.prefs.open = true;
                        }
                    });
                });
            });
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn tempo_matches_directly_when_in_range() {
        // 174 track, master at 170: within 161..187
        assert_eq!(pick_sync_tempo(170.0, 161.0, 187.0, 174.0), (170.0, false));
    }

    #[test]
    fn tempo_uses_double_or_half_when_out_of_range() {
        // 87 BPM track (range 80..93) following a 174 master -> 87, not clamped to 93
        assert_eq!(pick_sync_tempo(174.0, 80.0, 93.0, 87.0), (87.0, false));
        // 85 BPM track (78..91) following a 170 master -> 85
        assert_eq!(pick_sync_tempo(170.0, 78.0, 91.0, 85.0), (85.0, false));
        // 174 track following an 87 master -> 174
        assert_eq!(pick_sync_tempo(87.0, 161.0, 187.0, 174.0), (174.0, false));
    }

    #[test]
    fn tempo_is_flagged_limited_when_nothing_fits() {
        // 128 track (118..138) following a 100 master: nothing fits (100, 200, 50)
        let (t, limited) = pick_sync_tempo(100.0, 118.0, 138.0, 128.0);
        assert!(limited);
        assert!((118.0..=138.0).contains(&t));
    }

    #[test]
    fn stopped_deck_waits_for_the_masters_matching_phrase_position() {
        // Deck sits at beat 0 (phrase start); master is at beat 5.5 of its phrase.
        // The master reaches beat 16 (== 0 mod 16) in 10.5 beats.
        let f = phrase_start_delay_frames(0.0, 5.5, 16.0, 120.0, 48_000.0).unwrap();
        let expected = (10.5 * 0.5 * 48_000.0) as u64; // 0.5 s per beat at 120 BPM
        assert!((f as i64 - expected as i64).abs() <= 1, "{f} vs {expected}");
    }

    #[test]
    fn deck_cued_mid_phrase_keeps_its_phrase_offset() {
        // Deck cued at beat 8; master at beat 24.0 (== 8 mod 16): already aligned.
        assert_eq!(phrase_start_delay_frames(8.0, 24.0, 16.0, 128.0, 44_100.0), None);
        // Master at beat 3 -> wait 5 beats to reach 8.
        let f = phrase_start_delay_frames(8.0, 3.0, 16.0, 120.0, 44_100.0).unwrap();
        assert_eq!(f, (5.0 * 0.5 * 44_100.0) as u64);
    }

    #[test]
    fn negative_beats_before_the_anchor_wrap_correctly() {
        // Deck at beat -0.6 (intro before downbeat), master at 9.4: same phrase position.
        assert_eq!(phrase_start_delay_frames(-0.6, 15.4, 16.0, 120.0, 44_100.0), None);
    }
}

#[cfg(test)]
mod deck_pick_tests {
    use super::pick_deck;

    #[test]
    fn a_new_track_goes_to_a_deck_that_is_not_playing() {
        // (loaded, playing) for A and B
        assert_eq!(pick_deck([(false, false), (false, false)]), 0, "both empty: A");
        assert_eq!(pick_deck([(true, false), (false, false)]), 1, "A has a track, B is empty");
        assert_eq!(pick_deck([(false, false), (true, true)]), 0, "B is playing, A is empty: A");
        assert_eq!(pick_deck([(true, false), (true, true)]), 0, "B is playing, A is loaded but stopped: A, not B");
        assert_eq!(pick_deck([(true, true), (true, false)]), 1, "A is playing: B");
        assert_eq!(pick_deck([(true, false), (true, false)]), 1, "both idle: B, as before");
        assert_eq!(pick_deck([(true, true), (false, false)]), 1, "A is playing and B is empty: B");
        assert_eq!(pick_deck([(true, true), (true, true)]), 1, "both playing: B, refused by the usual message");
    }
}
