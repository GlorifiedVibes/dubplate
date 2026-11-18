use dubplate_engine::{DeckCommand, DeckHandle, STEM_SHORTS, WAVEFORM_CHUNK_FRAMES};
use egui::{Color32, RichText, Sense, Stroke};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::app::{palette, DeckAction};
use crate::waveform::{beat_info, draw_overview, draw_waveform, format_time, format_time_tenths, Marks};
use crate::widgets::{transport_btn, vertical_fader};

const PLATTER_MAX: f32 = 176.0;
/// 33⅓ rpm: one platter turn per 1.8 s of audio at normal speed.
const PLATTER_RAD_PER_SEC: f64 = 2.0 * std::f64::consts::PI * (100.0 / 3.0) / 60.0;
const INFO_H: f32 = 292.0;
/// The BPM above the tempo, as designed (egui's button size).
const BPM_SIZE: f32 = 14.0;

/// How much taller than designed the deck's readouts and labels are in the theme's sizes; the deck row grows by it.
pub fn extra_height() -> f32 {
    let t = crate::theme::get();
    let bpm = crate::theme::bpm_size(BPM_SIZE) - BPM_SIZE;
    let time = crate::theme::time_size(22.0) - 22.0;
    // the title, key and stem rows and the labels on the buttons grow with the text
    bpm.max(0.0) + time.max(0.0) + (t.text_scale - 1.0).max(0.0) * 30.0
}
const BTN_H_MAIN: f32 = 36.0;

/// A user edit to the beat grid; applied and persisted by the app.
#[derive(Clone, Copy, Debug)]
pub enum GridEdit {
    SetBpm(f32),
    Scale(f32),
    SetDownbeat(f64),
    Reset,
}

/// Things the deck UI asks the app to do (the app owns persistence and the other deck).
#[derive(Default)]
pub struct DeckEvents {
    pub grid_edit: Option<GridEdit>,
    pub tempo_target: Option<f32>,
    pub sync_click: bool,
    /// The KEY LOCK button was clicked.
    pub key_lock_click: bool,
    /// Key shift buttons: +1 / -1 semitone, 0 = back to the track's own key.
    pub key_shift: Option<i32>,
    pub key_sync_click: bool,
    /// The PREP button: open this deck's track in the Prepare screen.
    pub prepare: bool,
    /// Cue / hot-cue / loop button actions (same type MIDI produces).
    pub actions: Vec<DeckAction>,
}

#[derive(Clone, Copy, PartialEq)]
enum JogZone {
    None,
    Top,
    Side,
}

pub struct DeckView {
    pub deck_idx: usize,
    pending_commands: Vec<DeckCommand>,
    tempo_fader_val: f32,
    tempo_dragging: bool,
    events: DeckEvents,
    pub sync_on: bool,
    pub key_lock_on: bool,
    /// (track key, shift, key now heard, its note name) when the track has a key.
    pub key_info: Option<(String, i32, String, String)>,
    pub key_sync_enabled: bool,
    pub sync_enabled: bool,
    pub sync_limited: bool,
    /// While SYNC is on and both decks play: whether the beats are locked (within a few milliseconds) or still
    /// being pulled together. `None` when there is nothing to lock to.
    pub sync_lock: Option<bool>,
    cue_down_prev: bool,
    pad_down_prev: [bool; 8],
    jog_zone: JogZone,
    jog_last_angle: Option<f32>,
    jog_omega: f32,
    jog_sent: f32,
    platter_phase: f32,
    pub waveform_zoom: f32,
    /// Show a row per stem under the waveform (once the deck has stems).
    pub lanes_on: bool,
    /// What to draw as lanes this frame: set by the app when the lanes are on and the deck has stems and their summaries.
    pub lane_state: Lanes,
    /// The name of a track on its way onto this deck (being read and analysed), set by the app each frame.
    pub loading: Option<String>,
    playhead: dubplate_engine::playhead::Playhead,
}

/// Height of one stem lane under the waveform, in points, when there is room.
pub const LANE_H: f32 = 17.0;

/// The stems' summaries and gains to draw as lanes.
pub type Lanes = Option<(std::sync::Arc<dubplate_engine::StemWaves>, [f32; 4])>;

impl DeckView {
    pub fn new(deck_idx: usize) -> Self {
        Self {
            deck_idx,
            pending_commands: Vec::new(),
            tempo_fader_val: 0.0,
            tempo_dragging: false,
            events: DeckEvents::default(),
            sync_on: false,
            key_lock_on: false,
            key_info: None,
            key_sync_enabled: false,
            sync_enabled: false,
            sync_limited: false,
            sync_lock: None,
            cue_down_prev: false,
            pad_down_prev: [false; 8],
            jog_zone: JogZone::None,
            jog_last_angle: None,
            jog_omega: 0.0,
            jog_sent: 0.0,
            platter_phase: 0.0,
            waveform_zoom: 1.0,
            lanes_on: crate::wave_draw::lanes_default(),
            lane_state: None,
            loading: None,
            playhead: dubplate_engine::playhead::Playhead::new(),
        }
    }

    pub fn drain_commands(&mut self) -> Vec<DeckCommand> {
        std::mem::take(&mut self.pending_commands)
    }

    pub fn take_events(&mut self) -> DeckEvents {
        std::mem::take(&mut self.events)
    }

    pub fn is_jogging(&self) -> bool {
        self.jog_zone != JogZone::None
    }

    pub fn accent(&self) -> Color32 {
        if self.deck_idx == 0 {
            palette::deck_a()
        } else {
            palette::deck_b()
        }
    }

