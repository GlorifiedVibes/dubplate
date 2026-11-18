//! One VST3 effect on the main output and one on each deck: pick a plugin, switch it on and off, set its parameters.
//! What was chosen is remembered between runs (`master_fx.toml`, `deck_a_fx.toml`, `deck_b_fx.toml`).
//!
//! Safety nets for a live set: a plugin that crashes while loading would stop Dubplate starting, so a marker file is
//! written around the load at start-up and, if it is still there next time, that plugin is not loaded again; the same
//! for a plugin that crashes while the folders are being scanned.

use std::path::{Path, PathBuf};

use dubplate_engine::vst3_host::{find_bundles, inspect, search_dirs, MasterInsert, PluginInfo};
use egui::RichText;
use serde::{Deserialize, Serialize};

use crate::app::palette;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Saved {
    bundle: Option<PathBuf>,
    #[serde(default)]
    class_index: usize,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    values: Vec<ParamValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ParamValue {
    id: u32,
    value: f64,
}

fn file(name: &str) -> PathBuf {
    dubplate_engine::paths::config_dir().join(name)
}

/// Where an effect sits: its settings file, its button in the menu bar and how it is described.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    /// File names start with this.
    stem: &'static str,
    button: &'static str,
    /// "the main output", "deck A".
    place: &'static str,
    /// What to say about where in the signal it sits.
    about: &'static str,
}

pub const MAIN: Slot = Slot {
    stem: "master_fx",
    button: "FX",
    place: "the main output",
    about: "It sits before the limiter, and is included in recordings.",
};
const DECK_ABOUT: &str = "It sits after the channel fader and before the crossfader, so an echo or reverb rings on \
                          when the fader closes. The headphone cue is taken before it.";
pub const DECK_A: Slot = Slot { stem: "deck_a_fx", button: "A", place: "deck A", about: DECK_ABOUT };
pub const DECK_B: Slot = Slot { stem: "deck_b_fx", button: "B", place: "deck B", about: DECK_ABOUT };

pub struct MasterFx {
    slot: Slot,
    pub open: bool,
    saved: Saved,
    plugins: Vec<PluginInfo>,
    scanned: bool,
    note: String,
    filter: String,
    /// Last time a parameter changed, so the file is written once the dragging stops.
    dirty_since: Option<std::time::Instant>,
}

impl MasterFx {
    /// The effect on the main output: reads what was chosen last time and loads that plugin.
    pub fn new(insert: &MasterInsert) -> Self {
        Self::new_in(MAIN, insert)
    }

