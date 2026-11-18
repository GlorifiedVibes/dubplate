//! Preferences window: MIDI controllers, profiles, importing mappings, live monitor.

use std::sync::mpsc;

use egui::{vec2, RichText};

use super::{palette, DubplateApp};
use crate::importers::{self, Report};
use crate::midi_learn::{self, Editor};
use crate::midi_profile::{self, Profile, OFF};

type ImportResult = Result<(Profile, Report), String>;

#[derive(Default)]
pub struct PrefsState {
    pub open: bool,
    import_rx: Option<mpsc::Receiver<ImportResult>>,
    /// Outcome of the last import: (headline, ok?, detail lines).
    import_msg: Option<(String, bool, Vec<String>)>,
    /// Profile whose bindings are expanded.
    show_binds: Option<String>,
    /// MIDI learn editor, when open.
    pub editor: Option<Editor>,
    /// Set when the output / monitor choice changed; the app restarts audio.
    pub audio_apply: bool,
    /// The interface size while its slider is being dragged: the zoom changes when it is let go, so the slider does
    /// not move under the pointer.
    scale_drag: Option<f32>,
    /// What happened to the last attempt to write the theme file.
    theme_msg: Option<(String, bool)>,
}

impl DubplateApp {
    pub(super) fn show_prefs(&mut self, ctx: &egui::Context) {
        self.poll_import();
        if !self.prefs.open {
            return;
        }

        let mut open = true;
        let mut choice: Option<(String, Option<String>)> = None;
        let mut rescan = false;
        let mut import = false;
        let mut delete: Option<String> = None;
        let mut learn_port: Option<String> = None;
        let mut audio = self.audio_settings.clone();
        let mut refresh_devices = false;

        egui::Window::new("Preferences")
            .open(&mut open)
            .default_size([680.0, 560.0])
            .min_width(520.0)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    heading(ui, "APPEARANCE");
                    self.appearance_section(ui);
                    ui.add_space(10.0);
                    heading(ui, "AUDIO");
                    self.audio_section(ui, &mut audio, &mut refresh_devices);
                    ui.add_space(10.0);
                    heading(ui, "MIDI CONTROLLERS");
                    if let Some(e) = &self.midi.error {
                        ui.colored_label(palette::bad(), format!("MIDI unavailable: {e}"));
                    } else if self.midi.ports.is_empty() {
                        ui.colored_label(
                            palette::text_dim(),
                            "No MIDI controllers found. Plug one in, then press Rescan.",
                        );
                    }
                    for port in &self.midi.ports {
                        ui.horizontal(|ui| {
                            let (dot, tip) =
                                match (&port.profile_id, self.midi.settings.ports.get(&port.key).map(String::as_str)) {
                                    (_, Some(OFF)) => (palette::text_dim(), "Ignored"),
                                    (Some(_), _) => (palette::ok(), "Mapped"),
                                    (None, _) => (palette::warn(), "No profile — pick one or import a mapping"),
                                };
                            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.5, dot);
                            ui.label(RichText::new(&port.key).size(crate::theme::sz(14.0)).color(palette::text()))
                                .on_hover_text(tip);

                            let current = match self.midi.settings.ports.get(&port.key).map(String::as_str) {
                                Some(OFF) => "Off".to_owned(),
                                _ if port.auto => format!("Auto: {}", port.profile_name.clone().unwrap_or_default()),
                                _ => port.profile_name.clone().unwrap_or_else(|| "Auto: none found".into()),
                            };
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .button("Map…")
                                    .on_hover_text(
                                        "MIDI learn: map this controller's buttons, faders and knobs yourself",
                                    )
                                    .clicked()
                                {
                                    learn_port = Some(port.key.clone());
                                }
                                egui::ComboBox::from_id_salt(("port_profile", &port.key))
                                    .width(280.0)
                                    .selected_text(current)
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(port.auto, "Auto-detect").clicked() {
                                            choice = Some((port.key.clone(), None));
                                        }
                                        for p in &self.midi.profiles {
                                            let sel = !port.auto && port.profile_id.as_deref() == Some(&p.id);
                                            if ui.selectable_label(sel, &p.profile.device.name).clicked() {
                                                choice = Some((port.key.clone(), Some(p.id.clone())));
                                            }
                                        }
                                        if ui.selectable_label(false, "Off (ignore this device)").clicked() {
                                            choice = Some((port.key.clone(), Some(OFF.into())));
                                        }
                                    });
                            });
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Rescan").clicked() {
                            rescan = true;
                        }
                        if ui
                            .button("Import mapping…")
                            .on_hover_text(
                                "Convert a DJUCED, Mixxx or rekordbox controller map into a Dubplate profile",
                            )
                            .clicked()
                        {
                            import = true;
                        }
                        ui.colored_label(palette::text_dim(), "Your choice is remembered per device.");
                    });
                    if let Some((head, ok, lines)) = &self.prefs.import_msg {
                        ui.add_space(2.0);
                        ui.colored_label(if *ok { palette::ok() } else { palette::bad() }, head);
                        if !lines.is_empty() {
                            egui::CollapsingHeader::new(
                                RichText::new(format!("{} notes", lines.len())).color(palette::text_dim()),
                            )
                            .id_salt("import_notes")
                            .show(ui, |ui| {
                                for l in lines {
                                    ui.label(RichText::new(l).size(crate::theme::sz(12.0)).color(palette::text_dim()));
                                }
                            });
                        }
                    }

                    if self.prefs.editor.is_some() {
                        ui.add_space(10.0);
                        heading(ui, "MIDI LEARN");
                        self.learn_section(ui);
                    }

                    ui.add_space(10.0);
                    heading(ui, "MIDI MONITOR");
                    ui.label(
                        RichText::new("Move a control to see what Dubplate receives. Orange = no function bound.")
                            .size(crate::theme::sz(12.0))
                            .color(palette::text_dim()),
                    );
                    ui.horizontal(|ui| {
                        let paused = self.midi.monitor_paused.load(std::sync::atomic::Ordering::Relaxed);
                        if ui
                            .button(if paused { "Resume" } else { "Pause" })
                            .on_hover_text(
                                "Stop (or restart) recording messages here. Your controller keeps working either way.",
                            )
                            .clicked()
                        {
                            self.midi.monitor_paused.store(!paused, std::sync::atomic::Ordering::Relaxed);
                        }
                        if ui.button("Clear").clicked() {
                            if let Ok(mut l) = self.midi.monitor.lock() {
                                l.clear();
                            }
                        }
                        if paused {
                            ui.colored_label(palette::warn(), "Paused - new messages are not shown");
                        }
                    });
                    let lines: Vec<_> =
                        self.midi.monitor.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default();
                    if lines.is_empty() {
                        ui.label(
                            RichText::new("(nothing received yet)")
                                .size(crate::theme::sz(12.5))
                                .color(palette::text_dim()),
                        );
                    }
                    for l in &lines {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&l.text).monospace().size(crate::theme::sz(13.0)).color(palette::text()),
                            );
                            match &l.function {
                                Some(f) => ui.label(RichText::new(f).size(crate::theme::sz(13.0)).color(palette::ok())),
                                None => ui.label(
                                    RichText::new("unmapped").size(crate::theme::sz(13.0)).color(palette::warn()),
                                ),
                            };
                        });
                    }
                    ui.add_space(10.0);
                    heading(ui, "PROFILES");
                    for p in &self.midi.profiles {
                        let dev = &p.profile.device;
                        let open_now = self.prefs.show_binds.as_deref() == Some(&p.id);
                        ui.horizontal(|ui| {
                            let arrow = if open_now { "v" } else { ">" };
                            if ui.add(egui::Button::new(RichText::new(arrow).monospace()).frame(false)).clicked() {
                                self.prefs.show_binds = if open_now { None } else { Some(p.id.clone()) };
                            }
                            ui.label(RichText::new(&dev.name).size(crate::theme::sz(14.0)).color(palette::text()));
                            ui.label(
                                RichText::new(format!("{} controls", p.profile.binds.len()))
                                    .size(crate::theme::sz(12.0))
                                    .color(palette::text_dim()),
                            );
                            if !p.builtin {
                                ui.label(RichText::new("custom").size(crate::theme::sz(12.0)).color(palette::deck_a()));
                                if ui.small_button("Delete").clicked() {
                                    delete = Some(p.id.clone());
                                }
                            }
                        });
                        if open_now {
                            if !dev.source.is_empty() || !dev.notes.is_empty() {
                                ui.label(
                                    RichText::new(format!("{}  {}", dev.source, dev.notes))
                                        .size(crate::theme::sz(12.0))
                                        .color(palette::text_dim()),
                                );
                            }
                            ui.label(
                                RichText::new(format!("Matches ports containing: {}", dev.port_match.join(", ")))
                                    .size(crate::theme::sz(12.0))
                                    .color(palette::text_dim()),
                            );
                            egui::Grid::new(("binds", &p.id)).num_columns(3).spacing([18.0, 2.0]).striped(true).show(
                                ui,
                                |ui| {
                                    for b in &p.profile.binds {
                                        ui.label(
                                            RichText::new(&b.msg)
                                                .monospace()
                                                .size(crate::theme::sz(12.0))
                                                .color(palette::text_dim()),
                                        );
                                        let deck = match b.deck {
                                            Some(0) => " A",
                                            Some(1) => " B",
                                            _ => "",
                                        };
                                        let arg = b.arg.map(|a| format!(" {a}")).unwrap_or_default();
                                        ui.label(
                                            RichText::new(format!("{}{deck}{arg}", b.func))
                                                .size(crate::theme::sz(12.5))
                                                .color(palette::text()),
                                        );
                                        ui.label(
                                            RichText::new(&b.label)
                                                .size(crate::theme::sz(12.0))
                                                .color(palette::text_dim()),
                                        );
                                        ui.end_row();
                                    }
                                },
                            );
                            ui.add_space(4.0);
                        }
                    }
                });
            });
        if open {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        self.prefs.open = open;
        if refresh_devices {
            self.audio_devices = dubplate_engine::list_output_devices_with_channels();
        }
        if audio != self.audio_settings {
            self.audio_settings = audio;
            self.prefs.audio_apply = true;
        }
        if !open && self.prefs.editor.take().is_some() {
            midi_learn::cancel(&self.midi.learn);
        }
        if let Some(port) = learn_port {
            self.start_learning(&port);
        }

        if let Some((key, c)) = choice {
            self.midi.choose(&key, c.as_deref());
        }
        if rescan {
            self.midi.reload_profiles();
        }
        if import {
            self.start_import();
        }
        if let Some(id) = delete {
            let _ = std::fs::remove_file(midi_profile::user_profile_dir().join(format!("{id}.toml")));
            self.midi.reload_profiles();
        }
    }

    fn start_import(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.prefs.import_rx = Some(rx);
        std::thread::spawn(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Controller map (DJUCED .djm, Mixxx .xml, rekordbox .csv)", &["djm", "xml", "csv"])
                .set_title("Import a controller mapping")
                .pick_file()
            else {
                let _ = tx.send(Err(String::new()));
                return;
            };
            let result = importers::read_text(&path).map_err(|e| format!("{}: {e}", path.display())).and_then(|text| {
                let source = importers::detect(&path, &text)
                    .ok_or("not a DJUCED .djm, Mixxx .xml or rekordbox .csv controller map")?;
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("controller");
                importers::import(source, &text, stem)
            });
            let _ = tx.send(result);
        });
    }

    fn poll_import(&mut self) {
        let Some(rx) = &self.prefs.import_rx else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.prefs.import_rx = None;
        match result {
            Err(e) if e.is_empty() => {} // dialog cancelled
            Err(e) => self.prefs.import_msg = Some((e, false, Vec::new())),
            Ok((mut profile, report)) => {
                if profile.device.port_match.is_empty() {
                    profile.device.port_match = vec![profile.device.name.clone()];
                }
                match midi_profile::save_user_profile(&profile) {
                    Ok(_) => {
                        let mut lines: Vec<String> =
                            report.approximations.iter().map(|a| format!("approximate: {a}")).collect();
                        lines.extend(report.skipped.iter().map(|(n, w)| format!("skipped {n}: {w}")));
                        self.prefs.import_msg = Some((
                            format!(
                                "Imported {} — {} controls converted, {} left out.",
                                profile.device.name,
                                report.converted,
                                report.skipped.len()
                            ),
                            true,
                            lines,
                        ));
                        self.midi.reload_profiles();
                    }
                    Err(e) => {
                        self.prefs.import_msg = Some((format!("Saving the profile failed: {e}"), false, Vec::new()))
                    }
                }
            }
        }
    }
}