    fn label(&self) -> &'static str {
        if self.deck_idx == 0 {
            "A"
        } else {
            "B"
        }
    }

    // ── Full-width scrolling waveform ────────────────────────────────────────

    pub fn show_waveform_rect(&mut self, ui: &mut egui::Ui, rect: egui::Rect, deck: &DeckHandle) -> Option<PathBuf> {
        let accent = self.accent();
        let lanes = self.lane_state.clone();
        // With lanes, the waveform keeps the top and the four stems take a strip along the bottom.
        let lanes_h = if lanes.is_some() { (rect.height() * 0.42).min(LANE_H * 4.0) } else { 0.0 };
        let lanes_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - lanes_h), rect.max);
        let rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.bottom() - lanes_h));
        // The audio thread publishes its position in blocks; smooth it so the waveform scrolls evenly.
        let progress = match &deck.track {
            Some(t) => {
                let dur = t.duration_frames.max(1) as f64;
                let observed = deck.shared.position_frames.load(Ordering::Relaxed) as f64;
                let rate = t.sample_rate as f64 * deck.shared.tempo_ratio() as f64;
                let playing = deck.shared.is_playing.load(Ordering::Relaxed);
                (self.playhead.update(observed, rate, playing, std::time::Instant::now(), 0.06 * t.sample_rate as f64)
                    / dur)
                    .clamp(0.0, 1.0)
            }
            None => deck.shared.progress(),
        };
        if let Some(t) = &deck.track {
            let secs = deck.shared.position_frames.load(Ordering::Relaxed) as f64 / t.sample_rate as f64;
            self.platter_phase = (secs * PLATTER_RAD_PER_SEC).rem_euclid(std::f64::consts::TAU) as f32;
        }
        let is_loaded = deck.shared.is_loaded.load(Ordering::Relaxed);

        let id = egui::Id::new("wf_rect").with(self.deck_idx);
        let resp = ui.interact(rect, id, Sense::click_and_drag());

        let scroll = ui.input(|i| {
            if i.pointer.hover_pos().map(|p| rect.contains(p)).unwrap_or(false) {
                i.smooth_scroll_delta.y
            } else {
                0.0
            }
        });
        if scroll != 0.0 {
            let factor = (scroll * 0.004).exp();
            self.waveform_zoom = (self.waveform_zoom * factor).clamp(0.25, 8.0);
        }

        let mut dnd_path = None;
        if let Some(payload) = resp.dnd_release_payload::<PathBuf>() {
            dnd_path = Some((*payload).clone());
        }
        let hovering = resp.dnd_hover_payload::<PathBuf>().is_some();

        let painter = ui.painter_at(rect);

        if let Some(track) = &deck.track {
            let eq = [deck.shared.eq_lo(), deck.shared.eq_mid(), deck.shared.eq_hi()];
            let filter_val = if deck.shared.effect_mode_id() == 0 { deck.shared.filter_val() } else { 0.5 };
            draw_waveform(
                &painter,
                rect,
                track,
                deck.grid,
                &Marks::of(deck),
                progress,
                accent,
                eq,
                filter_val,
                self.waveform_zoom,
            );
            if !hovering && (resp.clicked() || resp.dragged()) {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let n = track.duration_frames as f64 / WAVEFORM_CHUNK_FRAMES as f64;
                    let target = (progress * n + ((pos.x - rect.center().x) / self.waveform_zoom) as f64).clamp(0.0, n);
                    self.pending_commands.push(DeckCommand::Seek(target / n));
                }
            }
        } else {
            painter.rect_filled(rect, 0.0, palette::surface());
            let hint = if hovering {
                "Drop to load"
            } else if self.loading.is_some() {
                ""
            } else {
                "Drag a track here"
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                hint,
                egui::FontId::proportional(crate::theme::sz(11.0)),
                if hovering { accent } else { palette::text_dim() },
            );
        }

        if hovering {
            painter.rect_stroke(rect.shrink(2.0), 3.0, Stroke::new(2.0_f32, accent));
        }
        // a track on its way: say so, so the second or two it takes does not look like nothing happened
        if let Some(name) = &self.loading {
            let name: String = name.chars().take(60).collect();
            let dots = ".".repeat(1 + (ui.input(|i| i.time) * 3.0) as usize % 3);
            let galley = painter.layout_no_wrap(
                format!("Loading {name} {dots}"),
                egui::FontId::proportional(crate::theme::sz(12.0)),
                accent,
            );
            let size = galley.size() + egui::vec2(16.0, 8.0);
            let at =
                if is_loaded { egui::pos2(rect.left() + 34.0, rect.top() + 4.0) } else { rect.center() - size / 2.0 };
            let pill = egui::Rect::from_min_size(at, size);
            painter.rect_filled(pill, 4.0, palette::alpha(palette::background(), 230));
            painter.rect_stroke(pill, 4.0, Stroke::new(1.0_f32, accent.linear_multiply(0.6)));
            painter.galley(pill.min + egui::vec2(8.0, 4.0), galley, accent);
        }

        if let (Some((waves, gains)), Some(track)) = (&lanes, &deck.track) {
            let head = progress * track.duration_frames as f64;
            let row_h = lanes_rect.height() / 4.0;
            let p = ui.painter_at(lanes_rect);
            p.rect_filled(lanes_rect, 0.0, palette::background());
            for i in 0..4 {
                let row = egui::Rect::from_min_size(
                    egui::pos2(lanes_rect.left(), lanes_rect.top() + row_h * i as f32),
                    egui::vec2(lanes_rect.width(), row_h),
                );
                p.line_segment([row.left_top(), row.right_top()], Stroke::new(1.0_f32, palette::raised()));
                crate::wave_draw::lane(&p, row, &waves.0[i], head, self.waveform_zoom, palette::stems()[i], gains[i]);
                p.text(
                    row.left_center() + egui::vec2(5.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    dubplate_engine::STEM_SHORTS[i],
                    egui::FontId::monospace(crate::theme::sz(9.0)),
                    palette::stems()[i].linear_multiply(if gains[i] < 0.01 { 0.4 } else { 0.9 }),
                );
            }
            p.line_segment(
                [
                    egui::pos2(lanes_rect.center().x, lanes_rect.top()),
                    egui::pos2(lanes_rect.center().x, lanes_rect.bottom()),
                ],
                Stroke::new(1.5_f32, palette::text_bright()),
            );
        }

        // Deck badge
        let badge = egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 7.0), egui::vec2(18.0, 15.0));
        painter.rect_filled(badge, 3.0, accent.linear_multiply(0.85));
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            self.label(),
            egui::FontId::monospace(crate::theme::sz(9.0)),
            palette::background(),
        );

        // Track title overlay
        if is_loaded {
            if let Some(track) = &deck.track {
                painter.text(
                    egui::pos2(rect.min.x + 32.0, rect.min.y + 8.0),
                    egui::Align2::LEFT_TOP,
                    &track.title,
                    egui::FontId::proportional(crate::theme::sz(11.0)),
                    Color32::from_rgba_unmultiplied(220, 220, 255, 150),
                );
            }
        }

        dnd_path
    }

    // ── Deck strip: [platter + fader] and [info/controls]. Only the platter/fader
    //    unit mirrors between decks; the info section is identical for A and B.

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        deck: &DeckHandle,
        stems_loaded: bool,
        stem_analyzing: bool,
        stem_progress: u32,
        stem_gains: &mut [f32; 4],
        stem_available: bool,
    ) -> (bool, bool, [Option<f32>; 4]) {
        let accent = self.accent();
        let shared = &deck.shared;
        let is_loaded = shared.is_loaded.load(Ordering::Relaxed);
        let is_playing = shared.is_playing.load(Ordering::Relaxed);
        let progress = shared.progress();
        let tempo_pct = deck.tempo_percent_vs_nominal();
        let tempo_ratio = shared.tempo_ratio();
        let position_frames = shared.position_frames.load(Ordering::Relaxed);

        let bpm_str = if deck.grid.is_valid() { format!("{:.0}", deck.target_bpm) } else { "---".into() };
        let tempo_str = format!("{:+.1}%", tempo_pct);

        let mut open_file = false;
        let mut analyze_stems = false;
        let mut gain_changes: [Option<f32>; 4] = [None; 4];

        const FADER_W: f32 = 34.0;
        const GAP: f32 = 8.0;
        const INFO_GAP: f32 = 14.0;
        const INFO_MIN: f32 = 270.0;

        let full = ui.max_rect().shrink2(egui::vec2(10.0, 8.0));
        let sz = (full.width() - FADER_W - GAP - INFO_GAP - INFO_MIN).clamp(110.0, PLATTER_MAX).min(full.height());
        let unit_w = sz + GAP + FADER_W;
        let block_h = (INFO_H + extra_height()).min(full.height());
        let block_top = full.top() + 2.0;
        let unit_top = block_top + (block_h - sz) / 2.0;
        let size = egui::vec2(sz, sz);

        let (platter_rect, fader_rect, info_rect) = if self.deck_idx == 0 {
            let p = egui::Rect::from_min_size(egui::pos2(full.left(), unit_top), size);
            let f = egui::Rect::from_min_size(egui::pos2(p.right() + GAP, unit_top), egui::vec2(FADER_W, sz));
            let i = egui::Rect::from_min_max(
                egui::pos2(full.left() + unit_w + INFO_GAP, block_top),
                egui::pos2(full.right(), full.bottom()),
            );
            (p, f, i)
        } else {
            let p = egui::Rect::from_min_size(egui::pos2(full.right() - sz, unit_top), size);
            let f = egui::Rect::from_min_size(egui::pos2(p.left() - GAP - FADER_W, unit_top), egui::vec2(FADER_W, sz));
            let i = egui::Rect::from_min_max(
                egui::pos2(full.left(), block_top),
                egui::pos2(full.right() - unit_w - INFO_GAP, full.bottom()),
            );
            (p, f, i)
        };

        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(info_rect).layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.set_clip_rect(info_rect.intersect(ui.clip_rect()));
                self.draw_info_panel(
                    ui,
                    deck,
                    accent,
                    is_loaded,
                    is_playing,
                    progress,
                    tempo_pct,
                    tempo_ratio,
                    position_frames,
                    stems_loaded,
                    stem_analyzing,
                    stem_progress,
                    stem_gains,
                    stem_available,
                    &mut open_file,
                    &mut analyze_stems,
                    &mut gain_changes,
                );
            },
        );

        let (secs_now, secs_total) =
            deck.track.as_ref().map(|t| (progress * t.duration_secs, t.duration_secs)).unwrap_or((0.0, 0.0));
        let frames = deck.track.as_ref().map_or(1.0, |t| t.duration_frames.max(1) as f64);
        let at = |f: f64| (f / frames).clamp(0.0, 1.0) as f32;
        let marks = PlatterMarks {
            cues: deck
                .cues
                .iter()
                .filter(|c| c.hot_cue.is_some())
                .map(|c| (at(c.pos_frames), Color32::from_rgb(c.color[0], c.color[1], c.color[2])))
                .collect(),
            main_cue: is_loaded.then(|| at(deck.cue_point)),
            beat: if is_playing {
                deck.beat_now().map(|b| (b.rem_euclid(1.0) as f32, b.floor() as i64 % 4 == 0))
            } else {
                None
            },
        };
        self.draw_platter_widget(
            ui,
            platter_rect,
            progress,
            is_loaded,
            (secs_total - secs_now).max(0.0),
            if is_loaded { format_time_tenths(secs_now) } else { String::new() },
            is_playing,
            accent,
            &bpm_str,
            &tempo_str,
            &marks,
        );

        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(fader_rect).layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                // a little wider than the fader, for the BPM and percentage under it in larger text
                ui.set_clip_rect(fader_rect.expand2(egui::vec2(8.0, 0.0)).intersect(ui.clip_rect()));
                self.draw_tempo_fader(ui, deck, fader_rect.height());
            },
        );

        (open_file, analyze_stems, gain_changes)
    }

    /// Whole-BPM tempo fader beside the platter. Up = faster,
    /// the centre notch is the track's nominal BPM, double-click resets.
    fn draw_tempo_fader(&mut self, ui: &mut egui::Ui, deck: &DeckHandle, height: f32) {
        let accent = self.accent();
        let loaded = deck.track.is_some();
        let nominal = deck.nominal_bpm();
        let (lo, hi) = deck.tempo_bounds();
        if !self.tempo_dragging {
            self.tempo_fader_val = deck.target_bpm - nominal;
        }

        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.add(
                egui::Label::new(
                    RichText::new("TEMPO").size(crate::theme::sz(9.0)).color(palette::text_dim()).monospace(),
                )
                .extend(),
            );
            let fader_h = (height - 62.0 * crate::theme::get().text_scale).max(40.0);
            let mut v = self.tempo_fader_val;
            let r = ui
                .add_enabled_ui(loaded, |ui| {
                    vertical_fader(ui, &mut v, lo - nominal, hi - nominal, fader_h, "", accent, Some(0.0))
                })
                .inner;
            self.tempo_dragging = r.dragged();
            if r.changed() {
                self.tempo_fader_val = v;
                let target = (nominal + v).round().clamp(lo, hi);
                if target != deck.target_bpm {
                    self.events.tempo_target = Some(target);
                }
            }
            let pct = deck.tempo_percent_vs_nominal();
            ui.label(
                RichText::new(if deck.grid.is_valid() { format!("{:.0}", deck.target_bpm) } else { "--".into() })
                    .size(crate::theme::sz(11.0))
                    .color(if pct.abs() > 0.05 { accent } else { palette::text() })
                    .monospace()
                    .strong(),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{:+.1}%", pct))
                        .size(crate::theme::sz(9.0))
                        .color(palette::text_dim())
                        .monospace(),
                )
                .extend(),
            );
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_info_panel(
        &mut self,
        ui: &mut egui::Ui,
        deck: &DeckHandle,
        accent: Color32,
        is_loaded: bool,
        is_playing: bool,
        progress: f64,
        tempo_pct: f32,
        _tempo_ratio: f32,
        position_frames: u64,
        stems_loaded: bool,
        stem_analyzing: bool,
        stem_progress: u32,
        stem_gains: &mut [f32; 4],
        stem_available: bool,
        open_file: &mut bool,
        analyze_stems: &mut bool,
        gain_changes: &mut [Option<f32>; 4],
    ) {
        // ── Row 1: badge + title + open ──────────────────────────────────────
        ui.horizontal(|ui| {
            let badge = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(20.0, 17.0));
            ui.allocate_rect(badge, Sense::hover());
            ui.painter().rect_filled(badge, 3.0, accent);
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                self.label(),
                egui::FontId::monospace(crate::theme::sz(10.0)),
                palette::background(),
            );
            ui.add_space(5.0);

            let title = deck.track.as_ref().map(|t| t.title.as_str()).unwrap_or("— no track —");
            let avail = (ui.available_width() - 70.0).max(10.0);
            let max_chars = (avail / 6.5) as usize;
            let title_short: String = if title.chars().count() > max_chars {
                let keep = max_chars.saturating_sub(1);
                let end = title.char_indices().nth(keep).map(|(i, _)| i).unwrap_or(title.len());
                format!("{}…", &title[..end])
            } else {
                title.to_string()
            };
            ui.label(
                RichText::new(title_short)
                    .color(if is_loaded { palette::text() } else { palette::text_dim() })
                    .size(crate::theme::sz(11.0))
                    .strong(),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⏏").clicked() {
                    *open_file = true;
                }
                if is_loaded
                    && !is_playing
                    && ui
                        .small_button(RichText::new("PREP").size(crate::theme::sz(10.0)))
                        .on_hover_text("Check and fix this track's beat grid")
                        .clicked()
                {
                    self.events.prepare = true;
                }
                if is_loaded {
                    ui.menu_button(RichText::new("GRID").size(crate::theme::sz(10.0)), |ui| {
                        ui.label(RichText::new("Beat grid").size(crate::theme::sz(10.0)).color(palette::text_dim()));
                        let valid = deck.grid.is_valid();
                        if ui.add_enabled(valid, egui::Button::new("Halve BPM")).clicked() {
                            self.events.grid_edit = Some(GridEdit::Scale(0.5));
                            ui.close_menu();
                        }
                        if ui.add_enabled(valid, egui::Button::new("Double BPM")).clicked() {
                            self.events.grid_edit = Some(GridEdit::Scale(2.0));
                            ui.close_menu();
                        }
                        if ui.button("Set beat 1 at playhead").clicked() {
                            self.events.grid_edit =
                                Some(GridEdit::SetDownbeat(position_frames as f64 / WAVEFORM_CHUNK_FRAMES as f64));
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(deck.grid != deck.analyzed_grid, egui::Button::new("Reset BPM and grid"))
                            .clicked()
                        {
                            self.events.grid_edit = Some(GridEdit::Reset);
                            ui.close_menu();
                        }
                    });
                }
            });
        });

        ui.add_space(6.0);

        // ── Row 2: BPM + tempo% + beat dots + BAR ─────────────────────────────
        ui.horizontal(|ui| {
            // Editable track BPM: drag or double-click to type. The edit is saved per track.
            let mut bpm_val = deck.nominal_bpm();
            let editable = is_loaded && deck.grid.is_valid();
            let bpm_resp = ui
                .scope(|ui| {
                    // a DragValue draws in the Button text style, so that is what sets the BPM's size
                    let font = egui::FontId::proportional(crate::theme::bpm_size(BPM_SIZE));
                    ui.style_mut().text_styles.insert(egui::TextStyle::Button, font);
                    ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
                    ui.visuals_mut().widgets.inactive.fg_stroke = Stroke::new(1.0_f32, accent);
                    ui.visuals_mut().widgets.hovered.fg_stroke = Stroke::new(1.0_f32, accent);
                    ui.visuals_mut().widgets.active.fg_stroke = Stroke::new(1.0_f32, accent);
                    ui.add_enabled(
                        editable,
                        egui::DragValue::new(&mut bpm_val).speed(0.1).range(40.0_f32..=250.0_f32).fixed_decimals(0),
                    )
                })
                .inner;
            if bpm_resp.changed() && editable {
                self.events.grid_edit = Some(GridEdit::SetBpm(bpm_val));
            }
            bpm_resp.on_hover_text("Track BPM (whole number) — drag or double-click to edit, saved per track");
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("{:+.1}%", tempo_pct))
                    .size(crate::theme::sz(11.0))
                    .color(if tempo_pct.abs() > 0.05 { accent } else { palette::text_dim() })
                    .monospace(),
            );

            if let Some(track) = &deck.track {
                if let Some((beat_in_bar, bar_num)) = beat_info(track, deck.grid, position_frames) {
                    ui.add_space(10.0);
                    for dot in 0..4usize {
                        let is_cur = dot == beat_in_bar;
                        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
                        let col = if is_cur {
                            if dot == 0 {
                                accent
                            } else {
                                palette::text_bright()
                            }
                        } else {
                            palette::alpha(palette::text_bright(), 40)
                        };
                        ui.painter_at(dot_rect).circle_filled(
                            dot_rect.center(),
                            if is_cur { 3.8_f32 } else { 2.0_f32 },
                            col,
                        );
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("BAR {bar_num}"))
                            .size(crate::theme::sz(10.0))
                            .color(palette::text_dim())
                            .monospace(),
                    );
                }
            }
        });

        // ── Row 3: remaining time (left-aligned) + elapsed / total ────────────
        ui.horizontal(|ui| {
            let (pos_secs, dur_secs) =
                deck.track.as_ref().map(|t| (progress * t.duration_secs, t.duration_secs)).unwrap_or((0.0, 0.0));
            let rem_secs = (dur_secs - pos_secs).max(0.0);
            let rem_str = if is_loaded { format!("-{}", format_time_tenths(rem_secs)) } else { "-00:00.0".into() };
            ui.label(RichText::new(rem_str).color(accent).monospace().size(crate::theme::time_size(22.0)).strong());
            ui.add_space(8.0);
            let (e, t) = if is_loaded {
                (format_time(pos_secs), format_time(dur_secs))
            } else {
                ("00:00".into(), "00:00".into())
            };
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{e} / {t}"))
                        .color(palette::text_dim())
                        .monospace()
                        .size(crate::theme::sz(11.0)),
                )
                .truncate(),
            );
        });

        ui.add_space(4.0);

        // ── Row 3: Overview strip ──────────────────────────────────────────────
        let avail_w = ui.available_width();
        let lanes = self.lane_state.clone();
        let (ov_rect, ov_resp) = ui.allocate_exact_size(
            egui::vec2(avail_w, if lanes.is_some() { 40.0 } else { 26.0 }),
            Sense::click_and_drag(),
        );
        if let Some(track) = &deck.track {
            draw_overview(
                &ui.painter_at(ov_rect),
                ov_rect,
                track,
                deck.grid,
                &Marks::of(deck),
                progress,
                accent,
                lanes.as_ref().map(|(w, g)| (&**w, *g)),
            );
            if let Some(pos) = ov_resp.hover_pos() {
                if let Some(text) =
                    crate::sections::describe(track, deck.grid, ((pos.x - ov_rect.left()) / ov_rect.width()) as f64)
                {
                    ov_resp.clone().on_hover_text(text);
                }
            }
            if ov_resp.clicked() || ov_resp.dragged() {
                if let Some(pos) = ov_resp.interact_pointer_pos() {
                    let t = ((pos.x - ov_rect.left()) / ov_rect.width()).clamp(0.0, 1.0) as f64;
                    self.pending_commands.push(DeckCommand::Seek(t));
                }
            }
        } else {
            ui.painter_at(ov_rect).rect_filled(ov_rect, 2.0, palette::surface());
        }

        ui.add_space(5.0);

        // ── Row 4: Transport ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let cue_r = transport_btn(ui, "CUE", false, is_loaded, egui::vec2(48.0, BTN_H_MAIN), palette::cue());
            let down = is_loaded && cue_r.is_pointer_button_down_on();
            if down && !self.cue_down_prev {
                let shift = ui.input(|i| i.modifiers.shift);
                self.events.actions.push(if shift { DeckAction::ToStart } else { DeckAction::CueDown });
            }
            if !down && self.cue_down_prev { self.events.actions.push(DeckAction::CueUp); }
            self.cue_down_prev = down;
            cue_r.on_hover_text("Playing: return to cue and pause. Stopped on the cue: hold to preview. Stopped elsewhere: set the cue point. Shift: go to start.");

            let armed = deck.shared.armed.load(Ordering::Relaxed);
            let play_lbl = if armed { "◔ WAIT" } else if is_playing { "⏸ PAUSE" } else { "▶ PLAY" };
            let play_r = transport_btn(ui, play_lbl, is_playing || armed, is_loaded, egui::vec2(74.0, BTN_H_MAIN), palette::play());
            if play_r.clicked() {
                self.pending_commands.push(if is_playing || armed { DeckCommand::Pause } else { DeckCommand::Play });
            }
            let sync_col = if self.sync_limited { palette::warn() } else { accent };
            let sync_r = transport_btn(ui, "SYNC", self.sync_on, self.sync_enabled, egui::vec2(46.0, BTN_H_MAIN), sync_col);
            if sync_r.clicked() { self.events.sync_click = true; }
            // lock lamp in the button's corner: green once the beats are together, amber while they are pulled in
            if let (true, Some(locked)) = (self.sync_on, self.sync_lock) {
                let col = if locked { palette::ok() } else { palette::warn() };
                let at = sync_r.rect.right_top() + egui::vec2(-5.0, 5.0);
                ui.painter().circle_filled(at, 2.5, col);
            }
            sync_r.on_hover_text(if self.sync_on {
                "SYNC on: following the other deck's tempo and beat. Click to release.\nThe lamp is green when the beats are locked, amber while they are being pulled together."
            } else {
                "Match tempo and beat to the other deck. A stopped deck then starts in phrase."
            });
            let key_r = transport_btn(ui, "KEY", self.key_lock_on, is_loaded, egui::vec2(38.0, BTN_H_MAIN), accent);
            if key_r.clicked() { self.events.key_lock_click = true; }
            key_r.on_hover_text(if self.key_lock_on {
                "Key lock (Master Tempo) is on: changing the tempo does not change the pitch. Click to turn off and let the pitch follow the tempo."
            } else {
                "Key lock is off: the pitch follows the tempo, like a record. Click to keep the pitch fixed."
            });
        });

        if let Some((own, shift, now, note)) = self.key_info.clone() {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("KEY").small().color(palette::text_mid()));
                let b = egui::vec2(24.0, 22.0);
                if transport_btn(ui, "−", false, true, b, accent).on_hover_text("Key shift down a semitone").clicked() { self.events.key_shift = Some(-1); }
                let shown = if shift == 0 { own.clone() } else { format!("{own} → {now}") };
                let r = ui.add_sized([96.0, 22.0], egui::Label::new(egui::RichText::new(format!("{shown}  {note}")).monospace().color(if shift == 0 { palette::text() } else { accent })).sense(egui::Sense::click()));
                if r.clicked() { self.events.key_shift = Some(0); }
                r.on_hover_text(if shift == 0 { "The track's key. Shift it with − and +.".to_string() } else { format!("Shifted {shift:+} semitones. Click to go back to {own}.") });
                if transport_btn(ui, "+", false, true, b, accent).on_hover_text("Key shift up a semitone").clicked() { self.events.key_shift = Some(1); }
                let ks = transport_btn(ui, "KEY SYNC", false, self.key_sync_enabled, egui::vec2(72.0, 22.0), accent);
                if ks.clicked() { self.events.key_sync_click = true; }
                ks.on_hover_text("Shift this deck the smallest amount that makes its key mix with the other deck's (same Camelot number).");
            });
        }

        ui.add_space(4.0);
        self.draw_hot_cue_pads(ui, deck);
        ui.add_space(3.0);
        self.draw_loop_row(ui, deck);

        ui.add_space(5.0);

        // ── Row 5: Stems ──────────────────────────────────────────────────────
        self.draw_stem_panel(
            ui,
            accent,
            is_loaded,
            stems_loaded,
            stem_analyzing,
            stem_progress,
            stem_gains,
            stem_available,
            analyze_stems,
            gain_changes,
        );
    }

    fn draw_hot_cue_pads(&mut self, ui: &mut egui::Ui, deck: &DeckHandle) {
        let gap = 3.0;
        let w = ((ui.available_width() - gap * 7.0) / 8.0).floor().max(18.0);
        let enabled = deck.track.is_some();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for i in 0..8usize {
                let pad = i as u8 + 1;
                let cue = deck.cues.iter().find(|c| c.hot_cue == Some(pad));
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(w, 24.0), if enabled { Sense::click() } else { Sense::hover() });
                let down = enabled && resp.is_pointer_button_down_on();
                let col = cue.map(|c| Color32::from_rgb(c.color[0], c.color[1], c.color[2]));
                let bg = match col {
                    Some(c) => c.linear_multiply(if down { 0.95 } else if resp.hovered() { 0.75 } else { 0.6 }),
                    None => if down { palette::pressed() } else { palette::raised() },
                };
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 3.0, bg);
                p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, col.unwrap_or(palette::pressed())));
                let letter = ((b'A' + i as u8) as char).to_string();
                p.text(rect.center(), egui::Align2::CENTER_CENTER, letter,
                    egui::FontId::monospace(crate::theme::sz(12.0)),
                    if col.is_some() { palette::background() } else { palette::text_dim() });
                if cue.map(|c| c.is_loop()).unwrap_or(false) {
                    p.text(rect.right_top() + egui::vec2(-3.0, 1.0), egui::Align2::RIGHT_TOP, "↻",
                        egui::FontId::proportional(crate::theme::sz(9.0)), palette::background());
                }

                if down && !self.pad_down_prev[i] {
                    let shift = ui.input(|i| i.modifiers.shift);
                    self.events.actions.push(if shift && cue.is_some() { DeckAction::PadDelete(i as u8) }
                                             else { DeckAction::PadDown(i as u8) });
                }
                if !down && self.pad_down_prev[i] { self.events.actions.push(DeckAction::PadUp(i as u8)); }
                self.pad_down_prev[i] = down;
                if enabled && resp.secondary_clicked() && cue.is_some() {
                    self.events.actions.push(DeckAction::PadDelete(i as u8));
                }
                resp.on_hover_text(match cue {
                    Some(c) => {
                        let s = c.time_ms as f64 / 1000.0;
                        format!("Hot cue {} at {:02}:{:04.1}{} — click to jump (hold to preview when stopped). Right-click or Shift+click to delete.",
                            (b'A' + i as u8) as char, (s / 60.0) as u32, s % 60.0,
                            if c.is_loop() { " (saved loop)" } else { "" })
                    }
                    None => format!("Hot cue {} — empty. Click to set at the playhead (a running loop is saved as a hot loop).",
                        (b'A' + i as u8) as char),
                });
            }
        });
    }

    fn draw_loop_row(&mut self, ui: &mut egui::Ui, deck: &DeckHandle) {
        let loaded = deck.track.is_some();
        let region = deck.loop_region;
        let active = matches!(region, Some(r) if r.active);
        let pending_in = deck.loop_in.is_some();
        let beats = match (region, deck.frames_per_beat()) {
            (Some(r), Some(fpb)) if r.active => ((r.end - r.start) / fpb) as f32,
            _ => deck.auto_loop_beats,
        };
        let h = 24.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let r = transport_btn(ui, "IN", pending_in, loaded, egui::vec2(30.0, h), palette::loop_on());
            if r.clicked() {
                self.events.actions.push(DeckAction::LoopInDown);
                self.events.actions.push(DeckAction::LoopInUp);
            }
            r.on_hover_text("Set loop in at the playhead (snapped when Quantize is on).");

            let out_label = if active {
                "EXIT"
            } else if region.is_some() {
                "RELOOP"
            } else {
                "OUT"
            };
            let r = transport_btn(ui, out_label, active, loaded, egui::vec2(54.0, h), palette::loop_on());
            if r.clicked() {
                self.events.actions.push(DeckAction::LoopOut);
            }
            r.on_hover_text("Set loop out and start the loop. While looping: exit. After exiting: re-enter.");

            let r = transport_btn(ui, "½", false, loaded, egui::vec2(26.0, h), palette::loop_on());
            if r.clicked() {
                self.events.actions.push(DeckAction::LoopHalf);
            }
            r.on_hover_text("Halve the loop (or the next auto-loop size when no loop is running).");

            let label = format!("LOOP {}", beats_label(beats));
            let r = transport_btn(ui, &label, active, loaded, egui::vec2(66.0, h), palette::loop_on());
            if r.clicked() {
                self.events.actions.push(DeckAction::LoopAuto);
            }
            r.on_hover_text("Auto beat loop from the playhead; click again to exit.");

            let r = transport_btn(ui, "2×", false, loaded, egui::vec2(28.0, h), palette::loop_on());
            if r.clicked() {
                self.events.actions.push(DeckAction::LoopDouble);
            }
            r.on_hover_text("Double the loop (or the next auto-loop size when no loop is running).");

            let r = transport_btn(ui, "Q", deck.quantize, loaded, egui::vec2(24.0, h), palette::loop_on());
            if r.clicked() {
                self.events.actions.push(DeckAction::ToggleQuantize);
            }
            r.on_hover_text("Quantize: snap cues and loops to the beat grid.");
        });
    }

    fn draw_stem_panel(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        is_loaded: bool,
        stems_loaded: bool,
        stem_analyzing: bool,
        stem_progress: u32,
        stem_gains: &mut [f32; 4],
        stem_available: bool,
        analyze_clicked: &mut bool,
        gain_changes: &mut [Option<f32>; 4],
    ) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("STEMS").size(crate::theme::sz(10.0)).color(palette::text_dim()).monospace().strong(),
            );
            ui.add_space(4.0);

            if stems_loaded {
                for i in 0..4usize {
                    let muted = stem_gains[i] < 0.01;
                    let col = palette::stems()[i];
                    let bg = if muted { palette::divider() } else { col.linear_multiply(0.8) };
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(32.0, 20.0), Sense::click());
                    ui.painter_at(rect).rect_filled(rect, 3.0, bg);
                    if !muted {
                        ui.painter_at(rect).rect_stroke(rect.shrink(1.0), 3.0, Stroke::new(1.0_f32, col));
                    }
                    ui.painter_at(rect).text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        STEM_SHORTS[i],
                        egui::FontId::monospace(crate::theme::sz(10.5)),
                        if muted { palette::text_dim() } else { palette::text_bright() },
                    );
                    if resp.clicked() {
                        let new_g = if muted { 1.0_f32 } else { 0.0_f32 };
                        stem_gains[i] = new_g;
                        gain_changes[i] = Some(new_g);
                    }
                    if i < 3 {
                        ui.add_space(3.0);
                    }
                }
                ui.add_space(6.0);
                ui.add_space(6.0);
                let on = self.lanes_on;
                let label = RichText::new("LANES").size(crate::theme::sz(8.0)).monospace().color(if on {
                    accent
                } else {
                    palette::text_dim()
                });
                if ui
                    .add(egui::Button::new(label).small().frame(false))
                    .on_hover_text(
                        "Show a row per stem (drums, bass, other, vocals) under the waveform and on the overview",
                    )
                    .clicked()
                {
                    self.lanes_on = !on;
                }
                let all_on = stem_gains.iter().all(|&g| g > 0.5);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("ALL")
                                .size(crate::theme::sz(8.0))
                                .color(if all_on { palette::text_dim() } else { accent })
                                .monospace(),
                        )
                        .small()
                        .frame(false),
                    )
                    .clicked()
                {
                    for i in 0..4 {
                        stem_gains[i] = 1.0;
                        gain_changes[i] = Some(1.0);
                    }
                }
            } else if stem_analyzing {
                ui.spinner();
                let pct =
                    if stem_progress > 0 && stem_progress < 100 { format!("{}%", stem_progress) } else { "…".into() };
                ui.label(RichText::new(pct).size(crate::theme::sz(9.0)).color(palette::text_dim()).monospace());
            } else if stem_available && is_loaded {
                if ui
                    .add(egui::Button::new(RichText::new("Analyze").size(crate::theme::sz(9.0)).color(accent)).small())
                    .clicked()
                {
                    *analyze_clicked = true;
                }
            } else if !stem_available {
                ui.label(
                    RichText::new("no stem engine").size(crate::theme::sz(8.0)).color(palette::text_dim()).monospace(),
                );
            }
        });
    }
}

