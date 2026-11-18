//! "Import rekordbox grids": reuses the beat grids rekordbox has already made (and that you may have corrected) for
//! tracks that are also in the Dubplate library. Point it at a rekordbox analysis folder (`PIONEER/USBANLZ` on a stick
//! or in rekordbox's data folder). Each grid is checked against the audio before it is kept.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

use dubplate_engine::anlz_read::{read_dat, RekordboxGrid};
use dubplate_engine::bpm::{self, GridVerdict};
use dubplate_engine::BeatGrid;
use egui::RichText;

use crate::app::palette;
use crate::db::{LibraryDb, TrackRow};

/// What happened to one track.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The rekordbox grid fits the audio and is used. `shift_ms` is the MP3 start offset that was removed, if any.
    Imported { grid: BeatGrid, shift_ms: Option<f64>, scatter_ms: f32 },
    /// Used, but there were too few clear kicks to check it against the audio.
    Unchecked { grid: BeatGrid },
    /// Not used, and why.
    Skipped(String),
}

/// Decides what to do with a rekordbox grid, given the audio's onsets (`bpm::onset_curve_full`).
pub fn judge(rb: &RekordboxGrid, onset: &[f32], frame_secs: f64, sample_rate: u32, own_bpm: f32) -> Outcome {
    let Some(t) = rb.tempo() else { return Outcome::Skipped("no tempo in the rekordbox grid".into()) };
    if t.dynamic {
        return Outcome::Skipped(
            "the tempo changes inside the track (a dynamic grid), which Dubplate can't hold yet".into(),
        );
    }
    let Some(plain) = rb.to_grid(0.0, sample_rate) else { return Outcome::Skipped("no usable grid".into()) };
    let measured = bpm::measure_import_offset(onset, frame_secs, plain.bpm, plain.offset);
    let shift = measured.and_then(|m| bpm::mp3_offset_shift(&m, sample_rate));
    let Some(grid) = rb.to_grid(shift.unwrap_or(0.0), sample_rate) else {
        return Outcome::Skipped("no usable grid".into());
    };
    let q = bpm::grid_quality(onset, frame_secs, grid.bpm, grid.offset);
    match q.verdict {
        GridVerdict::Good | GridVerdict::Check => {
            Outcome::Imported { grid, shift_ms: shift.map(|s| s * 1000.0), scatter_ms: q.spread_ms }
        }
        GridVerdict::Unknown if q.hits < 16 => Outcome::Unchecked { grid },
        _ => {
            let why = if own_bpm > 0.0 && (t.bpm - own_bpm as f64).abs() / own_bpm as f64 > 0.002 {
                format!("its tempo is {:.2} and Dubplate hears {:.2}", t.bpm, own_bpm)
            } else if let Some(m) = measured.filter(|m| m.spread_ms < 8.0) {
                format!("the tempo agrees, but the kicks sit a steady {:+.0} ms from it: probably another edit or encode of the song", m.median_ms)
            } else {
                format!("the kicks are scattered {:.0} ms around it", q.spread_ms)
            };
            Outcome::Skipped(format!("doesn't fit the audio ({why})"))
        }
    }
}