impl DubplateApp {
    /// Output and Monitor (headphone) device pickers.
    fn audio_section(
        &mut self,
        ui: &mut egui::Ui,
        audio: &mut crate::audio_settings::AudioSettings,
        refresh: &mut bool,
    ) {
        use crate::audio_settings::MonitorChoice;
        let short = |s: &str| {
            if s.chars().count() > 46 {
                format!("…{}", s.chars().skip(s.chars().count() - 43).collect::<String>())
            } else {
                s.to_owned()
            }
        };
        let devices = self.audio_devices.clone();

        egui::Grid::new("audio_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Output");
            let out_text = match &audio.output {
                Some(d) => short(d),
                None => format!("System default ({})", short(&self.selected_device)),
            };
            egui::ComboBox::from_id_salt("audio_output").width(380.0).selected_text(out_text).show_ui(ui, |ui| {
                if ui.selectable_label(audio.output.is_none(), "System default").clicked() {
                    audio.output = None;
                }
                for (d, _) in &devices {
                    if ui.selectable_label(audio.output.as_deref() == Some(d), short(d)).clicked() {
                        audio.output = Some(d.clone());
                    }
                }
            });
            ui.end_row();

            ui.label("Monitor");
            let mon_text = match &audio.monitor {
                MonitorChoice::Off => "Off".to_owned(),
                MonitorChoice::SameDevice => "Same device, channels 3-4".to_owned(),
                MonitorChoice::Device(d) => short(d),
                MonitorChoice::DeviceChannels34(d) => format!("{} - channels 3-4", short(d)),
            };
            egui::ComboBox::from_id_salt("audio_monitor").width(380.0).selected_text(mon_text).show_ui(ui, |ui| {
                if ui.selectable_label(audio.monitor == MonitorChoice::Off, "Off").clicked() {
                    audio.monitor = MonitorChoice::Off;
                }
                if ui
                    .selectable_label(audio.monitor == MonitorChoice::SameDevice, "Same device, channels 3-4")
                    .on_hover_text("For a 4-channel card such as the Hercules Inpulse: main on 1-2, headphones on 3-4")
                    .clicked()
                {
                    audio.monitor = MonitorChoice::SameDevice;
                }
                ui.separator();
                for (d, max) in &devices {
                    if ui.selectable_label(audio.monitor == MonitorChoice::Device(d.clone()), short(d)).clicked() {
                        audio.monitor = MonitorChoice::Device(d.clone());
                    }
                    if *max >= 4 {
                        let pair = MonitorChoice::DeviceChannels34(d.clone());
                        if ui
                            .selectable_label(audio.monitor == pair, format!("{} - channels 3-4", short(d)))
                            .on_hover_text(
                                "Headphones on a controller's sound card while the main mix plays on another output",
                            )
                            .clicked()
                        {
                            audio.monitor = pair;
                        }
                    }
                }
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Refresh devices").clicked() {
                *refresh = true;
            }
            let (colour, text) = match &self.audio_message {
                Some(m) => (palette::bad(), m.clone()),
                None if self._audio.monitor_active => (palette::ok(), self._audio.monitor_status.clone()),
                None => (palette::text_dim(), self._audio.monitor_status.clone()),
            };
            ui.colored_label(colour, text);
        });
        ui.label(RichText::new("Changing a device restarts the audio, so loaded tracks stop and return to the start. \
            A card can only be opened once: use \"Same device\" to put the main mix and headphones on one 4-channel card (set Output to that card too). On a Hercules Inpulse the headphone jack is channels 3-4, and its MASTER button adds the main output to it.")
            .size(crate::theme::sz(12.0)).color(palette::text_dim()));
    }
}