fn beats_label(b: f32) -> String {
    if b < 0.99 {
        format!("1/{}", (1.0 / b).round() as u32)
    } else if (b - b.round()).abs() < 0.02 {
        format!("{}", b.round() as u32)
    } else {
        format!("{:.1}", b)
    }
}

// ── Platter ────────────────────────────────────────────────────────────────────

impl DeckView {
    fn draw_platter_widget(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        progress: f64,
        is_loaded: bool,
        secs_left: f64,
        elapsed: String,
        is_playing: bool,
        accent: Color32,
        bpm_text: &str,
        tempo_text: &str,
        marks: &PlatterMarks,
    ) {
        let resp = ui.interact(rect, egui::Id::new(("platter", self.deck_idx)), Sense::click_and_drag());
        let center = rect.center();
        let radius = rect.width() / 2.0 - 5.0;
        let top_radius = radius * 0.65;

        // A click on the ring (a press and release without turning) goes to that point of the track, as a needle
        // drop does: twelve o'clock is the start, clockwise to the end.
        if resp.clicked() && is_loaded {
            if let Some(pos) = resp.interact_pointer_pos() {
                let v = pos - center;
                if v.length() >= radius - RING_REACH && v.length() <= radius + 5.0 {
                    let turn = v.x.atan2(-v.y).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
                    self.pending_commands.push(DeckCommand::Seek(turn as f64));
                }
            }
        }

        // Touch = pointer held down on the platter (a plain press without movement holds
        // the audio, like a finger on a vinyl). Top = hold/scratch, edge = pitch nudge.
        let down = resp.is_pointer_button_down_on();
        let pointer = ui.input(|i| i.pointer.latest_pos());

        if down && self.jog_zone == JogZone::None {
            if let Some(pos) = pointer {
                self.jog_zone = if (pos - center).length() < top_radius { JogZone::Top } else { JogZone::Side };
                self.jog_last_angle = None;
                self.jog_omega = 0.0;
                self.jog_sent = 0.0;
                if self.jog_zone == JogZone::Top {
                    self.pending_commands.push(DeckCommand::JogScratch(0.0));
                    self.pending_commands.push(DeckCommand::JogTouch(true));
                }
            }
        }

        if down && self.jog_zone != JogZone::None {
            if let Some(pos) = pointer {
                let v = pos - center;
                if v.length() > radius * 0.08 {
                    let a = v.y.atan2(v.x);
                    if let Some(last) = self.jog_last_angle {
                        let mut d = a - last;
                        let pi = std::f32::consts::PI;
                        if d > pi {
                            d -= 2.0 * pi
                        } else if d < -pi {
                            d += 2.0 * pi
                        }
                        let dt = ui.input(|i| i.stable_dt).clamp(1.0 / 240.0, 1.0 / 20.0);
                        self.jog_omega += (d / dt - self.jog_omega) * 0.5;
                    }
                    self.jog_last_angle = Some(a);
                }
            }
            // Clockwise rotation = forward; 1.0 = one platter turn per 1.8 s (33⅓ rpm).
            let vel = self.jog_omega / PLATTER_RAD_PER_SEC as f32;
            let (cmd, value) = match self.jog_zone {
                JogZone::Top => (true, vel),
                _ => (false, if vel.abs() < 0.02 { 0.0 } else { (vel * 0.35).clamp(-0.5, 0.5) }),
            };
            if (value - self.jog_sent).abs() > 0.004 || (value == 0.0 && self.jog_sent != 0.0) {
                self.jog_sent = value;
                self.pending_commands.push(if cmd {
                    DeckCommand::JogScratch(value)
                } else {
                    DeckCommand::JogNudge(value)
                });
            }
        }

        if !down && self.jog_zone != JogZone::None {
            match self.jog_zone {
                JogZone::Top => {
                    self.pending_commands.push(DeckCommand::JogScratch(0.0));
                    self.pending_commands.push(DeckCommand::JogTouch(false));
                }
                _ => self.pending_commands.push(DeckCommand::JogNudge(0.0)),
            }
            self.jog_zone = JogZone::None;
            self.jog_last_angle = None;
            self.jog_omega = 0.0;
            self.jog_sent = 0.0;
        }

        draw_platter(
            &ui.painter_at(rect),
            center,
            radius,
            progress,
            secs_left,
            self.platter_phase,
            is_playing,
            accent,
            &PlatterText { loaded: is_loaded, bpm: bpm_text, tempo: tempo_text, elapsed: &elapsed },
            marks,
        );
    }
}

