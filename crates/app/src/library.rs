//! Track browser: sidebar (collection, flux matches, folders, reels) + a table with a
//! mini waveform preview (hot cues included), tempo, key, genre and a Flux score.
//!
//! Vocabulary: a **Reel** is a named set of tracks that work together (a track can be
//! on many reels); **Flux** scores how well a track follows the one that is playing.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use dubplate_engine::Key;
use egui::{pos2, vec2, Color32, FontId, Rect, RichText, Sense, Stroke};

use crate::app::palette;
use crate::db::{LibraryDb, MiniCue, TrackRow};
use crate::flux::{flux, Flux, FluxInfo};
use crate::library_index::{IndexEvent, Indexer};
use crate::library_query::Query;

const SIDEBAR_W: f32 = 196.0;
const ROW_H: f32 = 36.0;
const HEADER_H: f32 = 24.0;
const PREVIEW_W: f32 = 150.0;

fn config_dir() -> PathBuf {
    dubplate_engine::paths::config_dir()
}

/// The folder the old browser remembered — used to seed the first watched folder.
fn legacy_dir() -> Option<PathBuf> {
    let s = std::fs::read_to_string(config_dir().join("lastdir")).ok()?;
    let p = PathBuf::from(s.trim());
    p.is_dir().then_some(p)
}

/// What the library panel wants the app to do.
pub enum LibraryAction {
    /// Double-click: load to the first available deck.
    LoadAny(PathBuf),
    /// Context menu / drag-drop: load to a specific deck index.
    LoadDeck(usize, PathBuf),
    /// Context menu: check and fix this track's beat grid in the Prepare screen.
    Prepare(PathBuf),
}

/// A deck as the browser sees it (for Flux and for highlighting).
#[derive(Clone, Default)]
pub struct DeckRef {
    pub path: Option<PathBuf>,
    /// Effective (playing) whole-number BPM; 0 when unknown.
    pub bpm: f32,
    pub playing: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum View {
    All,
    Source(PathBuf),
    Reel(i64),
    /// Tracks ranked by Flux against deck N.
    Flux(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Col {
    Title,
    Artist,
    Album,
    Bpm,
    Key,
    Genre,
    Time,
    Added,
    Flux,
}

const SEARCH_HELP: &str = "Type words to match title, artist, album, genre or file name.\n\
Narrow with terms (all must match):\n\
  bpm:120-128   bpm:124   bpm:~124 (±6%)   bpm:>130   bpm:<90\n\
  key:8A   key:~8A (8A plus 7A, 9A and 8B)\n\
  artist:  album:  genre:  title:  folder:  year:1995-2005\n\
  added:7d   added:2w   added:3m\n\
  \"exact phrase\"     -word (leave out)";

/// Changes decided while drawing, applied afterwards (drawing only borrows immutably).
enum Deferred {
    ImportGrids,
    Select(PathBuf),
    AddToReel(i64, PathBuf),
    RemoveFromReel(i64, PathBuf),
    NewReelFor(PathBuf),
    View(View),
    RemoveSource(PathBuf),
    DeleteReel(i64),
    RenameReel(i64),
    ExportReel(i64),
    AddFolder,
    NewReel,
    CommitNewReel,
    CancelNewReel,
}

struct NewReel {
    name: String,
    fresh: bool,
    /// Track to put on the reel as soon as it exists (from "Add to Reel > New reel").
    add_track: Option<PathBuf>,
}

/// What the visible list was built from; the list is rebuilt only when this changes.
#[derive(Clone, PartialEq)]
struct VisKey {
    gen: u64,
    view: View,
    sort: (Col, bool),
    filter: String,
    reference: Option<(Option<PathBuf>, Option<u32>)>,
    minute: u64,
}

/// Everything a reload reads and works out, so it can all be done on a background thread: tens of thousands of rows
/// take long enough to read, index and lower-case that doing it between two frames is seen as a hitch in the
/// scrolling waveforms.
struct Loaded {
    rows: Vec<TrackRow>,
    sources: Vec<PathBuf>,
    reels: Vec<(i64, String, usize)>,
    by_path: HashMap<PathBuf, usize>,
    /// Per row, what a plain search word is looked for in: title, artist, album, genre and file name, lower-cased.
    hay: Vec<String>,
    cues: HashMap<PathBuf, Vec<MiniCue>>,
    /// Tracks in the watched folders, and in each of them.
    counts: (usize, Vec<usize>),
    /// Per row: is it in a watched folder, and what Flux scores it on.
    in_src: Vec<bool>,
    infos: Vec<FluxInfo>,
    /// Per text column (title, artist, album, genre): each row's place in A to Z order, so sorting the list by a
    /// text column is sorting whole numbers.
    rank: [Vec<u32>; 4],
}

impl Loaded {
    fn read(db: &LibraryDb) -> Loaded {
        let rows = db.all_tracks();
        let sources = db.sources();
        let by_path = rows.iter().enumerate().map(|(i, r)| (r.path.clone(), i)).collect();
        let hay = rows.iter().map(search_text).collect();
        let mut counts = (0, vec![0; sources.len()]);
        let mut in_src = Vec::with_capacity(rows.len());
        for r in &rows {
            let mut any = false;
            for (i, s) in sources.iter().enumerate() {
                if r.path.starts_with(s) {
                    counts.1[i] += 1;
                    any = true;
                }
            }
            counts.0 += any as usize;
            in_src.push(any);
        }
        let infos = rows.iter().map(flux_info).collect();
        let text = |r: &TrackRow, col: usize| match col {
            0 => r.display_title(),
            1 => r.artist.clone().unwrap_or_default(),
            2 => r.album.clone().unwrap_or_default(),
            _ => r.genre.clone().unwrap_or_default(),
        };
        let rank = [0, 1, 2, 3].map(|col| {
            let keys: Vec<String> = rows.iter().map(|r| text(r, col).to_lowercase()).collect();
            let mut order: Vec<u32> = (0..rows.len() as u32).collect();
            order.sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize])); // stable: equal names keep their order
            let mut rank = vec![0u32; rows.len()];
            for (place, &row) in order.iter().enumerate() {
                rank[row as usize] = place as u32;
            }
            rank
        });
        Loaded { rows, by_path, hay, counts, in_src, infos, rank, reels: db.reels(), cues: db.mini_cues(), sources }
    }
}

/// What Flux scores a row on.
fn flux_info(row: &TrackRow) -> FluxInfo {
    FluxInfo {
        bpm: row.bpm,
        camelot: row.key.map(|(t, m)| Key { tonic: t, minor: m, confidence: 0.0 }.camelot()),
        genre: row.genre.clone().filter(|g| !g.trim().is_empty()),
    }
}

/// What a plain search word is looked for in.
fn search_text(r: &TrackRow) -> String {
    let mut s = r.display_title();
    for part in [&r.artist, &r.album, &r.genre] {
        s.push('\n');
        s.push_str(part.as_deref().unwrap_or(""));
    }
    s.push('\n');
    s.push_str(r.path.file_name().and_then(|f| f.to_str()).unwrap_or(""));
    s.to_lowercase()
}

