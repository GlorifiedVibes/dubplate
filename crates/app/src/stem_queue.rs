//! Separating stems, one track at a time, in the order that matters.
//!
//! A deck that asks for its stems goes first and at once: if a background job is running it is told to stop (it stops
//! between two segments of the track, about a second) and goes back to the front of the line. When both decks ask,
//! deck A is done first, then B, never both together. Tracks in a Reel are separated in the background when the track
//! has a bar line in doubt (see `dubplate_engine::decode::wants_stem_note`) and no grid the user saved; only a few
//! numbers are kept for them, not the 200-600 MB of stems.
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};

use dubplate_engine::deck::DecodedTrack;
use dubplate_engine::stems::TrackStems;

pub type StemResult = (usize, Result<Arc<TrackStems>, String>);

/// A deck waiting for its stems.
pub struct DeckJob {
    pub deck: usize,
    pub track: Arc<DecodedTrack>,
    pub path: PathBuf,
    pub progress: Arc<AtomicU32>,
    pub tx: mpsc::SyncSender<StemResult>,
}

/// What the worker should do next.
pub enum Next {
    Deck(DeckJob),
    Reel(PathBuf),
}

/// The line itself, without threads, so the order can be tested.
#[derive(Default)]
pub struct Plan {
    decks: VecDeque<DeckJob>,
    reel: VecDeque<PathBuf>,
    queued: HashSet<PathBuf>,
}

impl Plan {
    /// A deck goes to the back of the deck line (A before B when both ask), ahead of every Reel job.
    pub fn push_deck(&mut self, job: DeckJob) {
        // a deck asking again for the same deck replaces its earlier request
        self.decks.retain(|j| j.deck != job.deck);
        self.decks.push_back(job);
    }

    /// A Reel track goes to the back of the Reel line unless it is there already.
    pub fn push_reel(&mut self, path: PathBuf) {
        if self.queued.insert(path.clone()) {
            self.reel.push_back(path);
        }
    }

    /// A Reel job that was stopped for a deck goes back to the front.
    pub fn requeue_reel_first(&mut self, path: PathBuf) {
        if self.queued.insert(path.clone()) {
            self.reel.push_front(path);
        }
    }

    /// The next job: a deck's, else a Reel track's when `reel_on`.
    pub fn next(&mut self, reel_on: bool) -> Option<Next> {
        if let Some(j) = self.decks.pop_front() {
            return Some(Next::Deck(j));
        }
        if reel_on {
            let p = self.reel.pop_front()?;
            self.queued.remove(&p);
            return Some(Next::Reel(p));
        }
        None
    }

    pub fn reel_waiting(&self) -> usize {
        self.reel.len()
    }

    pub fn deck_waiting(&self) -> bool {
        !self.decks.is_empty()
    }
}

/// What the library toolbar shows.
#[derive(Default)]
pub struct Status {
    /// Background Reel tracks waiting, and the one being separated.
    pub waiting: AtomicUsize,
    pub running_reel: AtomicBool,
    /// Background separation on or off. On unless the user switched it off.
    pub reel_on: AtomicBool,
}

struct Inner {
    plan: Mutex<Plan>,
    wake: Condvar,
    /// The flag of the Reel job that is running, if one is.
    running_reel: Mutex<Option<Arc<AtomicBool>>>,
    status: Status,
}

pub struct StemQueue {
    inner: Arc<Inner>,
}

fn reel_file() -> PathBuf {
    dubplate_engine::paths::config_dir().join("reel_stems")
}

/// On unless the user switched it off.
pub fn load_reel_on() -> bool {
    std::fs::read_to_string(reel_file()).map_or(true, |s| s.trim() != "0")
}

pub fn save_reel_on(on: bool) {
    let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
    let _ = std::fs::write(reel_file(), if on { "1" } else { "0" });
}

static QUEUE: OnceLock<StemQueue> = OnceLock::new();

/// The one queue of the program, started the first time it is used.
pub fn queue() -> &'static StemQueue {
    QUEUE.get_or_init(StemQueue::start)
}