/// How far in from the platter's edge a click still counts as on the progress ring.
const RING_REACH: f32 = 16.0;

/// What is marked on the platter: the hot cues and the main cue on the progress ring, and the beat.
#[derive(Default)]
struct PlatterMarks {
    /// Hot cues as (share of the track, colour).
    cues: Vec<(f32, Color32)>,
    /// The main cue as a share of the track.
    main_cue: Option<f32>,
    /// While playing on a grid: how far through the current beat (0..1), and whether it is the first of its bar.
    beat: Option<(f32, bool)>,
}

/// What the platter shows besides the spinning marker.
struct PlatterText<'a> {
    loaded: bool,
    /// Whole-number BPM the deck plays at, or "---".
    bpm: &'a str,
    /// Tempo change, "+0.0%".
    tempo: &'a str,
    /// Elapsed time, "01:23.4".
    elapsed: &'a str,
}

/// An arc along a circle from `from` to `to` (radians clockwise from twelve o'clock).
fn arc(painter: &egui::Painter, c: egui::Pos2, r: f32, from: f32, to: f32, stroke: Stroke) {
    if to - from < 0.002 {
        return;
    }
    let n = (((to - from) / 0.05).ceil() as usize).max(2);
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let a = from + (to - from) * i as f32 / n as f32;
            egui::pos2(c.x + a.sin() * r, c.y - a.cos() * r)
        })
        .collect();
    painter.add(egui::Shape::line(pts, stroke));
}