impl DubplateApp {
    /// Interface and text sizes, a few looks to start from, and where the theme file is.
    fn appearance_section(&mut self, ui: &mut egui::Ui) {
        use crate::theme::{self, Theme};
        let st = &mut self.prefs;
        let mut t = (*theme::get()).clone();
        let before = t.clone();
        let mut save = false;

        // egui draws a slider's rail in the resting widget colour, which is the window's own here
        ui.visuals_mut().widgets.inactive.bg_fill = palette::track();
        ui.spacing_mut().slider_width = 220.0;
        egui::Grid::new("appearance").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Start from");
            ui.horizontal(|ui| {
                for name in theme::PRESETS {
                    if ui
                        .button(name)
                        .on_hover_text("Sizes and colours from this look (the interface size stays)")
                        .clicked()
                    {
                        t = Theme { ui_scale: t.ui_scale, ..Theme::preset(name) };
                        save = true;
                    }
                }
            });
            ui.end_row();

            ui.label("Interface size");
            let mut scale = st.scale_drag.unwrap_or(t.ui_scale);
            let r = ui.add(
                egui::Slider::new(&mut scale, 0.75..=2.0)
                    .step_by(0.05)
                    .trailing_fill(true)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
            let r = r.on_hover_text("Everything, as Ctrl + and Ctrl - do");
            if r.dragged() {
                st.scale_drag = Some(scale);
            } else if r.changed() || r.drag_stopped() {
                st.scale_drag = None;
                t.ui_scale = scale;
                save = true;
            }
            ui.end_row();

            let mut size = |ui: &mut egui::Ui, label: &str, v: &mut f32, max: f32, tip: &str| {
                ui.label(label);
                let r = ui.add(
                    egui::Slider::new(v, 0.75..=max)
                        .step_by(0.05)
                        .trailing_fill(true)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                r.clone().on_hover_text(tip);
                // the file is written once the slider is let go
                save |= r.drag_stopped() || (r.changed() && !r.dragged());
                ui.end_row();
            };
            size(ui, "Text size", &mut t.text_scale, 1.6, "All text, on top of the interface size");
            size(
                ui,
                "BPM numbers",
                &mut t.readouts.bpm,
                2.5,
                "The BPM on each deck and its platter, on top of the text size",
            );
            size(
                ui,
                "Time numbers",
                &mut t.readouts.time,
                2.0,
                "The time left and the time on the platter, on top of the text size",
            );
        });

        if t != before {
            theme::set(t);
        }
        if save {
            st.theme_msg = theme::save().err().map(|e| (format!("Could not save the theme: {e}"), false));
        }

        let path = theme::path();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Theme file").color(palette::text_dim()));
            ui.label(
                RichText::new(path.display().to_string())
                    .monospace()
                    .size(crate::theme::sz(11.0))
                    .color(palette::text()),
            );
        });
        ui.horizontal(|ui| {
            if ui
                .button("Write theme file")
                .on_hover_text("Saves every colour and size, so they can be changed in a text editor. The app picks up changes as the file is saved.")
                .clicked()
            {
                st.theme_msg = Some(match theme::save() {
                    Ok(()) => ("Written. Edit it while the app is open to see the changes.".into(), true),
                    Err(e) => (format!("Could not write it: {e}"), false),
                });
            }
            if ui.button("Show folder").clicked() {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                    open_folder(dir);
                }
            }
            if ui.button("Reset").on_hover_text("Back to the default look and sizes").clicked() {
                theme::set(Theme::default());
                st.scale_drag = None;
                st.theme_msg = theme::save().err().map(|e| (format!("Could not save the theme: {e}"), false));
            }
        });
        if let Some(e) = theme::error() {
            ui.colored_label(
                palette::bad(),
                format!("The theme file has a mistake, so the last good look is kept: {e}"),
            );
        }
        if let Some((msg, ok)) = &st.theme_msg {
            ui.colored_label(if *ok { palette::ok() } else { palette::bad() }, msg);
        }
    }
}

/// Opens a folder in the system's file manager.
fn open_folder(dir: &std::path::Path) {
    let cmd = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(cmd).arg(dir).spawn() {
        log::warn!("opening {}: {e}", dir.display());
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(crate::theme::sz(12.0)).color(palette::text_dim()).strong());
    ui.separator();
}