pub struct Library {
    db: Option<LibraryDb>,
    /// A background reload in flight, and whether another was asked for meanwhile.
    loading: Option<std::sync::mpsc::Receiver<Loaded>>,
    reload_queued: bool,
    /// Bumped whenever rows, folders or reels are re-read; part of the visible-list cache key.
    rows_gen: u64,
    vis_cache: Option<(VisKey, std::rc::Rc<Vec<(usize, Option<Flux>)>>)>,
    indexer: Indexer,
    rows: Vec<TrackRow>,
    by_path: HashMap<PathBuf, usize>,
    hay: Vec<String>,
    counts: (usize, Vec<usize>),
    in_src: Vec<bool>,
    infos: Vec<FluxInfo>,
    rank: [Vec<u32>; 4],
    cues: HashMap<PathBuf, Vec<MiniCue>>,
    cues_dirty: bool,
    sources: Vec<PathBuf>,
    reels: Vec<(i64, String, usize)>,
    view: View,
    sort: (Col, bool), // (column, ascending)
    filter: String,
    columns: ColumnPrefs,
    selected: Option<PathBuf>,
    new_reel: Option<NewReel>,
    renaming: Option<(i64, String)>,
    usb: Option<crate::usb::UsbDialog>,
    grid_import: Option<crate::grid_import::GridImportDialog>,
    /// Rows the browser encoder asked to move since the last frame.
    nav: i32,
    /// Row to bring into view after an encoder move.
    scroll_to: Option<usize>,
}

impl Library {
    pub fn new() -> Self {
        let db = LibraryDb::open().ok();
        if let Some(db) = &db {
            if db.sources().is_empty() {
                let seed = legacy_dir().unwrap_or_else(|| {
                    let home = dubplate_engine::paths::home_dir();
                    let music = home.join("Music");
                    if music.is_dir() {
                        music
                    } else {
                        home
                    }
                });
                let _ = db.add_source(&seed);
            }
        }
        let mut lib = Self {
            db,
            loading: None,
            reload_queued: false,
            rows_gen: 0,
            vis_cache: None,
            indexer: Indexer::start(),
            rows: Vec::new(),
            by_path: HashMap::new(),
            hay: Vec::new(),
            counts: (0, Vec::new()),
            in_src: Vec::new(),
            infos: Vec::new(),
            rank: Default::default(),
            cues: HashMap::new(),
            cues_dirty: true,
            sources: Vec::new(),
            reels: Vec::new(),
            view: View::All,
            sort: (Col::Title, true),
            filter: String::new(),
            columns: ColumnPrefs::load(),
            selected: None,
            new_reel: None,
            renaming: None,
            usb: None,
            grid_import: None,
            nav: 0,
            scroll_to: None,
        };
        lib.reload();
        lib
    }