/// Every `ANLZ0000.DAT` under `dir`.
pub fn find_dats(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("ANLZ0000.DAT")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Pairs rekordbox grids with library tracks by file name; when several tracks share the name, the one whose length
/// is closest to the grid's last beat (and within 3 s of it) is taken.
pub fn match_tracks(grids: Vec<RekordboxGrid>, rows: &[TrackRow]) -> (Vec<(RekordboxGrid, &TrackRow)>, usize) {
    let mut by_name: HashMap<String, Vec<&TrackRow>> = HashMap::new();
    for r in rows {
        if let Some(n) = r.path.file_name().and_then(|n| n.to_str()) {
            by_name.entry(n.to_lowercase()).or_default().push(r);
        }
    }
    let (mut pairs, mut missing) = (Vec::new(), 0);
    let mut used: std::collections::HashSet<i64> = Default::default();
    for g in grids {
        let last = g.last_secs();
        let pick = by_name.get(&g.file_name().to_lowercase()).and_then(|c| {
            c.iter()
                .filter(|r| !used.contains(&r.id))
                .filter(|r| r.duration_secs.is_none_or(|d| last <= d + 3.0 && last >= d - 90.0))
                .min_by(|a, b| {
                    let da = a.duration_secs.map_or(1e9, |d| (d - last).abs());
                    let db = b.duration_secs.map_or(1e9, |d| (d - last).abs());
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        match pick {
            Some(r) => {
                used.insert(r.id);
                pairs.push((g, *r));
            }
            None => missing += 1,
        }
    }
    (pairs, missing)
}

enum Msg {
    Scanned { grids: usize, matched: usize, missing: usize },
    Track { path: PathBuf, name: String, outcome: Outcome },
    Finished,
}

#[derive(Default)]
struct Tally {
    imported: usize,
    shifted: usize,
    unchecked: usize,
    kept_yours: usize,
    skipped: Vec<(String, String)>,
    missing: usize,
    grids: usize,
}

enum Stage {
    Ready,
    Running { rx: mpsc::Receiver<Msg>, total: usize, done: usize, cancel: Arc<std::sync::atomic::AtomicBool> },
    Finished,
}

pub struct GridImportDialog {
    dir: PathBuf,
    rows: Vec<TrackRow>,
    replace_mine: bool,
    stage: Stage,
    tally: Tally,
    /// Set once something was saved, so the library list can refresh.
    pub changed: bool,
}

impl GridImportDialog {
    pub fn new(dir: PathBuf, rows: Vec<TrackRow>) -> Self {
        GridImportDialog {
            dir,
            rows,
            replace_mine: false,
            stage: Stage::Ready,
            tally: Tally::default(),
            changed: false,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, db: Option<&LibraryDb>) -> bool {
        self.poll(db);
        let mut open = true;
        let mut start = false;
        egui::Window::new("Import rekordbox grids").open(&mut open).collapsible(false).resizable(false).default_width(500.0).show(ctx, |ui| {
            match &self.stage {
                Stage::Ready => {
                    ui.label(format!("From {}", self.dir.display()));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Reads rekordbox's beat grids for tracks you also have here, so grids you made or corrected there \
                        carry over. Each one is checked against the audio first and left out if it doesn't fit (rekordbox sometimes has \
                        the wrong tempo). Tracks are matched by file name. MP3s that sit a frame or two off because of the encoder delay \
                        are moved onto the music.").size(crate::theme::sz(12.5)).color(palette::text_dim()));
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.replace_mine, "Also replace grids I saved or imported before");
                    ui.add_space(6.0);
                    if ui.button("Import").clicked() { start = true; }
                }
                Stage::Running { total, done, cancel, .. } => {
                    let frac = if *total == 0 { 0.0 } else { *done as f32 / *total as f32 };
                    ui.add(egui::ProgressBar::new(frac).text(format!("{done} / {total}")));
                    ui.label(RichText::new("Checking each grid against its audio. This takes a second or two per track.").size(crate::theme::sz(12.5)).color(palette::text_dim()));
                    if ui.button("Stop").clicked() { cancel.store(true, std::sync::atomic::Ordering::Relaxed); }
                }
                Stage::Finished => {
                    let t = &self.tally;
                    ui.label(format!("{} rekordbox grids found; {} not in your library.", t.grids, t.missing));
                    ui.colored_label(palette::ok(), format!("{} imported ({} of them moved onto the music for an MP3 start offset).", t.imported + t.unchecked, t.shifted));
                    if t.unchecked > 0 { ui.label(RichText::new(format!("{} had too few clear kicks to check; used as rekordbox has them.", t.unchecked)).size(crate::theme::sz(12.0)).color(palette::text_dim())); }
                    if t.kept_yours > 0 { ui.label(format!("{} already had a grid you saved: kept.", t.kept_yours)); }
                    if !t.skipped.is_empty() {
                        ui.colored_label(palette::warn(), format!("{} left out:", t.skipped.len()));
                        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            for (n, why) in t.skipped.iter().take(200) { ui.label(RichText::new(format!("{n} — {why}")).size(crate::theme::sz(12.0)).color(palette::text_dim())); }
                        });
                    }
                }
            }
        });
        if start {
            self.start();
        }
        if matches!(self.stage, Stage::Running { .. }) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        open
    }

    fn start(&mut self) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (dir, rows, replace, flag) = (self.dir.clone(), self.rows.clone(), self.replace_mine, cancel.clone());
        std::thread::spawn(move || {
            let grids: Vec<RekordboxGrid> =
                find_dats(&dir).iter().filter_map(|p| std::fs::read(p).ok()).filter_map(|b| read_dat(&b)).collect();
            let n_grids = grids.len();
            let (pairs, missing) = match_tracks(grids, &rows);
            let _ = tx.send(Msg::Scanned { grids: n_grids, matched: pairs.len(), missing });
            let queue: Arc<Mutex<VecDeque<(RekordboxGrid, TrackRow)>>> =
                Arc::new(Mutex::new(pairs.into_iter().map(|(g, r)| (g, r.clone())).collect()));
            let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(3));
            std::thread::scope(|s| {
                for _ in 0..workers {
                    let (queue, tx, flag) = (queue.clone(), tx.clone(), flag.clone());
                    s.spawn(move || loop {
                        if flag.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let Some((g, row)) = queue.lock().unwrap().pop_front() else { break };
                        let name = row.path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
                        if row.bpm_manual && !replace {
                            let _ = tx.send(Msg::Track {
                                path: row.path.clone(),
                                name,
                                outcome: Outcome::Skipped("KEEP".into()),
                            });
                            continue;
                        }
                        let outcome = match dubplate_engine::decode::decode_file(&row.path) {
                            Ok(t) => {
                                let (curve, frame) = bpm::onset_curve_full(&t.samples, t.channels, t.sample_rate);
                                judge(&g, &curve, frame, t.sample_rate, t.bpm)
                            }
                            Err(e) => Outcome::Skipped(format!("couldn't read the audio ({e})")),
                        };
                        let _ = tx.send(Msg::Track { path: row.path.clone(), name, outcome });
                    });
                }
            });
            let _ = tx.send(Msg::Finished);
        });
        self.tally = Tally::default();
        self.stage = Stage::Running { rx, total: 0, done: 0, cancel };
    }

    fn poll(&mut self, db: Option<&LibraryDb>) {
        let Stage::Running { rx, total, done, .. } = &mut self.stage else { return };
        let mut finished = false;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Scanned { grids, matched, missing } => {
                    *total = matched;
                    self.tally.grids = grids;
                    self.tally.missing = missing;
                }
                Msg::Track { path, name, outcome } => {
                    *done += 1;
                    match outcome {
                        Outcome::Imported { grid, shift_ms, .. } => {
                            if db.is_some_and(|d| d.save_grid(&path, grid).is_ok()) {
                                self.changed = true;
                            }
                            self.tally.imported += 1;
                            if shift_ms.is_some() {
                                self.tally.shifted += 1;
                            }
                        }
                        Outcome::Unchecked { grid } => {
                            if db.is_some_and(|d| d.save_grid(&path, grid).is_ok()) {
                                self.changed = true;
                            }
                            self.tally.unchecked += 1;
                        }
                        Outcome::Skipped(why) if why == "KEEP" => self.tally.kept_yours += 1,
                        Outcome::Skipped(why) => self.tally.skipped.push((name, why)),
                    }
                }
                Msg::Finished => finished = true,
            }
        }
        if finished {
            self.stage = Stage::Finished;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dubplate_engine::anlz::{build_dat, AnlzTrack};

    const SR: u32 = 44100;

    /// A 60 s track: a kick on every beat of `bpm` starting at `first_s`.
    fn kicks(bpm: f64, first_s: f64) -> Vec<f32> {
        let n = SR as usize * 60;
        let mut out = vec![0.0f32; n * 2];
        let beat = 60.0 / bpm;
        let mut k = 0;
        loop {
            let t = first_s + k as f64 * beat;
            if t >= 59.5 {
                break;
            }
            let s0 = (t * SR as f64) as usize;
            for j in 0..(0.12 * SR as f64) as usize {
                let tt = j as f64 / SR as f64;
                let v =
                    (2.0 * std::f64::consts::PI * (50.0 + 100.0 * (-tt * 40.0).exp()) * tt).sin() * (-tt * 22.0).exp();
                out[(s0 + j) * 2] += v as f32;
                out[(s0 + j) * 2 + 1] += v as f32;
            }
            k += 1;
        }
        out
    }

    fn rb(bpm: f64, first_s: f64) -> RekordboxGrid {
        let beats: Vec<u32> = (0..110).map(|i| ((first_s + i as f64 * 60.0 / bpm) * 1000.0).round() as u32).collect();
        read_dat(&build_dat(&AnlzTrack {
            audio_path: "/Users/dj/Music/Song.mp3",
            beat_times_ms: &beats,
            bpm: bpm as f32,
            cues: &[],
            waveform: &[[0.5; 3]; 16],
            duration_ms: 60_000,
        }))
        .unwrap()
    }

    fn judged(grid: &RekordboxGrid, audio: &[f32], own: f32) -> Outcome {
        let (curve, frame) = bpm::onset_curve_full(audio, 2, SR);
        judge(grid, &curve, frame, SR, own)
    }

    fn row(id: i64, path: &str, dur: Option<f64>) -> TrackRow {
        TrackRow {
            id,
            path: PathBuf::from(path),
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            bpm_manual: false,
            key: None,
            duration_secs: dur,
            preview: vec![],
            added_at: 0,
            analysed: true,
        }
    }

    #[test]
    fn a_grid_that_fits_is_imported_untouched() {
        let audio = kicks(124.0, 1.0);
        match judged(&rb(124.0, 1.0), &audio, 124.0) {
            Outcome::Imported { shift_ms: None, grid, .. } => assert!((grid.bpm - 124.0).abs() < 0.01),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn an_mp3_start_offset_is_removed() {
        // rekordbox's beats sit 51 ms before the kicks, as when an MP3's encoder delay is stripped by one decoder and not the other
        let audio = kicks(124.0, 1.0);
        match judged(&rb(124.0, 1.0 - 0.0512), &audio, 124.0) {
            Outcome::Imported { shift_ms: Some(s), .. } => assert!((s - 51.2).abs() < 1.5, "{s}"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_grid_with_the_wrong_tempo_is_left_out_and_says_so() {
        let audio = kicks(138.0, 0.5);
        match judged(&rb(95.17, 0.5), &audio, 138.0) {
            Outcome::Skipped(why) => assert!(why.contains("95.17") && why.contains("138"), "{why}"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_dynamic_grid_is_left_out() {
        let mut g = rb(120.0, 0.5);
        for b in g.beats.iter_mut().skip(60) {
            b.bpm100 = 12300;
        }
        assert!(matches!(judged(&g, &kicks(120.0, 0.5), 120.0), Outcome::Skipped(w) if w.contains("dynamic")));
    }

    #[test]
    fn silence_cannot_check_a_grid_so_it_is_used_as_is() {
        assert!(matches!(judged(&rb(120.0, 0.5), &vec![0.0f32; SR as usize * 60 * 2], 0.0), Outcome::Unchecked { .. }));
    }

    #[test]
    fn tracks_are_matched_by_name_and_length() {
        let rows = vec![
            row(1, "/m/a/Song.mp3", Some(60.0)),
            row(2, "/m/b/song.MP3", Some(300.0)),
            row(3, "/m/Other.mp3", Some(60.0)),
        ];
        let (pairs, missing) = match_tracks(vec![rb(124.0, 0.5), rb(124.0, 0.5), rb(120.0, 0.5)], &rows);
        assert_eq!(
            pairs.len(),
            1,
            "the 300 s track is not the 60 s grid, and a second grid for the same file finds nothing left"
        );
        assert_eq!(pairs[0].1.id, 1);
        assert_eq!(missing, 2);
    }

    #[test]
    fn dat_files_are_found_below_the_folder() {
        let d = std::env::temp_dir().join(format!("dubplate-gi-{}", std::process::id()));
        std::fs::create_dir_all(d.join("P01/A1")).unwrap();
        std::fs::write(d.join("P01/A1/ANLZ0000.DAT"), b"x").unwrap();
        std::fs::write(d.join("P01/A1/ANLZ0000.EXT"), b"x").unwrap();
        assert_eq!(find_dats(&d).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