/// Like a CDJ's jog display: a ring round the edge that fills as the track plays (the last 30 s turn amber) with the
/// cues ticked on it, one marker turning at 33⅓ rpm (it follows the hand when scratching, and pulses on the beat), and
/// in the middle the BPM, tempo change and time.
fn draw_platter(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    progress: f64,
    secs_left: f64,
    phase: f32,
    is_playing: bool,
    accent: Color32,
    text: &PlatterText,
    marks: &PlatterMarks,
) {
    let tau = std::f32::consts::TAU;
    painter.circle_filled(
        egui::pos2(center.x + 2.5, center.y + 2.5),
        radius,
        Color32::from_rgba_unmultiplied(0, 0, 0, 70),
    );
    painter.circle_filled(center, radius, palette::inset());

    // Track progress ring: the part played in the deck colour, the rest dim, a bright head where it is now.
    let ring_r = radius - 6.0;
    let w = 6.0;
    let track = palette::track();
    painter.circle_stroke(center, ring_r, Stroke::new(w, track));
    if text.loaded {
        let warn = secs_left < 30.0 && secs_left > 0.0;
        let col = if warn { palette::ending() } else { accent };
        let col = if is_playing { col } else { col.linear_multiply(0.65) };
        let end = (progress.clamp(0.0, 1.0) as f32) * tau;
        arc(painter, center, ring_r, 0.0, end, Stroke::new(w, col));
        painter.circle_filled(
            egui::pos2(center.x + end.sin() * ring_r, center.y - end.cos() * ring_r),
            w * 0.7,
            palette::text_bright(),
        );
    }

    // Cues on the ring: a tick across it for each hot cue in its colour, a white one inside it for the main cue.
    if text.loaded {
        let spoke = |turn: f32, from: f32, to: f32, stroke: Stroke| {
            let a = turn * tau;
            let p = |r: f32| egui::pos2(center.x + a.sin() * r, center.y - a.cos() * r);
            painter.line_segment([p(from), p(to)], stroke);
        };
        for &(turn, col) in &marks.cues {
            spoke(turn, ring_r - w * 0.5 - 4.0, ring_r + w * 0.5 + 1.0, Stroke::new(2.5_f32, palette::inset()));
            spoke(turn, ring_r - w * 0.5 - 4.0, ring_r + w * 0.5 + 1.0, Stroke::new(1.5_f32, col));
        }
        if let Some(turn) = marks.main_cue {
            spoke(turn, ring_r - w * 0.5 - 7.0, ring_r - w * 0.5 - 1.0, Stroke::new(1.5_f32, palette::text_bright()));
        }
    }

    // The marker that turns with the music: a single dot just inside the ring. It swells on each beat and fades
    // over the first third of it, more on the first beat of a bar, so the beat can be seen without the waveform.
    if text.loaded {
        let r = ring_r - w - 5.0;
        let (pulse, bar) = marks.beat.map_or((0.0, false), |(ph, bar)| ((1.0 - ph / 0.33).max(0.0), bar));
        let at = egui::pos2(center.x + phase.sin() * r, center.y - phase.cos() * r);
        if pulse > 0.0 {
            let glow = if bar { palette::text_bright() } else { palette::beat_marker() };
            let alpha = pulse * if bar { 0.55 } else { 0.35 };
            painter.circle_filled(at, 4.0 + pulse * if bar { 5.0 } else { 3.0 }, glow.gamma_multiply(alpha));
        }
        painter.circle_filled(at, 4.0, palette::beat_marker());
    }

    // Middle: BPM large, tempo change, time.
    let dim = if text.loaded { 1.0 } else { 0.35 };
    let white = |a: f32| Color32::from_rgba_unmultiplied(255, 255, 255, (a * dim * 255.0) as u8);
    let (bpm, tempo, elapsed) = if text.loaded { (text.bpm, text.tempo, text.elapsed) } else { ("00", "", "") };
    // The three lines are stacked from their sizes, which the theme may make larger, and the stack is kept inside the
    // ring; at the designed sizes their middles are 0.14 r above, 0.14 r below and 0.34 r below the centre.
    let big = crate::theme::bpm_size((radius * 0.40).clamp(18.0, 34.0)).min(radius * 0.8);
    let small = crate::theme::sz((radius * 0.13).clamp(9.0, 12.0)).min(radius * 0.3);
    let time = crate::theme::time_size((radius * 0.12).clamp(8.5, 11.0)).min(radius * 0.3);
    let (gap1, gap2) = (radius * 0.015, radius * 0.075);
    let top = center.y + radius * 0.03 - (big + gap1 + small + gap2 + time) / 2.0;
    let y_bpm = top + big / 2.0;
    let y_tempo = top + big + gap1 + small / 2.0;
    let y_time = top + big + gap1 + small + gap2 + time / 2.0;
    painter.text(
        egui::pos2(center.x, y_bpm),
        egui::Align2::CENTER_CENTER,
        bpm,
        egui::FontId::monospace(big),
        white(1.0),
    );
    painter.text(
        egui::pos2(center.x, y_tempo),
        egui::Align2::CENTER_CENTER,
        tempo,
        egui::FontId::monospace(small),
        accent.linear_multiply(0.9),
    );
    painter.text(
        egui::pos2(center.x, y_time),
        egui::Align2::CENTER_CENTER,
        elapsed,
        egui::FontId::monospace(time),
        white(0.6),
    );
    if !text.loaded {
        painter.text(
            egui::pos2(center.x, y_time),
            egui::Align2::CENTER_CENTER,
            "no track",
            egui::FontId::monospace(crate::theme::sz(9.0)),
            white(0.5),
        );
    }

    // Fixed tick at twelve o'clock, outside the ring.
    painter.line_segment(
        [egui::pos2(center.x, center.y - radius + 1.0), egui::pos2(center.x, center.y - radius + 5.0)],
        Stroke::new(2.0_f32, palette::text_bright()),
    );
}

