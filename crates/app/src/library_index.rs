//! Background library indexer: walks the watched folders, reads tags, then analyses
//! new tracks (tempo, key, waveform preview) on background threads so the UI never blocks.

use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, UNIX_EPOCH};

use crate::db::LibraryDb;

pub static AUDIO_EXTS: &[&str] = &["mp3", "flac", "wav", "aiff", "aif", "m4a", "ogg", "opus"];
const PREVIEW_COLS: usize = 160;
const MAX_DEPTH: usize = 16;

pub fn is_audio(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).map(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str())).unwrap_or(false)
}

/// Audio files below `dir` (recursive; hidden folders and symlinked folders skipped), publishing how many it has found so
/// far, so a slow share shows progress.
fn walk_audio_counting(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, found: Option<&AtomicUsize>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        let hidden = entry.file_name().to_str().map(|n| n.starts_with('.')).unwrap_or(false);
        if ft.is_dir() {
            if !hidden && !ft.is_symlink() {
                walk_audio_counting(&path, out, depth + 1, found);
            }
        } else if is_audio(&path) {
            out.push(path);
            if let Some(f) = found {
                f.store(f.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Default)]
pub struct IndexStatus {
    pub busy: AtomicBool,
    pub total: AtomicUsize,
    pub done: AtomicUsize,
    /// Looking through the watched folders for new or changed files: `found` audio files so far, `checked` of them
    /// compared with the database. A network share can take minutes, so the screen says so.
    pub scanning: AtomicBool,
    pub found: AtomicUsize,
    pub checked: AtomicUsize,
    /// Analyse the whole library in the background. Off: a track is analysed when it first goes on a deck.
    pub auto: AtomicBool,
    /// A deck is playing: analysis keeps to one thread until it stops.
    pub live: AtomicBool,
}

fn auto_file() -> PathBuf {
    dubplate_engine::paths::config_dir().join("auto_analyse")
}

/// On unless the user switched it off.
pub fn load_auto() -> bool {
    std::fs::read_to_string(auto_file()).map_or(true, |s| s.trim() != "0")
}

pub fn save_auto(on: bool) {
    let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
    let _ = std::fs::write(auto_file(), if on { "1" } else { "0" });
}

pub enum IndexEvent {
    /// Rows in the database changed; reload the view.
    Changed,
}

enum Cmd {
    /// The program has just started: look through the folders only if it has been a while.
    Startup,
    /// Look through the folders for new or changed files, then analyse what is waiting (Rescan, a new folder).
    Rescan,
    /// Only analyse what is waiting (Auto-analyse switched on).
    Pending,
}

/// How long the folders are trusted between launches. New files are found at the next scan after this, or at once with
/// **Rescan**; a library on a network share of tens of thousands of tracks takes minutes to walk.
const WALK_EVERY_SECS: i64 = 6 * 3600;
const LAST_WALK_KEY: &str = "last_walk";

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Whether the folders are due to be walked, given when they last were. A clock set back counts as due.
fn walk_due(last: Option<i64>, now: i64) -> bool {
    match last {
        Some(t) => now < t || now - t >= WALK_EVERY_SECS,
        None => true,
    }
}

fn wants_walk(cmd: &Cmd, db: &LibraryDb) -> bool {
    match cmd {
        Cmd::Rescan => true,
        Cmd::Pending => false,
        Cmd::Startup => walk_due(db.meta_get(LAST_WALK_KEY).and_then(|v| v.parse().ok()), now_secs()),
    }
}

pub struct Indexer {
    cmd_tx: mpsc::Sender<Cmd>,
    pub events: mpsc::Receiver<IndexEvent>,
    pub status: Arc<IndexStatus>,
}

impl Indexer {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, events) = mpsc::channel();
        let status = Arc::new(IndexStatus::default());
        status.auto.store(load_auto(), Ordering::Relaxed);
        let st = Arc::clone(&status);
        std::thread::Builder::new().name("library-indexer".into()).spawn(move || run(cmd_rx, ev_tx, st)).ok();
        let _ = cmd_tx.send(Cmd::Startup);
        Self { cmd_tx, events, status }
    }

    /// Look through the folders for new or changed files, then analyse what is waiting.
    pub fn rescan(&self) {
        let _ = self.cmd_tx.send(Cmd::Rescan);
    }

    /// Analyse what is waiting, without walking the folders again.
    pub fn analyse_pending(&self) {
        let _ = self.cmd_tx.send(Cmd::Pending);
    }
}

