//! MIDI learn editor, shown in Preferences: pick a function, move a control, done.

use std::time::Instant;

use egui::RichText;

use super::{palette, DubplateApp};
use crate::midi_learn::{self, Editor, Kind, Slot};
use crate::midi_profile;

/// What the user did in one frame; applied after the rows are drawn.
enum Act {
    Learn(Slot, bool),
    Cancel,
    Remove(usize),
    Clear(Slot),
}

impl DubplateApp {
    /// Opens the editor for a port, starting from the profile it uses now.
    pub(super) fn start_learning(&mut self, port_key: &str) {
        let base = self
            .midi
            .ports
            .iter()
            .find(|p| p.key == port_key)
            .and_then(|p| p.profile_id.as_deref())
            .and_then(|id| self.midi.profiles.iter().find(|p| p.id == id));
        self.prefs.editor = Some(Editor::start(port_key, base));
    }

    pub(super) fn learn_section(&mut self, ui: &mut egui::Ui) {
        let Some(ed) = self.prefs.editor.as_mut() else { return };
        let learn = self.midi.learn.clone();

        // A control has been moved: bind it to the slot that asked for it.
        if let Some((slot, replace)) = ed.learning {
            if let Some(cap) = midi_learn::take_settled(&learn, Instant::now()) {
                match cap.suits(Kind::of(slot.func)) {
                    Ok(()) => {
                        let displaced = midi_learn::apply(&mut ed.profile, slot, &cap, replace);
                        let mut text = format!("{} ← {}", slot.label(), cap.msg());
                        if cap.hires {
                            text.push_str(" (14-bit)");
                        }
                        if let Some(d) = displaced {
                            text.push_str(&format!("; it no longer does: {d}"));
                        }
                        ed.note = Some((text, true));
                        ed.dirty = true;
                        ed.learning = None;
                    }
                    Err(e) => {
                        ed.note = Some((e, false));
                        midi_learn::begin(&learn); // keep listening
                    }
                }
            }
        }

        let mut act: Option<Act> = None;
        let mut save = false;
        let mut close = false;

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&ed.profile.device.name).size(crate::theme::sz(14.0)).color(palette::text()).strong(),
            );
            ui.label(RichText::new(format!("for {}", ed.port)).size(crate::theme::sz(12.0)).color(palette::text_dim()));
        });
        ui.label(
            RichText::new(
                "Press Learn, then move the control you want. Use Add for a second control (a shift layer). \
             Move faders and knobs through a good part of their travel.",
            )
            .size(crate::theme::sz(12.0))
            .color(palette::text_dim()),
        );
        if let Some((text, ok)) = &ed.note {
            ui.colored_label(if *ok { palette::ok() } else { palette::bad() }, text);
        }

        for group in midi_learn::groups() {
            let bound = group.slots.iter().filter(|s| !midi_learn::binds_of(&ed.profile, **s).is_empty()).count();
            egui::CollapsingHeader::new(
                RichText::new(format!("{}  ({bound}/{})", group.title, group.slots.len())).size(crate::theme::sz(13.5)),
            )
            .id_salt(("learn_group", group.title))
            .show(ui, |ui| {
                egui::Grid::new(("learn_grid", group.title)).num_columns(3).spacing([14.0, 3.0]).striped(true).show(
                    ui,
                    |ui| {
                        for slot in &group.slots {
                            let waiting = ed.learning.is_some_and(|(s, _)| s == *slot);
                            ui.label(RichText::new(slot.label()).size(crate::theme::sz(13.0)).color(palette::text()));

                            ui.horizontal(|ui| {
                                let ids = midi_learn::binds_of(&ed.profile, *slot);
                                if waiting {
                                    ui.colored_label(palette::deck_a(), "move a control…");
                                } else if ids.is_empty() {
                                    ui.label(RichText::new("—").color(palette::text_dim()));
                                }
                                if !waiting {
                                    for i in ids {
                                        let b = &ed.profile.binds[i];
                                        let text = if b.hires { format!("{} 14-bit", b.msg) } else { b.msg.clone() };
                                        ui.label(
                                            RichText::new(text)
                                                .monospace()
                                                .size(crate::theme::sz(12.5))
                                                .color(palette::text()),
                                        );
                                        if ui.small_button("x").on_hover_text("Remove this control").clicked() {
                                            act = Some(Act::Remove(i));
                                        }
                                    }
                                }
                            });

                            ui.horizontal(|ui| {
                                if waiting {
                                    if ui.small_button("Cancel").clicked() {
                                        act = Some(Act::Cancel);
                                    }
                                } else {
                                    if ui
                                        .small_button("Learn")
                                        .on_hover_text("Replace this function's controls")
                                        .clicked()
                                    {
                                        act = Some(Act::Learn(*slot, true));
                                    }
                                    if ui.small_button("Add").on_hover_text("Bind another control to it too").clicked()
                                    {
                                        act = Some(Act::Learn(*slot, false));
                                    }
                                    if ui.small_button("Clear").clicked() {
                                        act = Some(Act::Clear(*slot));
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    },
                );
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let n = ed.profile.binds.len();
            if ui.add_enabled(n > 0, egui::Button::new("Save and use")).clicked() {
                save = true;
            }
            if ui.button("Close").clicked() {
                close = true;
            }
            ui.label(
                RichText::new(format!("{n} controls{}", if ed.dirty { " · unsaved changes" } else { "" }))
                    .size(crate::theme::sz(12.0))
                    .color(palette::text_dim()),
            );
        });

        match act {
            Some(Act::Learn(slot, replace)) => {
                ed.learning = Some((slot, replace));
                ed.note = None;
                midi_learn::begin(&learn);
            }
            Some(Act::Cancel) => {
                ed.learning = None;
                midi_learn::cancel(&learn);
            }
            Some(Act::Remove(i)) => {
                ed.profile.binds.remove(i);
                ed.dirty = true;
            }
            Some(Act::Clear(slot)) => {
                ed.profile.binds.retain(|b| !slot.owns(b));
                ed.dirty = true;
            }
            None => {}
        }

        if save {
            match ed.profile.compile() {
                Err(e) => ed.note = Some((format!("Can't save: {e}"), false)),
                Ok(_) => match midi_profile::save_user_profile(&ed.profile) {
                    Ok(id) => {
                        let port = ed.port.clone();
                        ed.dirty = false;
                        ed.note = Some((format!("Saved, and now used for {port}."), true));
                        self.midi.reload_profiles();
                        self.midi.choose(&port, Some(&id));
                    }
                    Err(e) => ed.note = Some((format!("Saving failed: {e}"), false)),
                },
            }
        }
        if close {
            midi_learn::cancel(&learn);
            self.prefs.editor = None;
        }
    }
}