    /// The selected track (used by the MIDI LOAD buttons).
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected.clone()
    }

    /// Moves the selection by `rows` (browser encoder); applied on the next frame.
    pub fn move_selection(&mut self, rows: i32) {
        self.nav += rows;
    }

    /// Hot cues changed somewhere: refresh the preview markers.
    pub fn invalidate_cues(&mut self) {
        self.cues_dirty = true;
    }

    /// Re-reads rows, folders and reels from the database, now. For changes the user just made in the library itself
    /// (a reel, a folder), which must show at once; anything else uses [`reload_soon`](Self::reload_soon).
    pub fn reload(&mut self) {
        let Some(db) = &self.db else { return };
        let loaded = Loaded::read(db);
        self.take(loaded);
    }

    /// Tells the background analysis whether a deck is playing; it keeps to one thread while one is.
    pub fn set_live(&self, live: bool) {
        self.indexer.status.live.store(live, std::sync::atomic::Ordering::Relaxed);
    }

    /// Re-reads the library off the UI thread and shows it when it is ready (a moment later). For changes that come
    /// from elsewhere (a track was loaded, analysed or saved), where a stall would be seen in the playing waveforms.
    pub fn reload_soon(&mut self) {
        self.reload_in_background();
    }

    fn take(&mut self, l: Loaded) {
        // the old rows are tens of thousands of small allocations: freeing them is work for another thread too
        let old = (
            std::mem::replace(&mut self.rows, l.rows),
            std::mem::replace(&mut self.by_path, l.by_path),
            std::mem::replace(&mut self.hay, l.hay),
            std::mem::replace(&mut self.infos, l.infos),
        );
        std::thread::spawn(move || drop(old));
        self.in_src = l.in_src;
        self.rank = l.rank;
        self.counts = l.counts;
        self.sources = l.sources;
        self.reels = l.reels;
        self.cues = l.cues;
        self.cues_dirty = false;
        self.rows_gen += 1;
    }

    fn reload_in_background(&mut self) {
        if self.loading.is_some() {
            self.reload_queued = true;
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.loading = Some(rx);
        std::thread::spawn(move || {
            if let Ok(db) = LibraryDb::open() {
                let _ = tx.send(Loaded::read(&db));
            }
        });
    }

    fn poll_reload(&mut self) {
        let Some(rx) = &self.loading else { return };
        match rx.try_recv() {
            Ok(loaded) => {
                self.take(loaded);
                self.loading = None;
                if std::mem::take(&mut self.reload_queued) {
                    self.reload_in_background();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.loading = None;
            }
        }
    }

    /// Which deck Flux is measured against: the requested one in a Flux view, else the
    /// playing deck, else the first loaded one.
    fn reference(&self, decks: &[DeckRef; 2]) -> Option<(usize, FluxInfo)> {
        let pick = match &self.view {
            View::Flux(d) if decks[*d].path.is_some() => Some(*d),
            _ => (0..2)
                .find(|&d| decks[d].playing && decks[d].path.is_some())
                .or_else(|| (0..2).find(|&d| decks[d].path.is_some())),
        }?;
        let path = decks[pick].path.as_ref()?;
        let row = self.by_path.get(path).map(|&i| &self.rows[i]);
        let mut info = row.map(flux_info).unwrap_or_default();
        if decks[pick].bpm > 0.0 {
            info.bpm = Some(decks[pick].bpm);
        }
        Some((pick, info))
    }

    /// Rows to show (indices into `rows`) with their Flux, filtered and sorted.
    fn compute_visible(&self, decks: &[DeckRef; 2]) -> Vec<(usize, Option<Flux>)> {
        let query = Query::parse(&self.filter);
        let now = unix_now();
        let reference = self.reference(decks);
        let ref_path = reference.as_ref().and_then(|(d, _)| decks[*d].path.clone());

        let base: Vec<usize> = match &self.view {
            View::All | View::Flux(_) => (0..self.rows.len()).filter(|&i| self.in_src[i]).collect(),
            View::Source(dir) => (0..self.rows.len()).filter(|&i| self.rows[i].path.starts_with(dir)).collect(),
            View::Reel(id) => self
                .db
                .as_ref()
                .map(|db| db.reel_paths(*id))
                .unwrap_or_default()
                .iter()
                .filter_map(|p| self.by_path.get(p).copied())
                .collect(),
        };

        let mut out: Vec<(usize, Option<Flux>)> = base
            .into_iter()
            .filter(|&i| {
                let r = &self.rows[i];
                if matches!(self.view, View::Flux(_)) && Some(&r.path) == ref_path.as_ref() {
                    return false;
                }
                query.matches_in(r, &self.hay[i], now)
            })
            .map(|i| {
                let f = reference.as_ref().and_then(|(_, info)| flux(info, &self.infos[i]));
                (i, f)
            })
            .collect();

        let (col, asc) = self.sort;
        let is_reel = matches!(self.view, View::Reel(_));
        if !(is_reel && self.sort.0 == Col::Title) {
            let rows = &self.rows;
            // text columns were put in order when the library was read
            let text_col = match col {
                Col::Title => Some(0),
                Col::Artist => Some(1),
                Col::Album => Some(2),
                Col::Genre => Some(3),
                _ => None,
            };
            if let Some(c) = text_col {
                let rank = &self.rank[c];
                out.sort_by_key(|a| rank[a.0]);
                if !asc {
                    out.reverse();
                }
            } else {
                let camelot = |r: &TrackRow| {
                    r.key
                        .map(|(t, m)| Key { tonic: t, minor: m, confidence: 0.0 }.camelot())
                        .map(|(n, l)| n as u32 * 2 + (l == 'B') as u32)
                };
                out.sort_by(|a, b| {
                    let (ra, rb) = (&rows[a.0], &rows[b.0]);
                    let ord = match col {
                        Col::Bpm => cmp_opt(ra.bpm, rb.bpm),
                        Col::Key => cmp_opt(camelot(ra), camelot(rb)),
                        Col::Time => cmp_opt(ra.duration_secs, rb.duration_secs),
                        Col::Flux => a.1.map(|f| f.score).cmp(&b.1.map(|f| f.score)),
                        Col::Added => ra.added_at.cmp(&rb.added_at),
                        _ => std::cmp::Ordering::Equal,
                    };
                    if asc {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
            }
        }
        out
    }

    /// [`compute_visible`](Self::compute_visible), recomputed only when something it depends on changed.
    /// Filtering and sorting tens of thousands of rows on every frame made the whole UI sluggish.
    fn visible(&mut self, decks: &[DeckRef; 2]) -> std::rc::Rc<Vec<(usize, Option<Flux>)>> {
        let key = VisKey {
            gen: self.rows_gen,
            view: self.view.clone(),
            sort: self.sort,
            filter: self.filter.clone(),
            reference: self.reference(decks).map(|(d, info)| (decks[d].path.clone(), info.bpm.map(f32::to_bits))),
            minute: unix_now() as u64 / 60,
        };
        if let Some((k, rows)) = &self.vis_cache {
            if *k == key {
                return std::rc::Rc::clone(rows);
            }
        }
        let rows = std::rc::Rc::new(self.compute_visible(decks));
        self.vis_cache = Some((key, std::rc::Rc::clone(&rows)));
        rows
    }

    // ── drawing ──────────────────────────────────────────────────────────────

    /// Draws the browser and returns a load request, if the user made one.
    pub fn show(&mut self, ui: &mut egui::Ui, decks: &[DeckRef; 2]) -> Option<LibraryAction> {
        let mut changed = false;
        while let Ok(IndexEvent::Changed) = self.indexer.events.try_recv() {
            changed = true;
        }
        if changed {
            self.reload_in_background();
        }
        self.poll_reload();
        if self.cues_dirty {
            if let Some(db) = &self.db {
                self.cues = db.mini_cues();
            }
            self.cues_dirty = false;
        }

        let mut action: Option<LibraryAction> = None;
        let mut deferred: Vec<Deferred> = Vec::new();

        let full = ui.available_rect_before_wrap();
        let side = Rect::from_min_size(full.min, vec2(SIDEBAR_W, full.height()));
        let main = Rect::from_min_max(pos2(side.right() + 1.0, full.top()), full.max);
        ui.painter().rect_filled(side, 0.0, palette::inset());
        ui.painter().line_segment(
            [pos2(side.right(), full.top()), pos2(side.right(), full.bottom())],
            Stroke::new(1.0_f32, palette::divider()),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().id_salt("lib_side").max_rect(side.shrink2(vec2(6.0, 4.0))), |ui| {
            ui.set_clip_rect(side.intersect(ui.clip_rect()));
            self.draw_sidebar(ui, decks, &mut deferred);
        });
        ui.allocate_new_ui(egui::UiBuilder::new().id_salt("lib_main").max_rect(main.shrink2(vec2(8.0, 4.0))), |ui| {
            ui.set_clip_rect(main.intersect(ui.clip_rect()));
            self.draw_main(ui, decks, &mut deferred, &mut action);
        });
        ui.allocate_rect(full, Sense::hover());

        self.apply(deferred);
        if let Some(d) = &mut self.usb {
            if !d.show(ui.ctx()) {
                self.usb = None;
            }
        }
        if let Some(d) = &mut self.grid_import {
            let keep = d.show(ui.ctx(), self.db.as_ref());
            if d.changed {
                d.changed = false;
                self.reload();
            }
            if !keep {
                self.grid_import = None;
            }
        }
        action
    }

    fn draw_sidebar(&mut self, ui: &mut egui::Ui, decks: &[DeckRef; 2], deferred: &mut Vec<Deferred>) {
        egui::ScrollArea::vertical().id_salt("lib_side_scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(SIDEBAR_W - 14.0);

            side_header(ui, "COLLECTION", None);
            let total = self.counts.0;
            if side_item(ui, "All tracks", Some(total), self.view == View::All, None).clicked() {
                deferred.push(Deferred::View(View::All));
            }

            side_header(ui, "FLUX MATCHES", None);
            for d in 0..2usize {
                let title = decks[d].path.as_ref().and_then(|p| self.by_path.get(p))
                    .map(|&i| self.rows[i].display_title());
                let label = match &title {
                    Some(t) => format!("{}  {}", if d == 0 { "A" } else { "B" }, truncate(t, 20)),
                    None => format!("{}  (empty)", if d == 0 { "A" } else { "B" }),
                };
                let r = side_item(ui, &label, None, self.view == View::Flux(d),
                    Some(palette::deck(d)));
                match &title {
                    Some(t) => { r.clone().on_hover_text(format!("Best follow-ups to: {t}")); }
                    None => { r.clone().on_hover_text("Load a track on this deck to see what mixes well after it"); }
                }
                if r.clicked() && decks[d].path.is_some() { deferred.push(Deferred::View(View::Flux(d))); }
            }

            if side_header(ui, "REELS", Some("+")) { deferred.push(Deferred::NewReel); }
            if let Some(nr) = &mut self.new_reel {
                let te = ui.add(egui::TextEdit::singleline(&mut nr.name)
                    .hint_text("Reel name, Enter to create")
                    .font(FontId::proportional(crate::theme::sz(13.0)))
                    .desired_width(SIDEBAR_W - 24.0));
                if nr.fresh { te.request_focus(); nr.fresh = false; }
                if te.lost_focus() {
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    deferred.push(if enter { Deferred::CommitNewReel } else { Deferred::CancelNewReel });
                }
            }
            if self.reels.is_empty() && self.new_reel.is_none() {
                ui.label(RichText::new("No reels yet — right-click a track\nand choose Add to Reel").size(crate::theme::sz(11.0)).color(palette::text_dim()));
            }
            for (id, name, count) in self.reels.clone() {
                if let Some((rid, text)) = &mut self.renaming {
                    if *rid == id {
                        let te = ui.add(egui::TextEdit::singleline(text)
                            .font(FontId::proportional(crate::theme::sz(13.0))).desired_width(SIDEBAR_W - 24.0));
                        if !te.has_focus() && !te.lost_focus() { te.request_focus(); }
                        if te.lost_focus() { deferred.push(Deferred::RenameReel(id)); }
                        continue;
                    }
                }
                let resp = side_item(ui, &name, Some(count), self.view == View::Reel(id), None);
                if resp.clicked() { deferred.push(Deferred::View(View::Reel(id))); }
                // Drop a dragged track onto a reel to add it.
                if resp.dnd_hover_payload::<PathBuf>().is_some() {
                    ui.painter().rect_stroke(resp.rect, 4.0, Stroke::new(1.5_f32, palette::deck_a()));
                }
                if let Some(p) = resp.dnd_release_payload::<PathBuf>() {
                    deferred.push(Deferred::AddToReel(id, (*p).clone()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename").clicked() { self.renaming = Some((id, name.clone())); ui.close_menu(); }
                    if ui.button("Export to USB…").on_hover_text("Copy this reel to a drive that CDJ / XDJ players can read").clicked() {
                        deferred.push(Deferred::ExportReel(id));
                        ui.close_menu();
                    }
                    if ui.button("Delete reel").clicked() { deferred.push(Deferred::DeleteReel(id)); ui.close_menu(); }
                });
            }

            let r = side_header(ui, "FOLDERS", Some("+"));
            if r { deferred.push(Deferred::AddFolder); }
            for (k, src) in self.sources.clone().into_iter().enumerate() {
                let n = self.counts.1.get(k).copied().unwrap_or(0);
                let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("/").to_string();
                let resp = side_item(ui, &name, Some(n), self.view == View::Source(src.clone()), None)
                    .on_hover_text(src.display().to_string());
                if resp.clicked() { deferred.push(Deferred::View(View::Source(src.clone()))); }
                resp.context_menu(|ui| {
                    if ui.button("Remove folder from library").clicked() {
                        deferred.push(Deferred::RemoveSource(src.clone()));
                        ui.close_menu();
                    }
                });
            }
            if ui.small_button("Import rekordbox grids…")
                .on_hover_text("Reuse beat grids rekordbox already made, from its analysis folder (PIONEER/USBANLZ on a stick or in rekordbox's data folder)")
                .clicked() { deferred.push(Deferred::ImportGrids); }

        });
    }

    fn draw_main(
        &mut self,
        ui: &mut egui::Ui,
        decks: &[DeckRef; 2],
        deferred: &mut Vec<Deferred>,
        action: &mut Option<LibraryAction>,
    ) {
        // Toolbar
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let title = match &self.view {
                View::All => "All tracks".to_string(),
                View::Source(p) => p.file_name().and_then(|n| n.to_str()).unwrap_or("Folder").to_string(),
                View::Reel(id) => self.reels.iter().find(|r| r.0 == *id).map(|r| r.1.clone()).unwrap_or_default(),
                View::Flux(d) => format!("Flux matches for deck {}", if *d == 0 { "A" } else { "B" }),
            };
            ui.label(RichText::new(title).size(crate::theme::sz(15.0)).strong().color(palette::text()));
            ui.add(egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Search…  try bpm:120-128 key:~8A")
                .font(FontId::proportional(crate::theme::sz(13.0)))
                .desired_width(260.0))
                .on_hover_text(SEARCH_HELP);
            let busy = self.indexer.status.busy.load(std::sync::atomic::Ordering::Relaxed);
            let (done, total) = (
                self.indexer.status.done.load(std::sync::atomic::Ordering::Relaxed),
                self.indexer.status.total.load(std::sync::atomic::Ordering::Relaxed),
            );
            let st = &self.indexer.status;
            if st.scanning.load(std::sync::atomic::Ordering::Relaxed) {
                let (found, checked) = (
                    st.found.load(std::sync::atomic::Ordering::Relaxed),
                    st.checked.load(std::sync::atomic::Ordering::Relaxed),
                );
                ui.spinner();
                let text = if checked == 0 {
                    format!("Scanning folders… {found} files")
                } else {
                    format!("Checking files {checked}/{found}")
                };
                ui.label(RichText::new(text).size(crate::theme::sz(12.0)).color(palette::text_dim()));
            } else if busy && total > 0 && done < total {
                ui.spinner();
                ui.label(RichText::new(format!("Analysing {done}/{total}")).size(crate::theme::sz(12.0)).color(palette::text_dim()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut auto = self.indexer.status.auto.load(std::sync::atomic::Ordering::Relaxed);
                if ui.checkbox(&mut auto, "Auto-analyse")
                    .on_hover_text("Analyse every track in the background (tempo, key, waveform). Off: a track is analysed the first time you load it on a deck.")
                    .changed()
                {
                    self.indexer.status.auto.store(auto, std::sync::atomic::Ordering::Relaxed);
                    crate::library_index::save_auto(auto);
                    if auto { self.indexer.analyse_pending(); }
                }
                let q = crate::stem_queue::queue().status();
                let mut reel_on = q.reel_on.load(std::sync::atomic::Ordering::Relaxed);
                let waiting = q.waiting.load(std::sync::atomic::Ordering::Relaxed)
                    + usize::from(q.running_reel.load(std::sync::atomic::Ordering::Relaxed));
                let tip = "Separate the stems of tracks on a Reel in the background when their bar line is in doubt, and keep only what they say about it. A deck that wants its stems always goes first.";
                let label = if reel_on && waiting > 0 { format!("Reel stems ({waiting})") } else { "Reel stems".to_string() };
                if ui.checkbox(&mut reel_on, label)
                    .on_hover_text(tip)
                    .changed()
                {
                    crate::stem_queue::queue().set_reel_on(reel_on);
                }
                if ui.small_button("Rescan").on_hover_text("Look for new or changed files in your folders now. At start-up this only happens if the last scan was over 6 hours ago.").clicked() {
                    self.indexer.rescan();
                }
            });
        });
        let chips = Query::parse(&self.filter).chips();
        if !chips.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for c in &chips {
                    ui.label(RichText::new(&c.label).size(crate::theme::sz(11.5)).color(if c.ok {
                        palette::deck_a()
                    } else {
                        palette::warn()
                    }));
                }
            });
        }
        ui.add_space(2.0);

        let vis_rc = self.visible(decks);
        let vis = &*vis_rc;
        if self.nav != 0 && !vis.is_empty() {
            let cur = self.selected.as_ref().and_then(|p| vis.iter().position(|(i, _)| &self.rows[*i].path == p));
            let target = match cur {
                Some(c) => (c as i64 + self.nav as i64).clamp(0, vis.len() as i64 - 1) as usize,
                None => {
                    if self.nav > 0 {
                        0
                    } else {
                        vis.len() - 1
                    }
                }
            };
            self.selected = Some(self.rows[vis[target].0].path.clone());
            self.scroll_to = Some(target);
        }
        self.nav = 0;
        let reference = self.reference(decks);
        let w = ui.available_width();
        let cols = ColLayout::new(w, &self.columns);

        // Header: click to sort, drag a column's left edge to resize, right-click to pick columns.
        let (hrect, _) = ui.allocate_exact_size(vec2(w, HEADER_H), Sense::hover());
        ui.painter().rect_filled(hrect, 0.0, palette::raised());
        ui.painter().text(
            pos2(hrect.left() + cols.preview.0 + 4.0, hrect.center().y),
            egui::Align2::LEFT_CENTER,
            "PREVIEW",
            FontId::proportional(crate::theme::sz(11.5)),
            palette::text_dim(),
        );
        let flux_label = match &reference {
            Some((d, _)) => format!("FLUX vs {}", if *d == 0 { "A" } else { "B" }),
            None => "FLUX".into(),
        };
        let mut resized: Option<(Col, f32)> = None;
        let mut resize_done = false;
        let mut toggle: Option<Col> = None;
        let mut reset_cols = false;
        let mut cells: Vec<(Col, (f32, f32))> = vec![(Col::Title, cols.title)];
        cells.extend(cols.slots.iter().copied());
        for (col, (x0, x1)) in cells {
            let cell =
                Rect::from_min_max(pos2(hrect.left() + x0, hrect.top()), pos2(hrect.left() + x1, hrect.bottom()));
            let resp = ui.interact(cell, egui::Id::new(("lib_hdr", col as u8)), Sense::click());
            let active = self.sort.0 == col;
            let right = col.align_right();
            let label = if col == Col::Flux { flux_label.clone() } else { col.label().to_owned() };
            let at = if right {
                pos2(cell.right() - 4.0, cell.center().y)
            } else {
                pos2(cell.left() + 4.0, cell.center().y)
            };
            let text_rect = ui.painter_at(cell).text(
                at,
                if right { egui::Align2::RIGHT_CENTER } else { egui::Align2::LEFT_CENTER },
                &label,
                FontId::proportional(crate::theme::sz(11.5)),
                if active { palette::text() } else { palette::text_dim() },
            );
            if active {
                let (x, y) = if right {
                    (text_rect.left() - 8.0, cell.center().y)
                } else {
                    (text_rect.right() + 8.0, cell.center().y)
                };
                let pts = if self.sort.1 {
                    vec![pos2(x - 4.0, y + 2.5), pos2(x + 4.0, y + 2.5), pos2(x, y - 3.5)]
                } else {
                    vec![pos2(x - 4.0, y - 2.5), pos2(x + 4.0, y - 2.5), pos2(x, y + 3.5)]
                };
                ui.painter().add(egui::Shape::convex_polygon(pts, palette::text(), Stroke::NONE));
            }
            if resp.clicked() {
                self.sort = if active { (col, !self.sort.1) } else { (col, col.sorts_ascending_first()) };
            }
            resp.context_menu(|ui| column_menu(ui, &self.columns, &mut toggle, &mut reset_cols));

            if col != Col::Title {
                let grip =
                    Rect::from_min_max(pos2(cell.left() - 3.0, cell.top()), pos2(cell.left() + 3.0, cell.bottom()));
                let g = ui.interact(grip, egui::Id::new(("lib_grip", col as u8)), Sense::drag());
                if g.hovered() || g.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    ui.painter().line_segment(
                        [pos2(cell.left(), cell.top() + 3.0), pos2(cell.left(), cell.bottom() - 3.0)],
                        Stroke::new(1.5_f32, palette::deck_a()),
                    );
                }
                if g.dragged() {
                    resized = Some((col, self.columns.width[col as usize] - g.drag_delta().x));
                }
                if g.drag_stopped() {
                    resize_done = true;
                }
            }
        }
        if let Some((c, wd)) = resized {
            self.columns.set_width(c, wd);
            ui.ctx().request_repaint();
        }
        if let Some(c) = toggle {
            self.columns.toggle(c);
        }
        if reset_cols {
            self.columns = ColumnPrefs::default();
        }
        if resize_done || toggle.is_some() || reset_cols {
            self.columns.save();
            ui.ctx().request_repaint();
        }

        if vis.is_empty() {
            ui.add_space(20.0);
            let msg = if self.rows.is_empty() {
                "Add a folder to build your library (＋ next to FOLDERS)."
            } else if matches!(self.view, View::Reel(_)) {
                "This reel is empty. Drag tracks onto it, or right-click a track → Add to Reel."
            } else {
                "No tracks match."
            };
            ui.label(RichText::new(msg).size(crate::theme::sz(14.0)).color(palette::text_dim()));
            return;
        }

        let reel_view = if let View::Reel(id) = self.view { Some(id) } else { None };
        let reels = self.reels.clone();
        let loaded: Vec<Option<&PathBuf>> = decks.iter().map(|d| d.path.as_ref()).collect();
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::vertical().id_salt("lib_rows").auto_shrink([false, false]);
        if let Some(t) = self.scroll_to.take() {
            // Keep the encoder's row roughly in the middle of the list.
            let h = ui.available_height();
            area = area.vertical_scroll_offset((t as f32 * ROW_H - h / 2.0 + ROW_H).max(0.0));
        }
        area.show_rows(ui, ROW_H, vis.len(), |ui, range| {
            for i in range {
                let (idx, fl) = vis[i];
                let row = &self.rows[idx];
                let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click_and_drag());
                resp.dnd_set_drag_payload(row.path.clone());

                if ui.is_rect_visible(rect) {
                    let on_deck = loaded.iter().position(|p| *p == Some(&row.path));
                    let sel = self.selected.as_ref() == Some(&row.path);
                    let bg = if resp.dragged() {
                        palette::deck_a().linear_multiply(0.25)
                    } else if sel {
                        palette::selected()
                    } else if resp.hovered() {
                        palette::hover()
                    } else if i % 2 == 0 {
                        palette::inset()
                    } else {
                        palette::surface()
                    };
                    ui.painter().rect_filled(rect, 0.0, bg);
                    if let Some(d) = on_deck {
                        let c = palette::deck(d);
                        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(3.0, ROW_H)), 0.0, c);
                    }
                    self.paint_row(ui, rect, &cols, row, fl);
                }

                let mut tip = row.path.display().to_string();
                if let Some(f) = fl {
                    let pct = |v: Option<f32>| v.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "—".into());
                    tip = format!(
                        "Flux {}%  ·  tempo {}  ·  key {}  ·  genre {}\n{tip}",
                        f.score,
                        pct(f.tempo),
                        pct(f.key),
                        pct(f.genre)
                    );
                }
                resp.clone().on_hover_text(tip);

                if resp.clicked() {
                    deferred.push(Deferred::Select(row.path.clone()));
                }
                if resp.double_clicked() {
                    *action = Some(LibraryAction::LoadAny(row.path.clone()));
                }

                let path = row.path.clone();
                let db = self.db.as_ref();
                resp.context_menu(|ui| {
                    ui.set_min_width(200.0);
                    ui.label(
                        RichText::new(row.display_title()).size(crate::theme::sz(12.0)).color(palette::text_dim()),
                    );
                    ui.separator();
                    if ui.button("Load to Deck A").clicked() {
                        *action = Some(LibraryAction::LoadDeck(0, path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Load to Deck B").clicked() {
                        *action = Some(LibraryAction::LoadDeck(1, path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Prepare (beat grid)…").clicked() {
                        *action = Some(LibraryAction::Prepare(path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.menu_button("Add to Reel", |ui| {
                        let member = db.map(|d| d.reels_of(&path)).unwrap_or_default();
                        for (id, name, _) in &reels {
                            let on = member.contains(id);
                            let label = if on { format!("[x]  {name}") } else { format!("[  ]  {name}") };
                            if ui.button(label).clicked() {
                                deferred.push(if on {
                                    Deferred::RemoveFromReel(*id, path.clone())
                                } else {
                                    Deferred::AddToReel(*id, path.clone())
                                });
                                ui.close_menu();
                            }
                        }
                        if !reels.is_empty() {
                            ui.separator();
                        }
                        if ui.button("New reel…").clicked() {
                            deferred.push(Deferred::NewReelFor(path.clone()));
                            ui.close_menu();
                        }
                    });
                    if let Some(rid) = reel_view {
                        if ui.button("Remove from this reel").clicked() {
                            deferred.push(Deferred::RemoveFromReel(rid, path.clone()));
                            ui.close_menu();
                        }
                    }
                });
            }
        });
    }

    fn paint_row(&self, ui: &egui::Ui, rect: Rect, cols: &ColLayout, row: &TrackRow, fl: Option<Flux>) {
        let p = ui.painter_at(rect);
        let cy = rect.center().y;
        let x = |v: f32| rect.left() + v;

        // Preview waveform + hot cues
        let prev =
            Rect::from_min_max(pos2(x(cols.preview.0), rect.top() + 7.0), pos2(x(cols.preview.1), rect.bottom() - 7.0));
        draw_preview(
            &p,
            prev,
            &row.preview,
            self.cues.get(&row.path).map(|v| v.as_slice()).unwrap_or(&[]),
            row.duration_secs.unwrap_or(0.0),
        );

        // Title, with whatever the artist / album columns are not already showing underneath
        let tw = cols.title.1 - cols.title.0 - 8.0;
        let title = row.display_title();
        p.text(
            pos2(x(cols.title.0) + 4.0, cy - 8.0),
            egui::Align2::LEFT_CENTER,
            fit(&title, tw, 7.4),
            FontId::proportional(crate::theme::sz(14.0)),
            palette::text(),
        );
        let mut sub: Vec<&str> = Vec::new();
        if cols.get(Col::Artist).is_none() {
            sub.extend(row.artist.as_deref().filter(|a| !a.is_empty()));
        }
        if cols.get(Col::Album).is_none() {
            sub.extend(row.album.as_deref().filter(|a| !a.is_empty()));
        }
        let sub = if sub.is_empty() {
            row.path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()
        } else {
            sub.join("  ·  ")
        };
        p.text(
            pos2(x(cols.title.0) + 4.0, cy + 9.0),
            egui::Align2::LEFT_CENTER,
            fit(&sub, tw, 6.4),
            FontId::proportional(crate::theme::sz(12.0)),
            palette::text_dim(),
        );

        let text_col = |v: &Option<String>, c: Col| {
            let Some((c0, c1)) = cols.get(c) else { return };
            let t = v.clone().filter(|g| !g.trim().is_empty());
            p.text(
                pos2(x(c0) + 4.0, cy),
                egui::Align2::LEFT_CENTER,
                fit(t.as_deref().unwrap_or("—"), c1 - c0 - 8.0, 7.0),
                FontId::proportional(crate::theme::sz(13.0)),
                if t.is_some() { palette::text() } else { palette::text_dim() },
            );
        };
        text_col(&row.artist, Col::Artist);
        text_col(&row.album, Col::Album);

        // BPM
        if let Some((_, b1)) = cols.get(Col::Bpm) {
            let bpm_txt = match row.bpm {
                Some(b) => format!("{:.0}", b),
                None if !row.analysed => "…".into(),
                None => "—".into(),
            };
            p.text(
                pos2(x(b1) - 6.0, cy),
                egui::Align2::RIGHT_CENTER,
                bpm_txt,
                FontId::monospace(crate::theme::sz(15.0)),
                if row.bpm_manual { palette::deck_b() } else { palette::text() },
            );
        }

        // Key
        if let Some((k0, _)) = cols.get(Col::Key) {
            if let Some((t, m)) = row.key {
                let key = Key { tonic: t, minor: m, confidence: 0.0 };
                let (n, _) = key.camelot();
                let hue = Color32::from(egui::ecolor::Hsva::new((n as f32 - 1.0) / 12.0, 0.55, 0.95, 1.0));
                p.text(
                    pos2(x(k0) + 4.0, cy),
                    egui::Align2::LEFT_CENTER,
                    key.camelot_label(),
                    FontId::monospace(crate::theme::sz(14.0)),
                    hue,
                );
            } else {
                p.text(
                    pos2(x(k0) + 4.0, cy),
                    egui::Align2::LEFT_CENTER,
                    "—",
                    FontId::monospace(crate::theme::sz(14.0)),
                    palette::text_dim(),
                );
            }
        }

        text_col(&row.genre, Col::Genre);

        // Time
        if let (Some((_, t1)), Some(d)) = (cols.get(Col::Time), row.duration_secs.filter(|d| *d > 0.0)) {
            p.text(
                pos2(x(t1) - 6.0, cy),
                egui::Align2::RIGHT_CENTER,
                format!("{}:{:02}", (d / 60.0) as u32, (d % 60.0) as u32),
                FontId::monospace(crate::theme::sz(13.0)),
                palette::text_dim(),
            );
        }

        // Added
        if let Some((a0, _)) = cols.get(Col::Added) {
            p.text(
                pos2(x(a0) + 4.0, cy),
                egui::Align2::LEFT_CENTER,
                ago(unix_now() - row.added_at, row.added_at),
                FontId::proportional(crate::theme::sz(12.5)),
                palette::text_dim(),
            );
        }

        // Flux
        if let (Some((f0, f1)), Some(f)) = (cols.get(Col::Flux), fl) {
            let col = flux_colour(f.score);
            let bar = Rect::from_min_size(pos2(x(f0) + 2.0, cy - 3.0), vec2((f1 - f0 - 40.0).max(10.0), 6.0));
            p.rect_filled(bar, 3.0, palette::divider());
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f.score as f32 / 100.0, 6.0)), 3.0, col);
            p.text(
                pos2(x(f1) - 2.0, cy),
                egui::Align2::RIGHT_CENTER,
                format!("{}", f.score),
                FontId::monospace(crate::theme::sz(13.0)),
                col,
            );
        }
    }

    fn apply(&mut self, deferred: Vec<Deferred>) {
        let mut reload = false;
        for d in deferred {
            match d {
                Deferred::Select(p) => self.selected = Some(p),
                Deferred::View(v) => {
                    // Flux views rank best-first; other views go back to A-Z.
                    if matches!(v, View::Flux(_)) {
                        self.sort = (Col::Flux, false);
                    } else if self.sort.0 == Col::Flux {
                        self.sort = (Col::Title, true);
                    }
                    self.view = v;
                }
                Deferred::NewReel => {
                    self.new_reel = Some(NewReel { name: String::new(), fresh: true, add_track: None })
                }
                Deferred::NewReelFor(p) => {
                    self.new_reel = Some(NewReel { name: String::new(), fresh: true, add_track: Some(p) })
                }
                Deferred::CancelNewReel => self.new_reel = None,
                Deferred::CommitNewReel => {
                    if let Some(nr) = self.new_reel.take() {
                        let name = nr.name.trim().to_string();
                        if let (false, Some(db)) = (name.is_empty(), &self.db) {
                            if let Ok(id) = db.create_reel(&name) {
                                if let Some(p) = nr.add_track {
                                    let _ = db.add_to_reel(id, &p);
                                    crate::stem_queue::queue().add_reel_track(p);
                                }
                                self.view = View::Reel(id);
                            }
                        }
                        reload = true;
                    }
                }
                Deferred::ImportGrids => {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Choose rekordbox's analysis folder (USBANLZ, or the PIONEER folder above it)")
                        .pick_folder()
                    {
                        self.grid_import = Some(crate::grid_import::GridImportDialog::new(dir, self.rows.clone()));
                    }
                }
                Deferred::ExportReel(id) => {
                    let Some(db) = &self.db else { continue };
                    let name = self.reels.iter().find(|r| r.0 == id).map(|r| r.1.clone()).unwrap_or_default();
                    let rows: Vec<TrackRow> = db
                        .reel_paths(id)
                        .iter()
                        .filter_map(|p| self.by_path.get(p).map(|&i| self.rows[i].clone()))
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    if let Some(dest) =
                        rfd::FileDialog::new().set_title("Choose the USB drive (its top folder)").pick_folder()
                    {
                        self.usb = Some(crate::usb::UsbDialog::new(name, rows, dest));
                    }
                }
                Deferred::AddFolder => {
                    if let (Some(dir), Some(db)) = (rfd::FileDialog::new().pick_folder(), &self.db) {
                        let _ = db.add_source(&dir);
                        self.indexer.rescan();
                        reload = true;
                    }
                }
                Deferred::RemoveSource(dir) => {
                    if let Some(db) = &self.db {
                        let _ = db.remove_source(&dir);
                    }
                    if self.view == View::Source(dir) {
                        self.view = View::All;
                    }
                    reload = true;
                }
                Deferred::AddToReel(id, p) => {
                    if let Some(db) = &self.db {
                        let _ = db.add_to_reel(id, &p);
                        crate::stem_queue::queue().add_reel_track(p);
                    }
                    reload = true;
                }
                Deferred::RemoveFromReel(id, p) => {
                    if let Some(db) = &self.db {
                        let _ = db.remove_from_reel(id, &p);
                    }
                    reload = true;
                }
                Deferred::DeleteReel(id) => {
                    if let Some(db) = &self.db {
                        let _ = db.delete_reel(id);
                    }
                    if self.view == View::Reel(id) {
                        self.view = View::All;
                    }
                    reload = true;
                }
                Deferred::RenameReel(id) => {
                    if let (Some((_, name)), Some(db)) = (self.renaming.take(), &self.db) {
                        if !name.trim().is_empty() {
                            let _ = db.rename_reel(id, &name);
                        }
                    }
                    reload = true;
                }
            }
        }
        if reload {
            self.reload();
        }
    }
}