fn file_stamp(path: &Path) -> (i64, i64) {
    std::fs::metadata(path)
        .ok()
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            (mtime, m.len() as i64)
        })
        .unwrap_or((0, 0))
}

fn run(rx: mpsc::Receiver<Cmd>, tx: mpsc::Sender<IndexEvent>, status: Arc<IndexStatus>) {
    // nobody is waiting on this: it gets the processor when the audio and the screen do not want it (the workers
    // started from here inherit that)
    dubplate_engine::threads::background();
    let Ok(db) = LibraryDb::open() else {
        log::warn!("indexer: library DB unavailable");
        return;
    };
    while let Ok(first) = rx.recv() {
        let mut walk = wants_walk(&first, &db);
        while let Ok(more) = rx.try_recv() {
            walk |= wants_walk(&more, &db); // coalesce bursts
        }
        status.busy.store(true, Ordering::Relaxed);
        while let Some(again) = rescan(&db, &rx, &tx, &status, walk) {
            walk = again;
        }
        status.busy.store(false, Ordering::Relaxed);
        let _ = tx.send(IndexEvent::Changed);
    }
}

/// Returns `Some(walk)` when a new scan was requested part-way and the work should start over (walking the folders again
/// if that request wanted it).
fn rescan(
    db: &LibraryDb,
    rx: &mpsc::Receiver<Cmd>,
    tx: &mpsc::Sender<IndexEvent>,
    status: &IndexStatus,
    walk: bool,
) -> Option<bool> {
    if walk {
        index_folders(db, tx, status);
    }
    analyse_pending(db, rx, tx, status)
}

/// 1. Index files (cheap: tags only, and only for new / changed files).
fn index_folders(db: &LibraryDb, tx: &mpsc::Sender<IndexEvent>, status: &IndexStatus) {
    status.found.store(0, Ordering::Relaxed);
    status.checked.store(0, Ordering::Relaxed);
    status.scanning.store(true, Ordering::Relaxed);
    let known = db.known_files();
    let mut files = Vec::new();
    for src in db.sources() {
        walk_audio_counting(&src, &mut files, 0, Some(&status.found));
    }
    status.found.store(files.len(), Ordering::Relaxed);
    let mut added = 0usize;
    for (i, path) in files.iter().enumerate() {
        status.checked.store(i + 1, Ordering::Relaxed);
        let (mtime, size) = file_stamp(path);
        if known.get(path) == Some(&(mtime, size)) {
            continue;
        }
        let tags = dubplate_engine::read_tags(path);
        if let Err(e) = db.index_file(path, &tags, mtime, size) {
            log::warn!("indexing {} failed: {e}", path.display());
        }
        added += 1;
        if added.is_multiple_of(50) {
            let _ = tx.send(IndexEvent::Changed);
        }
    }
    status.scanning.store(false, Ordering::Relaxed);
    db.meta_set(LAST_WALK_KEY, &now_secs().to_string());
    let _ = tx.send(IndexEvent::Changed);
}