#[cfg(test)]
mod platter_tests {
    use super::*;

    #[test]
    fn the_deck_row_keeps_its_designed_height_in_the_default_theme() {
        assert_eq!(extra_height(), 0.0);
    }

    #[test]
    fn the_platter_draws_loaded_stopped_ending_and_empty() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let p = ui.painter().clone();
                let c = egui::pos2(100.0, 100.0);
                for (loaded, prog, left, playing) in [
                    (true, 0.4, 120.0, true),
                    (true, 0.4, 120.0, false),
                    (true, 0.97, 8.0, true),
                    (true, 0.0, 0.0, false),
                    (false, 0.0, 0.0, false),
                ] {
                    draw_platter(
                        &p,
                        c,
                        80.0,
                        prog,
                        left,
                        1.0,
                        playing,
                        Color32::LIGHT_BLUE,
                        &PlatterText { loaded, bpm: "124", tempo: "+0.0%", elapsed: "01:02.3" },
                        &PlatterMarks {
                            cues: vec![(0.1, Color32::RED), (0.6, Color32::GREEN)],
                            main_cue: Some(0.05),
                            beat: playing.then_some((0.1, prog > 0.5)),
                        },
                    );
                }
            });
        });
        let a = ctx.tessellate(vec![], 1.0);
        let _ = a;
    }
}