// ── layout + small widgets ────────────────────────────────────────────────────

/// Narrowest the title may get before optional columns start dropping out.
const MIN_TITLE: f32 = 170.0;

/// Columns after the title, in display order.
const ORDER: [Col; 8] = [Col::Artist, Col::Album, Col::Bpm, Col::Key, Col::Genre, Col::Time, Col::Added, Col::Flux];
/// Which columns give way first when the window is too narrow.
const DROP_FIRST: [Col; 8] =
    [Col::Added, Col::Album, Col::Artist, Col::Genre, Col::Time, Col::Key, Col::Bpm, Col::Flux];

impl Col {
    fn label(self) -> &'static str {
        match self {
            Col::Title => "TRACK",
            Col::Artist => "ARTIST",
            Col::Album => "ALBUM",
            Col::Bpm => "BPM",
            Col::Key => "KEY",
            Col::Genre => "GENRE",
            Col::Time => "TIME",
            Col::Added => "ADDED",
            Col::Flux => "FLUX",
        }
    }
    fn menu_name(self) -> &'static str {
        match self {
            Col::Artist => "Artist",
            Col::Album => "Album",
            Col::Bpm => "BPM",
            Col::Key => "Key",
            Col::Genre => "Genre",
            Col::Time => "Time",
            Col::Added => "Date added",
            Col::Flux => "Flux match",
            Col::Title => "Track",
        }
    }
    fn id(self) -> &'static str {
        self.menu_name()
    }
    fn align_right(self) -> bool {
        matches!(self, Col::Bpm | Col::Time)
    }
    fn sorts_ascending_first(self) -> bool {
        matches!(self, Col::Title | Col::Artist | Col::Album | Col::Genre | Col::Key)
    }
    fn default_width(self) -> f32 {
        match self {
            Col::Title => 0.0,
            Col::Artist | Col::Album => 150.0,
            Col::Bpm => 62.0,
            Col::Key => 54.0,
            Col::Genre => 150.0,
            Col::Time => 60.0,
            Col::Added => 80.0,
            Col::Flux => 96.0,
        }
    }
    fn min_width(self) -> f32 {
        match self {
            Col::Title => 0.0,
            Col::Artist | Col::Album | Col::Genre => 60.0,
            Col::Bpm | Col::Time => 46.0,
            Col::Key => 44.0,
            Col::Added => 56.0,
            Col::Flux => 64.0,
        }
    }
    fn default_visible(self) -> bool {
        !matches!(self, Col::Artist | Col::Album | Col::Added)
    }
}