impl StemQueue {
    fn start() -> StemQueue {
        let inner = Arc::new(Inner {
            plan: Mutex::new(Plan::default()),
            wake: Condvar::new(),
            running_reel: Mutex::new(None),
            status: Status::default(),
        });
        inner.status.reel_on.store(load_reel_on(), Ordering::Relaxed);
        let worker = Arc::clone(&inner);
        std::thread::Builder::new().name("stem-queue".into()).spawn(move || work(worker)).expect("stem queue thread");
        StemQueue { inner }
    }

    pub fn status(&self) -> &Status {
        &self.inner.status
    }

    /// A deck wants its stems now. A background job that is running is asked to stop.
    pub fn request_deck(&self, job: DeckJob) {
        self.inner.plan.lock().unwrap().push_deck(job);
        if let Some(flag) = self.inner.running_reel.lock().unwrap().as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        self.inner.wake.notify_one();
    }

    /// A track went onto a Reel (or was on one when the program started).
    pub fn add_reel_track(&self, path: PathBuf) {
        let mut plan = self.inner.plan.lock().unwrap();
        plan.push_reel(path);
        self.inner.status.waiting.store(plan.reel_waiting(), Ordering::Relaxed);
        drop(plan);
        self.inner.wake.notify_one();
    }

    pub fn set_reel_on(&self, on: bool) {
        self.inner.status.reel_on.store(on, Ordering::Relaxed);
        save_reel_on(on);
        if on {
            self.inner.wake.notify_one();
        } else if let Some(flag) = self.inner.running_reel.lock().unwrap().as_ref() {
            flag.store(true, Ordering::Relaxed); // stop the one in progress too
        }
    }
}

fn work(inner: Arc<Inner>) {
    // splitting a track takes a processor (or the graphics card) for a long while: it must never take it from the audio
    dubplate_engine::threads::background();
    let db = crate::db::LibraryDb::open().ok();
    loop {
        let next = {
            let mut plan = inner.plan.lock().unwrap();
            loop {
                let on = inner.status.reel_on.load(Ordering::Relaxed);
                if let Some(n) = plan.next(on) {
                    inner.status.waiting.store(plan.reel_waiting(), Ordering::Relaxed);
                    break n;
                }
                plan = inner.wake.wait(plan).unwrap();
            }
        };
        match next {
            Next::Deck(job) => run_deck(job),
            Next::Reel(path) => run_reel(&inner, db.as_ref(), path),
        }
    }
}

fn run_deck(job: DeckJob) {
    log::info!("stem separation deck {}: {}", job.deck, job.path.display());
    let t = &job.track;
    let result =
        dubplate_engine::stems::separate(&t.samples, t.channels, t.sample_rate, &job.path, Some(&job.progress))
            .map_err(|e| e.0);
    if let Ok(stems) = &result {
        // what the drums say about the bar line is kept for the next time the track is analysed
        let outcome = dubplate_engine::decode::note_drums(&job.path, stems);
        log::info!("drums note for {}: {outcome:?}", job.path.display());
    }
    let _ = job.tx.send((job.deck, result));
}

fn run_reel(inner: &Inner, db: Option<&crate::db::LibraryDb>, path: PathBuf) {
    // a grid the user saved is theirs: leave the track alone
    if db.is_some_and(|d| d.load_grid(&path).is_some()) {
        return;
    }
    let wants = match dubplate_engine::decode::wants_stem_note(&path) {
        Some(w) => w,
        None => {
            // not analysed yet: analyse it (which also makes its cache entry), then ask again
            if dubplate_engine::decode::decode_file(&path).is_err() {
                return;
            }
            dubplate_engine::decode::wants_stem_note(&path).unwrap_or(false)
        }
    };
    if !wants {
        return;
    }
    let Ok(pcm) = dubplate_engine::decode::decode_pcm(&path) else { return };
    let cancel = Arc::new(AtomicBool::new(false));
    *inner.running_reel.lock().unwrap() = Some(Arc::clone(&cancel));
    inner.status.running_reel.store(true, Ordering::Relaxed);
    // a request may have come in while the file was being read
    if inner.plan.lock().unwrap().deck_waiting() || !inner.status.reel_on.load(Ordering::Relaxed) {
        cancel.store(true, Ordering::Relaxed);
    }
    log::info!("background stems: {}", path.display());
    let result = dubplate_engine::stems::separate_uncached(
        &pcm.samples,
        pcm.channels,
        pcm.sample_rate,
        &path,
        None,
        Some(&cancel),
    );
    *inner.running_reel.lock().unwrap() = None;
    inner.status.running_reel.store(false, Ordering::Relaxed);
    match result {
        Ok(stems) => {
            let outcome = dubplate_engine::decode::note_drums(&path, &stems);
            log::info!("drums note for {}: {outcome:?}", path.display());
        }
        Err(e) if dubplate_engine::stems::is_cancelled(&e) => {
            // a deck wants the engine: this one goes back to the front and waits its turn
            let mut plan = inner.plan.lock().unwrap();
            plan.requeue_reel_first(path);
            inner.status.waiting.store(plan.reel_waiting(), Ordering::Relaxed);
        }
        Err(e) => log::warn!("background stems failed for {}: {}", path.display(), e.0),
    }
}

