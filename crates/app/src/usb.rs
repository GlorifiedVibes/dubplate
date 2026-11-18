//! "Export to USB": the dialog and the background job around `dubplate_engine::usb_export`.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use dubplate_engine::usb_export::{self, ExportError, ExportItem, ExportReel, ExportReport, SUPPORTED};
use egui::RichText;

use crate::app::palette;
use crate::db::{LibraryDb, TrackRow};

/// What the job reports back.
enum Msg {
    Progress(usize, usize, String),
    Done(Result<ExportReport, String>),
}

enum Stage {
    /// Waiting for the user to confirm.
    Ready,
    Running {
        rx: mpsc::Receiver<Msg>,
        done: usize,
        total: usize,
        what: String,
    },
    Finished(Result<ExportReport, String>),
}

pub struct UsbDialog {
    reel_name: String,
    rows: Vec<TrackRow>,
    dest: PathBuf,
    /// Tracks players can't play, and tracks never analysed (no beat grid or waveform).
    unsupported: usize,
    unanalysed: usize,
    foreign: bool,
    replace_foreign: bool,
    stage: Stage,
}

pub fn is_playable(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| SUPPORTED.contains(&e.to_ascii_lowercase().as_str()))
}

impl UsbDialog {
    /// `rows` are the reel's tracks in order.
    pub fn new(reel_name: String, rows: Vec<TrackRow>, dest: PathBuf) -> Self {
        let unsupported = rows.iter().filter(|r| !is_playable(&r.path)).count();
        let unanalysed = rows.iter().filter(|r| is_playable(&r.path) && !r.analysed).count();
        UsbDialog {
            reel_name,
            foreign: usb_export::has_foreign_library(&dest),
            rows,
            dest,
            unsupported,
            unanalysed,
            replace_foreign: false,
            stage: Stage::Ready,
        }
    }

    /// Draws the window; returns false once the user has closed it.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        self.poll();
        let mut open = true;
        let mut start = false;
        egui::Window::new(format!("Export “{}” to USB", self.reel_name))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                match &self.stage {
                    Stage::Ready => {
                        ui.label(format!("{} tracks → {}", self.rows.len(), self.dest.display()));
                        ui.add_space(4.0);
                        ui.label(RichText::new("Copies the audio and writes the database, beat grids, waveforms and cues \
                            that CDJ and XDJ players read, so the drive shows this reel as a playlist.")
                            .size(crate::theme::sz(12.5)).color(palette::text_dim()));
                        if self.unsupported > 0 {
                            ui.colored_label(palette::warn(), format!(
                                "{} track(s) are a type players can't play and will be left off (players take MP3, AAC, WAV, AIFF, FLAC).",
                                self.unsupported));
                        }
                        if self.unanalysed > 0 {
                            ui.colored_label(palette::warn(), format!(
                                "{} track(s) haven't been analysed yet, so they'll have no beat grid or waveform on the player. \
                                 Load them in a deck once to analyse them.", self.unanalysed));
                        }
                        if self.foreign {
                            ui.add_space(4.0);
                            ui.colored_label(palette::bad(), "This drive already holds a rekordbox library. Exporting replaces its database \
                                (your audio files are not touched).");
                            ui.checkbox(&mut self.replace_foreign, "Replace the library on this drive");
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new("Not yet tried on a real player — please report how it goes.")
                            .size(crate::theme::sz(12.0)).color(palette::text_dim()));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let ready = !self.foreign || self.replace_foreign;
                            if ui.add_enabled(ready && !self.rows.is_empty(), egui::Button::new("Export")).clicked() { start = true; }
                        });
                    }
                    Stage::Running { done, total, what, .. } => {
                        let frac = if *total == 0 { 0.0 } else { *done as f32 / *total as f32 };
                        ui.add(egui::ProgressBar::new(frac).text(format!("{done} / {total}")));
                        ui.label(RichText::new(what).size(crate::theme::sz(12.5)).color(palette::text_dim()));
                        ui.label(RichText::new("Keep the drive plugged in until this finishes.").size(crate::theme::sz(12.0)).color(palette::text_dim()));
                    }
                    Stage::Finished(Ok(r)) => {
                        ui.colored_label(palette::ok(), format!("Exported {} tracks.", r.tracks));
                        ui.label(format!("{} copied ({:.0} MB), {} already on the drive.",
                            r.copied, r.bytes_copied as f64 / 1_048_576.0, r.reused));
                        if !r.skipped.is_empty() {
                            ui.colored_label(palette::warn(), format!("{} left off:", r.skipped.len()));
                            for (p, why) in r.skipped.iter().take(12) {
                                ui.label(RichText::new(format!("{} — {why}", p.file_name().and_then(|f| f.to_str()).unwrap_or("?")))
                                    .size(crate::theme::sz(12.0)).color(palette::text_dim()));
                            }
                        }
                        ui.add_space(4.0);
                        ui.label("Eject the drive from your system before unplugging it.");
                    }
                    Stage::Finished(Err(e)) => { ui.colored_label(palette::bad(), e); }
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
        let (name, rows, dest, replace) =
            (self.reel_name.clone(), self.rows.clone(), self.dest.clone(), self.replace_foreign);
        let total = rows.len();
        std::thread::spawn(move || {
            let items = build_items(&rows, LibraryDb::open().ok().as_ref());
            let reels = [ExportReel { name, tracks: items }];
            let progress_tx = tx.clone();
            let result = usb_export::export_usb(&dest, &reels, replace, &mut |d, t, what| {
                let _ = progress_tx.send(Msg::Progress(d, t, what.to_owned()));
            });
            let _ = tx.send(Msg::Done(result.map_err(|e| match e {
                ExportError::WouldReplaceForeignLibrary => {
                    "The drive holds a rekordbox library; tick the box to replace it.".to_owned()
                }
                ExportError::Io(e) => format!("Export failed: {e}. Is the drive full or unplugged?"),
            })));
        });
        self.stage = Stage::Running { rx, done: 0, total, what: "Reading the library…".into() };
    }

    fn poll(&mut self) {
        let Stage::Running { rx, done, total, what } = &mut self.stage else { return };
        let mut finished = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Progress(d, t, w) => {
                    *done = d;
                    *total = t;
                    *what = w;
                }
                Msg::Done(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            self.stage = Stage::Finished(r);
        }
    }
}