/// Which columns are shown and how wide, remembered between runs. The title takes the rest.
#[derive(Clone, Debug, PartialEq)]
struct ColumnPrefs {
    width: [f32; 9],
    visible: [bool; 9],
}

impl Default for ColumnPrefs {
    fn default() -> Self {
        let mut p = Self { width: [0.0; 9], visible: [true; 9] };
        for c in ORDER {
            p.width[c as usize] = c.default_width();
            p.visible[c as usize] = c.default_visible();
        }
        p
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SavedColumns {
    #[serde(default)]
    columns: BTreeMap<String, SavedColumn>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedColumn {
    width: f32,
    visible: bool,
}

impl ColumnPrefs {
    fn path() -> PathBuf {
        config_dir().join("library_columns.toml")
    }

    fn load() -> Self {
        let mut p = Self::default();
        let saved: SavedColumns =
            std::fs::read_to_string(Self::path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
        for c in ORDER {
            if let Some(s) = saved.columns.get(c.id()) {
                p.set_width(c, s.width);
                p.visible[c as usize] = s.visible;
            }
        }
        p
    }

    fn save(&self) {
        let saved = SavedColumns {
            columns: ORDER
                .iter()
                .map(|c| {
                    (
                        c.id().to_owned(),
                        SavedColumn { width: self.width[*c as usize], visible: self.visible[*c as usize] },
                    )
                })
                .collect(),
        };
        if let Ok(text) = toml::to_string_pretty(&saved) {
            let _ = std::fs::create_dir_all(config_dir());
            let _ = std::fs::write(Self::path(), text);
        }
    }

    fn set_width(&mut self, c: Col, w: f32) {
        if c != Col::Title && w.is_finite() {
            self.width[c as usize] = w.clamp(c.min_width(), 480.0);
        }
    }

    fn toggle(&mut self, c: Col) {
        self.visible[c as usize] = !self.visible[c as usize];
    }
}

/// Right-click menu on the header.
fn column_menu(ui: &mut egui::Ui, prefs: &ColumnPrefs, toggle: &mut Option<Col>, reset: &mut bool) {
    ui.label(RichText::new("Columns").size(crate::theme::sz(12.0)).color(palette::text_dim()));
    for c in ORDER {
        let mut on = prefs.visible[c as usize];
        if ui.checkbox(&mut on, c.menu_name()).changed() {
            *toggle = Some(c);
        }
    }
    ui.separator();
    if ui.button("Reset columns").clicked() {
        *reset = true;
        ui.close_menu();
    }
}

struct ColLayout {
    preview: (f32, f32),
    title: (f32, f32),
    /// Visible columns after the title, left to right, as (start, end) offsets from the row's left edge.
    slots: Vec<(Col, (f32, f32))>,
}

impl ColLayout {
    fn new(w: f32, prefs: &ColumnPrefs) -> Self {
        let preview = (6.0, 6.0 + PREVIEW_W);
        let title_left = preview.1 + 10.0;
        let mut shown: Vec<Col> = ORDER.into_iter().filter(|c| prefs.visible[*c as usize]).collect();
        let total = |shown: &[Col]| -> f32 { shown.iter().map(|c| prefs.width[*c as usize]).sum() };
        // Narrow window: let the least important columns give way rather than squeezing the title.
        while w - total(&shown) - 4.0 - title_left < MIN_TITLE {
            match DROP_FIRST.iter().find(|c| shown.contains(c)) {
                Some(c) => {
                    let c = *c;
                    shown.retain(|s| *s != c);
                }
                None => break,
            }
        }
        let mut x = w - total(&shown);
        let title = (title_left, (x - 4.0).max(title_left));
        let slots = shown
            .into_iter()
            .map(|c| {
                let r = (x, x + prefs.width[c as usize]);
                x = r.1;
                (c, r)
            })
            .collect();
        Self { preview, title, slots }
    }

    fn get(&self, c: Col) -> Option<(f32, f32)> {
        self.slots.iter().find(|(s, _)| *s == c).map(|(_, r)| *r)
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// "today", "3d", "2w", "5mo", "1y" — how long ago a track was added.
fn ago(secs: i64, added_at: i64) -> String {
    if added_at <= 0 {
        return "—".into();
    }
    let d = secs.max(0) / 86_400;
    match d {
        0 => "today".into(),
        1..=13 => format!("{d}d"),
        14..=59 => format!("{}w", d / 7),
        60..=729 => format!("{}mo", d / 30),
        _ => format!("{}y", d / 365),
    }
}

fn cmp_opt<T: PartialOrd>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Fits text into `width` px given an average glyph width, adding an ellipsis.
fn fit(s: &str, width: f32, glyph_w: f32) -> String {
    truncate(s, ((width / glyph_w).floor() as usize).max(3))
}

fn flux_colour(score: u8) -> Color32 {
    match score {
        85..=100 => Color32::from_rgb(70, 220, 120),
        65..=84 => Color32::from_rgb(160, 220, 70),
        40..=64 => Color32::from_rgb(235, 175, 50),
        _ => Color32::from_rgb(220, 90, 80),
    }
}

/// Section header; returns true when its optional action button was clicked.
fn side_header(ui: &mut egui::Ui, text: &str, button: Option<&str>) -> bool {
    ui.add_space(5.0);
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(crate::theme::sz(11.0)).color(palette::text_dim()).strong());
        if let Some(b) = button {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new(b).size(crate::theme::sz(14.0)).color(palette::deck_a()))
                            .small()
                            .frame(false),
                    )
                    .on_hover_text(if text == "FOLDERS" { "Add a music folder" } else { "New reel" })
                    .clicked()
                {
                    clicked = true;
                }
            });
        }
    });
    clicked
}