/// Reel tracks that are not yet known to have been asked, for the start of the program.
pub fn enqueue_all_reels(db: &crate::db::LibraryDb) {
    let q = queue();
    for (id, _, _) in db.reels() {
        for p in db.reel_paths(id) {
            q.add_reel_track(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck_job(deck: usize) -> DeckJob {
        let (tx, _rx) = mpsc::sync_channel(1);
        DeckJob {
            deck,
            track: Arc::new(DecodedTrack {
                samples: Vec::new(),
                sample_rate: 44_100,
                channels: 2,
                duration_frames: 0,
                waveform: Vec::new(),
                wave: Arc::new(Default::default()),
                title: String::new(),
                duration_secs: 0.0,
                bpm: 0.0,
                bpm_coarse: 0,
                beat_offset: 0.0,
                beat_times_ms: Vec::new(),
                key: None,
                cues: Vec::new(),
                loudness: None,
            }),
            path: PathBuf::from(format!("deck{deck}")),
            progress: Arc::new(AtomicU32::new(0)),
            tx,
        }
    }

    fn name(n: Option<Next>) -> String {
        match n {
            Some(Next::Deck(j)) => format!("deck {}", j.deck),
            Some(Next::Reel(p)) => p.display().to_string(),
            None => "nothing".into(),
        }
    }

    #[test]
    fn a_deck_goes_before_every_reel_track_and_deck_a_before_b() {
        let mut p = Plan::default();
        p.push_reel("r1".into());
        p.push_reel("r2".into());
        p.push_deck(deck_job(0));
        p.push_deck(deck_job(1));
        assert_eq!(name(p.next(true)), "deck 0");
        assert_eq!(name(p.next(true)), "deck 1");
        assert_eq!(name(p.next(true)), "r1");
        assert_eq!(name(p.next(true)), "r2");
        assert_eq!(name(p.next(true)), "nothing");
    }

    #[test]
    fn a_stopped_reel_job_goes_back_to_the_front_and_a_track_is_queued_once() {
        let mut p = Plan::default();
        p.push_reel("r1".into());
        p.push_reel("r2".into());
        p.push_reel("r1".into()); // already waiting
        assert_eq!(p.reel_waiting(), 2);
        assert_eq!(name(p.next(true)), "r1");
        p.requeue_reel_first("r1".into());
        assert_eq!(name(p.next(true)), "r1");
        assert_eq!(name(p.next(true)), "r2");
    }

    #[test]
    fn with_background_stems_off_only_decks_are_served() {
        let mut p = Plan::default();
        p.push_reel("r1".into());
        assert_eq!(name(p.next(false)), "nothing");
        p.push_deck(deck_job(1));
        assert_eq!(name(p.next(false)), "deck 1");
        assert_eq!(p.reel_waiting(), 1);
        assert_eq!(name(p.next(true)), "r1");
    }

    #[test]
    fn a_deck_that_asks_again_replaces_its_earlier_request() {
        let mut p = Plan::default();
        p.push_deck(deck_job(0));
        p.push_deck(deck_job(0));
        assert_eq!(name(p.next(true)), "deck 0");
        assert_eq!(name(p.next(true)), "nothing");
    }
}