/// Everything Dubplate knows about each track, gathered from the library, the analysis cache and the
/// cue table. Tracks that were never analysed export without a beat grid or waveform.
pub fn build_items(rows: &[TrackRow], db: Option<&LibraryDb>) -> Vec<ExportItem> {
    rows.iter()
        .map(|r| {
            let cached = dubplate_engine::cache::load(&r.path);
            let cues = db.map(|d| d.load_cues(&r.path)).unwrap_or_default();
            ExportItem {
                source: r.path.clone(),
                title: r.display_title(),
                artist: r.artist.clone().unwrap_or_default(),
                album: r.album.clone().unwrap_or_default(),
                genre: r.genre.clone().unwrap_or_default(),
                key: r.key,
                bpm: r.bpm.or(cached.as_ref().map(|c| c.bpm)).unwrap_or(0.0),
                year: r.year.unwrap_or(0).min(u16::MAX as u32) as u16,
                duration_secs: r.duration_secs.or(cached.as_ref().map(|c| c.duration_secs)).unwrap_or(0.0),
                sample_rate: cached.as_ref().map_or(44100, |c| c.sample_rate),
                beat_times_ms: cached.as_ref().map(|c| c.beat_times_ms.clone()).unwrap_or_default(),
                waveform: cached.as_ref().map(|c| c.waveform.clone()).unwrap_or_default(),
                cues,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, analysed: bool) -> TrackRow {
        TrackRow {
            id: 1,
            path: PathBuf::from(path),
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: None,
            bpm: None,
            bpm_manual: false,
            key: None,
            duration_secs: None,
            preview: vec![],
            added_at: 0,
            analysed,
        }
    }

    #[test]
    fn only_formats_players_can_play_are_exported() {
        assert!(is_playable(Path::new("/a/b.MP3")));
        assert!(is_playable(Path::new("x.flac")));
        assert!(!is_playable(Path::new("x.ogg")));
        assert!(!is_playable(Path::new("noext")));
    }

    #[test]
    fn the_dialog_counts_what_will_be_left_off() {
        let rows = vec![row("/a.mp3", true), row("/b.mp3", false), row("/c.ogg", true)];
        let d = UsbDialog::new("R".into(), rows, PathBuf::from("/definitely/not/here"));
        assert_eq!((d.unsupported, d.unanalysed), (1, 1));
        assert!(!d.foreign);
    }

    #[test]
    fn items_fall_back_to_the_file_name_and_defaults() {
        let items = build_items(&[row("/music/Some Song.mp3", false)], None);
        assert_eq!(items[0].title, "Some Song");
        assert_eq!(items[0].sample_rate, 44100);
        assert!(items[0].beat_times_ms.is_empty() && items[0].cues.is_empty());
    }
}