fn side_item(
    ui: &mut egui::Ui,
    label: &str,
    count: Option<usize>,
    selected: bool,
    dot: Option<Color32>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 25.0), Sense::click());
    let bg = if selected {
        palette::deck_a().linear_multiply(0.22)
    } else if resp.hovered() {
        palette::hover()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    let mut x = rect.left() + 8.0;
    if let Some(c) = dot {
        ui.painter().circle_filled(pos2(x + 3.0, rect.center().y), 3.5, c);
        x += 12.0;
    }
    ui.painter().text(
        pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate(label, 24),
        FontId::proportional(crate::theme::sz(13.5)),
        if selected { palette::text() } else { palette::text_mid() },
    );
    if let Some(n) = count {
        ui.painter().text(
            pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::monospace(crate::theme::sz(11.5)),
            palette::text_dim(),
        );
    }
    resp
}

/// Mini waveform (same colour recipe as the deck waveform) with hot cue markers.
fn draw_preview(p: &egui::Painter, rect: Rect, preview: &[[u8; 3]], cues: &[MiniCue], duration: f64) {
    p.rect_filled(rect, 2.0, palette::inset());
    let cy = rect.center().y;
    if preview.is_empty() {
        p.line_segment([pos2(rect.left(), cy), pos2(rect.right(), cy)], Stroke::new(1.0_f32, palette::divider()));
    } else {
        crate::wave_draw::mini(p, rect, preview);
    }
    if duration > 0.0 {
        for c in cues {
            let x = rect.left() + (c.time_ms as f64 / 1000.0 / duration).clamp(0.0, 1.0) as f32 * rect.width();
            let col = Color32::from_rgb(c.color[0], c.color[1], c.color[2]);
            if let Some(out) = c.out_time_ms {
                let x1 = rect.left() + (out as f64 / 1000.0 / duration).clamp(0.0, 1.0) as f32 * rect.width();
                p.rect_filled(
                    Rect::from_min_max(pos2(x, rect.top()), pos2(x1.max(x + 2.0), rect.bottom())),
                    0.0,
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 60),
                );
            }
            p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0_f32, col));
            p.add(egui::Shape::convex_polygon(
                vec![pos2(x - 3.5, rect.top() - 2.0), pos2(x + 3.5, rect.top() - 2.0), pos2(x, rect.top() + 4.0)],
                col,
                Stroke::NONE,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_never_overlap_at_any_width_or_selection() {
        let mut all = ColumnPrefs::default();
        for c in ORDER {
            all.visible[c as usize] = true;
        }
        for prefs in [ColumnPrefs::default(), all] {
            for w in (400..=2400).step_by(40) {
                let c = ColLayout::new(w as f32, &prefs);
                assert!(c.preview.1 < c.title.0, "w={w}");
                assert!(
                    c.title.1 - c.title.0 >= MIN_TITLE - 4.0 || c.slots.is_empty(),
                    "title squeezed at w={w}: {:?}",
                    c.title
                );
                let mut edge = c.title.1;
                for (col, (a, b)) in &c.slots {
                    assert!(*a >= edge && b > a, "{col:?} overlaps at w={w}");
                    edge = *b;
                }
                assert!(edge <= w as f32 + 0.01, "w={w}");
            }
        }
    }

    #[test]
    fn narrow_windows_drop_the_least_important_columns_first() {
        let mut all = ColumnPrefs::default();
        for c in ORDER {
            all.visible[c as usize] = true;
        }
        let wide = ColLayout::new(2000.0, &all);
        assert_eq!(wide.slots.len(), 8);
        let narrow = ColLayout::new(700.0, &all);
        assert!(narrow.get(Col::Added).is_none() && narrow.get(Col::Flux).is_some());
    }

    #[test]
    fn widths_are_clamped_and_hidden_columns_take_no_room() {
        let mut p = ColumnPrefs::default();
        p.set_width(Col::Genre, 5.0);
        assert_eq!(p.width[Col::Genre as usize], Col::Genre.min_width());
        p.set_width(Col::Genre, 9000.0);
        assert_eq!(p.width[Col::Genre as usize], 480.0);
        p.set_width(Col::Genre, f32::NAN);
        assert_eq!(p.width[Col::Genre as usize], 480.0);
        p.toggle(Col::Genre);
        assert!(ColLayout::new(1400.0, &p).get(Col::Genre).is_none());
    }

    #[test]
    fn added_dates_read_naturally() {
        assert_eq!(ago(100, 5), "today");
        assert_eq!(ago(3 * 86_400, 5), "3d");
        assert_eq!(ago(21 * 86_400, 5), "3w");
        assert_eq!(ago(120 * 86_400, 5), "4mo");
        assert_eq!(ago(800 * 86_400, 5), "2y");
        assert_eq!(ago(10, 0), "—");
    }

    #[test]
    fn truncation_keeps_short_text_and_adds_an_ellipsis_to_long_text() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long title indeed", 10), "a very lo…");
        assert_eq!(fit("abcdefghij", 40.0, 8.0), "abcd…");
    }

    #[test]
    fn missing_values_sort_before_present_ones() {
        assert_eq!(cmp_opt(Some(1.0), None), std::cmp::Ordering::Greater);
        assert_eq!(cmp_opt::<f32>(None, Some(1.0)), std::cmp::Ordering::Less);
        assert_eq!(cmp_opt(Some(1.0), Some(2.0)), std::cmp::Ordering::Less);
    }
}