    /// The same for any slot.
    pub fn new_in(slot: Slot, insert: &MasterInsert) -> Self {
        let saved: Saved = std::fs::read_to_string(file(&format!("{}.toml", slot.stem)))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        let mut fx = MasterFx {
            slot,
            open: false,
            saved,
            plugins: Vec::new(),
            scanned: false,
            note: String::new(),
            filter: String::new(),
            dirty_since: None,
        };
        let marker = file(&format!("{}.loading", slot.stem));
        if marker.exists() {
            let name = fx
                .saved
                .bundle
                .as_ref()
                .and_then(|b| b.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            fx.note = format!("{name} did not finish loading last time, so it was left out. Pick it again to retry.");
            let _ = std::fs::remove_file(&marker);
            fx.saved.bundle = None;
            fx.saved.enabled = false;
        } else if let Some(bundle) = fx.saved.bundle.clone() {
            let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
            let _ = std::fs::write(&marker, bundle.to_string_lossy().as_bytes());
            match Self::load_from(&bundle, fx.saved.class_index, insert) {
                Ok(()) => {
                    for v in &fx.saved.values {
                        insert.set_param(v.id, v.value);
                    }
                    insert.set_enabled(fx.saved.enabled);
                }
                Err(e) => {
                    fx.note = e;
                    fx.saved.bundle = None;
                    fx.saved.enabled = false;
                }
            }
            let _ = std::fs::remove_file(&marker);
        }
        fx
    }

    fn load_from(bundle: &Path, class_index: usize, insert: &MasterInsert) -> Result<(), String> {
        let infos = inspect(bundle)?;
        let info = infos
            .iter()
            .find(|i| i.class_index == class_index)
            .or(infos.iter().find(|i| i.is_effect))
            .ok_or("no effect in that plugin file")?;
        insert.load(info)
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
        if let Ok(s) = toml::to_string_pretty(&self.saved) {
            let _ = std::fs::write(file(&format!("{}.toml", self.slot.stem)), s);
        }
    }

    /// Scans the plugin folders. A plugin that crashes during the scan is remembered and skipped next time.
    fn scan(&mut self) {
        let skip_file = file("master_fx.skip");
        let crashed = file("master_fx.scanning");
        let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
        let mut skip: Vec<String> =
            std::fs::read_to_string(&skip_file).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default();
        if let Ok(b) = std::fs::read_to_string(&crashed) {
            if !skip.contains(&b) {
                skip.push(b.clone());
            }
            let _ = std::fs::write(&skip_file, skip.join("\n"));
            self.note = format!(
                "{} crashed the last scan and is now skipped.",
                Path::new(&b).file_name().and_then(|n| n.to_str()).unwrap_or("a plugin")
            );
        }
        self.plugins.clear();
        for bundle in find_bundles(&search_dirs()) {
            let key = bundle.to_string_lossy().into_owned();
            if skip.contains(&key) {
                continue;
            }
            let _ = std::fs::write(&crashed, &key);
            if let Ok(found) = inspect(&bundle) {
                self.plugins.extend(found.into_iter().filter(|p| p.is_effect));
            }
        }
        let _ = std::fs::remove_file(&crashed);
        self.scanned = true;
        if self.plugins.is_empty() && self.note.is_empty() {
            self.note = format!(
                "No VST3 effects found in {}. Set DUBPLATE_VST3_PATH to add folders.",
                search_dirs().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            );
        }
    }

    fn choose(&mut self, info: &PluginInfo, insert: &MasterInsert) {
        let marker = file(&format!("{}.loading", self.slot.stem));
        let _ = std::fs::create_dir_all(dubplate_engine::paths::config_dir());
        let _ = std::fs::write(&marker, info.bundle.to_string_lossy().as_bytes());
        let r = insert.load(info);
        let _ = std::fs::remove_file(&marker);
        match r {
            Ok(()) => {
                self.saved = Saved {
                    bundle: Some(info.bundle.clone()),
                    class_index: info.class_index,
                    enabled: false,
                    values: Vec::new(),
                };
                insert.set_enabled(false);
                self.note = String::new();
            }
            Err(e) => self.note = format!("Could not load {}: {e}", info.name),
        }
        self.save();
    }

    /// The on/off switch and the gear for the menu bar.
    pub fn menubar(&mut self, ui: &mut egui::Ui, insert: &MasterInsert) {
        let loaded = insert.info();
        let on = loaded.is_some() && insert.enabled();
        let (dot, col) = if on { ("●", palette::ok()) } else { ("○", palette::text_dim()) };
        let label = RichText::new(format!("{} {dot}", self.slot.button)).size(crate::theme::sz(13.0)).color(if on {
            palette::text_bright()
        } else {
            palette::text()
        });
        let btn =
            egui::Button::new(label).fill(if on { palette::ok().linear_multiply(0.4) } else { palette::raised() });
        let tip = match &loaded {
            Some(i) if on => format!("{} is on {}. Click to switch it off.", i.name, self.slot.place),
            Some(i) => format!("{} is loaded but off. Click to switch it on.", i.name),
            None => format!("No effect on {}. Click to choose one.", self.slot.place),
        };
        let _ = col;
        if ui.add(btn).on_hover_text(tip).clicked() {
            if loaded.is_some() {
                insert.set_enabled(!on);
                self.saved.enabled = !on;
                self.save();
            } else {
                self.open = true;
            }
        }
        if ui
            .add(egui::Button::new(RichText::new("⚙").size(crate::theme::sz(13.0)).color(palette::text())).frame(false))
            .on_hover_text(format!("Effect on {} (VST3)", self.slot.place))
            .clicked()
        {
            self.open = !self.open;
        }
    }

    /// The window: plugin choice, on/off, parameters.
    pub fn window(&mut self, ctx: &egui::Context, insert: &MasterInsert) {
        if !self.open {
            return;
        }
        if let Some(t) = self.dirty_since {
            if t.elapsed() > std::time::Duration::from_millis(400) {
                self.saved.values = insert.settings().into_iter().map(|(id, value)| ParamValue { id, value }).collect();
                self.save();
                self.dirty_since = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
        let mut open = self.open;
        let slot = self.slot;
        egui::Window::new(format!("Effect on {} (VST3)", slot.place))
            .open(&mut open)
            .default_width(460.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                let loaded = insert.info();
                ui.horizontal(|ui| {
                    let on = insert.enabled() && loaded.is_some();
                    if ui
                        .add_enabled(
                            loaded.is_some(),
                            egui::SelectableLabel::new(
                                on,
                                RichText::new(if on { "  ON  " } else { "  OFF  " })
                                    .strong()
                                    .size(crate::theme::sz(14.0)),
                            ),
                        )
                        .clicked()
                    {
                        insert.set_enabled(!on);
                        self.saved.enabled = !on;
                        self.save();
                    }
                    ui.label(
                        RichText::new(
                            loaded.as_ref().map_or("no plugin".to_string(), |i| format!("{}  ·  {}", i.name, i.vendor)),
                        )
                        .size(crate::theme::sz(13.0)),
                    );
                    if loaded.is_some() && ui.small_button("Unload").clicked() {
                        insert.unload();
                        insert.set_enabled(false);
                        self.saved = Saved::default();
                        self.save();
                    }
                });
                if insert.latency_frames() > 0 {
                    ui.label(
                        RichText::new(format!("adds {} frames of delay to {}", insert.latency_frames(), slot.place))
                            .size(crate::theme::sz(11.0))
                            .color(palette::text_dim()),
                    );
                }
                if !self.note.is_empty() {
                    ui.colored_label(palette::warn(), &self.note);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button(if self.scanned { "Scan again" } else { "Find plugins" })
                        .on_hover_text(
                            "Looks in the VST3 folders for effects. Plugins are loaded briefly to read their names.",
                        )
                        .clicked()
                    {
                        self.scan();
                    }
                    if !self.plugins.is_empty() {
                        let current = loaded
                            .as_ref()
                            .map(|i| format!("{} ({})", i.name, i.vendor))
                            .unwrap_or_else(|| "choose…".into());
                        egui::ComboBox::from_id_salt((slot.stem, "pick")).selected_text(current).width(280.0).show_ui(
                            ui,
                            |ui| {
                                let list = self.plugins.clone();
                                for p in &list {
                                    if ui.selectable_label(false, format!("{} ({})", p.name, p.vendor)).clicked() {
                                        self.choose(p, insert);
                                    }
                                }
                            },
                        );
                    }
                });
                ui.add_space(4.0);
                let params = insert.params();
                if loaded.is_some() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{} parameters", params.len()))
                                .size(crate::theme::sz(11.0))
                                .color(palette::text_dim()),
                        );
                        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter").desired_width(140.0));
                    });
                    let f = self.filter.to_lowercase();
                    let shown: Vec<_> =
                        params.iter().filter(|p| f.is_empty() || p.name.to_lowercase().contains(&f)).collect();
                    let mut changed = false;
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                        ui,
                        24.0,
                        shown.len(),
                        |ui, range| {
                            for p in &shown[range] {
                                let mut v = insert.get_param(p.id).unwrap_or(p.default);
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [150.0, 20.0],
                                        egui::Label::new(RichText::new(&p.name).size(crate::theme::sz(12.0)))
                                            .truncate(),
                                    );
                                    let mut slider = egui::Slider::new(&mut v, 0.0..=1.0).show_value(false);
                                    if p.steps > 0 {
                                        slider = slider.step_by(1.0 / p.steps as f64);
                                    }
                                    if ui.add(slider).changed() {
                                        insert.set_param(p.id, v);
                                        changed = true;
                                    }
                                    ui.label(
                                        RichText::new(format!("{} {}", insert.param_text(p.id, v), p.units))
                                            .size(crate::theme::sz(11.0))
                                            .monospace()
                                            .color(palette::text_dim()),
                                    );
                                });
                            }
                        },
                    );
                    if changed {
                        self.dirty_since = Some(std::time::Instant::now());
                    }
                } else {
                    ui.label(
                        RichText::new(format!("Choose a VST3 effect to put on {}. {}", slot.place, slot.about))
                            .size(crate::theme::sz(12.5))
                            .color(palette::text_dim()),
                    );
                }
            });
        self.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `DUBPLATE_TEST_VST3=/path/Effect.vst3`: the button and the window draw with a real plugin loaded, off and on,
    /// with and without a filter, without panicking (nothing is read from or written to the user's settings).
    #[test]
    fn the_button_and_window_draw_with_a_real_plugin() {
        let Some(path) = std::env::var_os("DUBPLATE_TEST_VST3") else { return };
        let insert = MasterInsert::default();
        let info = inspect(Path::new(&path)).unwrap().into_iter().find(|i| i.is_effect).unwrap();
        insert.load(&info).unwrap();
        let mut fx = MasterFx {
            slot: MAIN,
            open: true,
            saved: Saved::default(),
            plugins: vec![info],
            scanned: true,
            note: "a note".into(),
            filter: String::new(),
            dirty_since: None,
        };
        let ctx = egui::Context::default();
        for (i, on) in [false, true, true].into_iter().enumerate() {
            insert.set_enabled(on);
            if i == 2 {
                fx.filter = "delay".into();
            }
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
                    ..Default::default()
                },
                |ctx| {
                    egui::TopBottomPanel::top("t").show(ctx, |ui| {
                        ui.horizontal(|ui| fx.menubar(ui, &insert));
                    });
                    fx.window(ctx, &insert);
                },
            );
            assert!(!out.shapes.is_empty());
        }
        fx.plugins.clear();
        fx.open = true;
        insert.unload();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::TopBottomPanel::top("t").show(ctx, |ui| {
                ui.horizontal(|ui| fx.menubar(ui, &insert));
            });
            fx.window(ctx, &insert);
        });
    }

    /// With `DUBPLATE_TEST_VST3` (a bundle) and `DUBPLATE_VST3_PATH` (its folder) set, and `XDG_CONFIG_HOME` pointing at a
    /// scratch folder (so the user's own settings are never touched): scanning finds the effect, choosing loads it, a
    /// new start brings it back with its on/off state and values, and a leftover load marker keeps it out.
    #[test]
    fn scan_choose_remember_and_the_crash_marker() {
        let (Some(_), Some(cfg)) = (std::env::var_os("DUBPLATE_TEST_VST3"), std::env::var_os("XDG_CONFIG_HOME")) else {
            return;
        };
        assert!(file("x").starts_with(&cfg), "config folder is not the scratch one");
        let _ = std::fs::remove_dir_all(dubplate_engine::paths::config_dir());

        let insert = MasterInsert::default();
        let mut fx = MasterFx::new(&insert);
        assert!(insert.info().is_none());
        fx.scan();
        let info = fx.plugins.iter().find(|p| p.name.contains("Effects")).expect("the effect is found").clone();
        assert!(fx.plugins.iter().all(|p| p.is_effect));
        fx.choose(&info, &insert);
        assert!(insert.info().is_some() && !insert.enabled());
        let id = insert.params().iter().find(|p| p.name == "Output Mix").unwrap().id;
        insert.set_param(id, 0.25);
        insert.set_enabled(true);
        fx.saved.enabled = true;
        fx.saved.values = insert.settings().into_iter().map(|(id, value)| ParamValue { id, value }).collect();
        fx.save();

        let again = MasterInsert::default();
        let fx2 = MasterFx::new(&again);
        assert!(again.enabled(), "{}", fx2.note);
        assert_eq!(again.info().unwrap().name, info.name);
        assert!((again.get_param(id).unwrap() - 0.25).abs() < 1e-6);

        // a load that never finished: the marker is still there
        std::fs::write(file("master_fx.loading"), b"x").unwrap();
        let third = MasterInsert::default();
        let fx3 = MasterFx::new(&third);
        assert!(
            third.info().is_none() && !third.enabled() && fx3.note.contains("did not finish loading"),
            "{}",
            fx3.note
        );
        assert!(!file("master_fx.loading").exists());
        let _ = std::fs::remove_dir_all(dubplate_engine::paths::config_dir());
    }

    #[test]
    fn what_was_chosen_round_trips_through_toml() {
        let s = Saved {
            bundle: Some("/home/dj/.vst3/Kotelnikov.vst3".into()),
            class_index: 1,
            enabled: true,
            values: vec![ParamValue { id: 7, value: 0.25 }, ParamValue { id: 4_000_000_000, value: 1.0 }],
        };
        let t = toml::to_string_pretty(&s).unwrap();
        assert_eq!(toml::from_str::<Saved>(&t).unwrap(), s);
        assert_eq!(toml::from_str::<Saved>("").unwrap(), Saved::default());
    }
}