/// 2. Analyse what has not been analysed yet.
fn analyse_pending(
    db: &LibraryDb,
    rx: &mpsc::Receiver<Cmd>,
    tx: &mpsc::Sender<IndexEvent>,
    status: &IndexStatus,
) -> Option<bool> {
    if !status.auto.load(Ordering::Relaxed) {
        status.total.store(0, Ordering::Relaxed);
        return None;
    }

    // 2. Analyse what has not been analysed yet, on several threads. The counter shows progress through
    //    the whole library, so a restart picks up where it left off instead of looking like it began again.
    let pending: Vec<PathBuf> = db.pending_analysis().into_iter().collect();
    let library = db.known_files().len();
    let already = library.saturating_sub(pending.len());
    status.total.store(library, Ordering::Relaxed);
    status.done.store(already, Ordering::Relaxed);
    if pending.is_empty() {
        return None;
    }

    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).div_ceil(2).clamp(3, 6); // a network share leaves them waiting, but the UI and audio need cores too
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let mut restart = None;
    let mut last_refresh = std::time::Instant::now();
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let (next, stop, pending) = (&next, &stop, &pending);
            scope.spawn(move || loop {
                // during a set one thread carries on and the rest wait: decoding a track fills the memory caches
                // and the disk queue the playing decks are using, whatever its priority
                while worker > 0 && status.live.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= pending.len() || stop.load(Ordering::Relaxed) {
                    break;
                }
                analyse_one(db, &pending[i]);
                status.done.fetch_add(1, Ordering::Relaxed);
            });
        }
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if let Ok(first) = rx.try_recv() {
                stop.store(true, Ordering::Relaxed);
                let mut walk = wants_walk(&first, db);
                while let Ok(more) = rx.try_recv() {
                    walk |= wants_walk(&more, db);
                }
                restart = Some(walk);
                break;
            }
            if !status.auto.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            if last_refresh.elapsed() > Duration::from_secs(5) {
                last_refresh = std::time::Instant::now();
                let _ = tx.send(IndexEvent::Changed);
            }
            if status.done.load(Ordering::Relaxed) >= already + pending.len() {
                break;
            }
        }
    });
    restart
}

fn analyse_one(db: &LibraryDb, path: &Path) {
    match dubplate_engine::decode::decode_file(path) {
        Ok(t) => {
            let preview = t.wave.preview(PREVIEW_COLS);
            let key = t.key.map(|k| (k.tonic, k.minor, k.confidence));
            if let Err(e) = db.save_analysis(path, t.bpm, t.duration_secs, key, &preview) {
                log::warn!("saving analysis for {} failed: {e}", path.display());
            }
        }
        Err(e) => {
            log::warn!("analysis of {} failed: {e}", path.display());
            // Mark a broken file as done so it is not retried on every scan - but only if the file is
            // really there; a share that dropped out for a moment must not blank the track for good.
            if path.metadata().is_ok_and(|m| m.len() > 0) {
                let _ = db.save_analysis(path, 0.0, 0.0, None, &[]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_extensions_are_case_insensitive() {
        assert!(is_audio(Path::new("/a/b.MP3")));
        assert!(is_audio(Path::new("c.flac")));
        assert!(!is_audio(Path::new("notes.txt")));
        assert!(!is_audio(Path::new("noext")));
    }

    #[test]
    fn the_folders_are_walked_on_first_run_and_after_a_while_but_not_at_every_launch() {
        assert!(walk_due(None, 1_000_000));
        assert!(!walk_due(Some(1_000_000), 1_000_000 + 60));
        assert!(!walk_due(Some(1_000_000), 1_000_000 + WALK_EVERY_SECS - 1));
        assert!(walk_due(Some(1_000_000), 1_000_000 + WALK_EVERY_SECS));
        assert!(walk_due(Some(1_000_000), 999_000), "a clock set back is not trusted");
    }

    #[test]
    fn walk_finds_nested_audio_and_skips_hidden_and_non_audio() {
        let root = std::env::temp_dir().join(format!("dubplate_walk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        for f in ["a.mp3", "sub/b.flac", "sub/deeper/c.ogg", "sub/readme.txt", ".hidden/d.mp3"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let mut found = Vec::new();
        let counted = AtomicUsize::new(0);
        walk_audio_counting(&root, &mut found, 0, Some(&counted));
        assert_eq!(counted.load(Ordering::Relaxed), 3);
        let mut names: Vec<_> = found.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        names.sort();
        assert_eq!(names, vec!["a.mp3", "b.flac", "c.ogg"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
