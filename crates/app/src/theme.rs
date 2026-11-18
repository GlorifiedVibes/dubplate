//! The look of the app: the scale of the whole interface, the size of its text and the colour of everything that is
//! not the music's own (the waveform palettes and the hot cues' colours stay as they are).
//!
//! It is read from `theme.toml` in the config folder, or from the file `DUBPLATE_THEME` names, and read again whenever
//! that file changes, so a theme can be worked on with the app open. Every field may be left out: what a file does not
//! say comes from the default look. Preferences > Appearance changes the sizes and writes the file.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A colour written in a theme file as `"#rrggbb"`, or `"#rrggbbaa"` when it is see-through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hex(pub Color32);

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let [r, g, b, a] = self.0.to_srgba_unmultiplied();
        let text = if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") };
        s.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        parse_hex(&text).map(Hex).ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not a #rrggbb colour")))
    }
}

fn parse_hex(text: &str) -> Option<Color32> {
    let h = text.trim().strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Hex {
    Hex(Color32::from_rgb(r, g, b))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// The whole interface, 1.0 being as designed. Ctrl +/- changes it too.
    pub ui_scale: f32,
    /// Every piece of text, on top of `ui_scale`.
    pub text_scale: f32,
    pub readouts: Readouts,
    pub colours: Colours,
}

/// The big numbers on a deck, each on top of `text_scale`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Readouts {
    /// The BPM above the tempo and in the middle of the platter.
    pub bpm: f32,
    /// The time left on the track and the time on the platter.
    pub time: f32,
}

impl Default for Readouts {
    fn default() -> Self {
        Readouts { bpm: 1.0, time: 1.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Colours {
    // ── surfaces, darkest first ──
    /// Behind the panels; also the dark text on a coloured button.
    pub background: Hex,
    /// The decks, windows.
    pub surface: Hex,
    /// Sunk into a surface: the platter, the menu bar, fader slots, the library sidebar.
    pub inset: Hex,
    /// A control at rest: knob bodies, empty pads and buttons.
    pub raised: Hex,
    pub hover: Hex,
    pub pressed: Hex,
    /// Lines between panels and rows.
    pub divider: Hex,
    /// The edge of a knob, fader slot, pad or panel.
    pub outline: Hex,
    // ── knobs and faders ──
    /// The ring a knob's value runs round, the platter's ring.
    pub track: Hex,
    pub handle: Hex,
    pub handle_active: Hex,
    /// Scale marks and the line along a fader slot.
    pub tick: Hex,
    /// The grip line on a fader handle.
    pub handle_line: Hex,
    // ── text ──
    pub text: Hex,
    pub text_dim: Hex,
    /// Between the two: the second line of a library row.
    pub text_mid: Hex,
    /// The brightest things: the BPM, the playhead, a knob's pointer.
    pub text_bright: Hex,
    /// Plain labels and the text of buttons in windows and menus (egui's own widgets).
    pub label: Hex,
    pub button_text: Hex,
    // ── the decks ──
    pub deck_a: Hex,
    pub deck_b: Hex,
    /// The selected row in the library.
    pub selected: Hex,
    // ── what things are ──
    pub ok: Hex,
    pub warn: Hex,
    pub bad: Hex,
    pub record: Hex,
    /// The dot on the platter that turns with the music.
    pub beat_marker: Hex,
    /// The platter's ring when the track has under 30 s left.
    pub ending: Hex,
    pub cue: Hex,
    pub play: Hex,
    pub loop_on: Hex,
    /// The filter knob's colour below and above the middle.
    pub filter_low: Hex,
    pub filter_high: Hex,
    /// Drums, bass, other, vocals.
    pub stems: [Hex; 4],
    // ── the sections drawn over a track ──
    pub intro: Hex,
    pub build: Hex,
    pub drop: Hex,
    pub groove: Hex,
    #[serde(rename = "break")]
    pub break_: Hex,
    pub outro: Hex,
}

impl Default for Colours {
    fn default() -> Self {
        Colours {
            background: rgb(10, 10, 15),
            surface: rgb(18, 18, 26),
            inset: rgb(12, 12, 18),
            raised: rgb(22, 22, 32),
            hover: rgb(32, 32, 46),
            pressed: rgb(48, 48, 65),
            divider: rgb(28, 28, 42),
            outline: rgb(55, 55, 75),
            track: rgb(38, 38, 55),
            handle: rgb(40, 40, 58),
            handle_active: rgb(58, 58, 80),
            tick: rgb(80, 80, 110),
            handle_line: rgb(145, 145, 165),
            text: rgb(210, 210, 228),
            text_dim: rgb(95, 95, 115),
            text_mid: rgb(185, 185, 205),
            text_bright: rgb(255, 255, 255),
            label: rgb(140, 140, 140),
            button_text: rgb(180, 180, 180),
            deck_a: rgb(0, 196, 255),
            deck_b: rgb(255, 100, 30),
            selected: rgb(0, 74, 110),
            ok: rgb(70, 200, 110),
            warn: rgb(230, 170, 60),
            bad: rgb(235, 80, 80),
            record: rgb(160, 30, 30),
            beat_marker: rgb(235, 60, 60),
            ending: rgb(235, 150, 30),
            cue: rgb(220, 155, 0),
            play: rgb(0, 195, 75),
            loop_on: rgb(0, 200, 90),
            filter_low: rgb(255, 130, 0),
            filter_high: rgb(0, 150, 255),
            stems: [rgb(255, 80, 80), rgb(60, 160, 255), rgb(80, 210, 100), rgb(210, 160, 255)],
            intro: rgb(110, 130, 170),
            build: rgb(240, 170, 40),
            drop: rgb(235, 70, 110),
            groove: rgb(60, 180, 170),
            break_: rgb(90, 120, 235),
            outro: rgb(110, 130, 170),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme { ui_scale: 1.0, text_scale: 1.0, readouts: Readouts::default(), colours: Colours::default() }
    }
}

/// The looks offered in Preferences, each a starting point to adjust.
pub const PRESETS: [&str; 3] = ["Default", "Large text", "High contrast"];

impl Theme {
    pub fn preset(name: &str) -> Theme {
        let mut t = Theme::default();
        match name {
            "Large text" => {
                t.text_scale = 1.2;
                t.readouts = Readouts { bpm: 1.6, time: 1.3 };
            }
            "High contrast" => {
                t.text_scale = 1.1;
                t.readouts = Readouts { bpm: 1.4, time: 1.2 };
                let c = &mut t.colours;
                c.background = rgb(0, 0, 0);
                c.surface = rgb(12, 12, 16);
                c.inset = rgb(4, 4, 6);
                c.raised = rgb(26, 26, 34);
                c.divider = rgb(60, 60, 75);
                c.outline = rgb(110, 110, 135);
                c.track = rgb(60, 60, 80);
                c.tick = rgb(130, 130, 160);
                c.handle_line = rgb(220, 220, 235);
                c.text = rgb(250, 250, 255);
                c.text_dim = rgb(165, 165, 185);
                c.text_mid = rgb(225, 225, 240);
                c.label = rgb(210, 210, 220);
                c.button_text = rgb(235, 235, 245);
            }
            _ => {}
        }
        t
    }

    /// Keeps the numbers in a range the layout can take, so a typo cannot make the app unusable.
    fn clamped(mut self) -> Theme {
        let fix = |v: f32, lo: f32, hi: f32, def: f32| if v.is_finite() { v.clamp(lo, hi) } else { def };
        self.ui_scale = fix(self.ui_scale, 0.5, 3.0, 1.0);
        self.text_scale = fix(self.text_scale, 0.6, 2.0, 1.0);
        self.readouts.bpm = fix(self.readouts.bpm, 0.5, 3.0, 1.0);
        self.readouts.time = fix(self.readouts.time, 0.5, 2.5, 1.0);
        self
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string_pretty(self).unwrap_or_default();
        format!(
            "# Dubplate theme. Every value may be left out: what is not here comes from the default look.\n\
             # Sizes are multipliers (1.0 = as designed); colours are \"#rrggbb\" or \"#rrggbbaa\".\n\
             # The app reads this file again whenever it changes. See docs/theme.md for what each colour is used for.\n\n\
             {body}"
        )
    }
}

static CURRENT: LazyLock<RwLock<Arc<Theme>>> = LazyLock::new(|| RwLock::new(Arc::new(Theme::default())));

/// The theme in use.
pub fn get() -> Arc<Theme> {
    CURRENT.read().map(|t| t.clone()).unwrap_or_default()
}

pub fn set(t: Theme) {
    if let Ok(mut c) = CURRENT.write() {
        *c = Arc::new(t.clamped());
    }
}

/// A text size as designed, in the theme's scale.
pub fn sz(size: f32) -> f32 {
    size * get().text_scale
}

/// The size of a BPM readout designed at `size`.
pub fn bpm_size(size: f32) -> f32 {
    let t = get();
    size * t.text_scale * t.readouts.bpm
}

/// The size of a time readout designed at `size`.
pub fn time_size(size: f32) -> f32 {
    let t = get();
    size * t.text_scale * t.readouts.time
}

pub fn path() -> PathBuf {
    std::env::var_os("DUBPLATE_THEME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dubplate_engine::paths::config_dir().join("theme.toml"))
}

struct Watch {
    /// When the file was last looked at, and its modified time then.
    checked: Option<Instant>,
    seen: Option<SystemTime>,
    /// What was wrong with the file the last time it was read.
    error: Option<String>,
    /// The zoom last given to egui, to notice Ctrl +/- changing it.
    zoom: Option<f32>,
    /// The text scale egui's own text styles were last sized for.
    text: Option<f32>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { checked: None, seen: None, error: None, zoom: None, text: None });

fn modified() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// Reads the theme file, keeping the theme in use when it is missing or cannot be read.
pub fn load() {
    let mut w = WATCH.lock().unwrap();
    w.seen = modified();
    let Ok(text) = std::fs::read_to_string(path()) else {
        w.error = None;
        return;
    };
    match toml::from_str::<Theme>(&text) {
        Ok(t) => {
            set(t);
            w.error = None;
        }
        Err(e) => {
            log::warn!("theme {}: {e}", path().display());
            w.error = Some(e.message().to_owned());
        }
    }
}

/// Writes the theme in use to the theme file.
pub fn save() -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, get().to_toml())?;
    WATCH.lock().unwrap().seen = modified();
    Ok(())
}

/// What was wrong with the theme file, if it could not be read.
pub fn error() -> Option<String> {
    WATCH.lock().unwrap().error.clone()
}

/// Called once a frame: reads the file again when it has changed (looking once a second), keeps egui's zoom and the
/// theme's `ui_scale` the same whichever of them changed, and sizes egui's own text styles.
pub fn frame(ctx: &egui::Context) {
    let due = {
        let mut w = WATCH.lock().unwrap();
        let due = w.checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if due {
            w.checked = Some(Instant::now());
        }
        due && modified() != w.seen
    };
    if due {
        load();
        ctx.request_repaint();
    }

    let t = get();
    let zoom = ctx.zoom_factor();
    let mut w = WATCH.lock().unwrap();
    // Ctrl +/- changed egui's zoom since the last frame (and the theme did not change): the theme follows it.
    let keys = w.zoom.is_some_and(|z| (z - zoom).abs() > 1e-3 && (t.ui_scale - z).abs() < 1e-3);
    if keys {
        let mut n = (*t).clone();
        n.ui_scale = zoom;
        set(n);
        w.zoom = Some(get().ui_scale);
    } else if (t.ui_scale - zoom).abs() > 1e-3 {
        ctx.set_zoom_factor(t.ui_scale);
        w.zoom = Some(t.ui_scale);
    } else {
        w.zoom = Some(zoom);
    }
    let text = t.text_scale;
    let restyle = w.text != Some(text);
    w.text = Some(text);
    drop(w);
    if keys {
        let _ = save();
    }
    if !restyle {
        return;
    }
    ctx.style_mut(|s| {
        let base = egui::Style::default().text_styles;
        for (style, font) in s.text_styles.iter_mut() {
            if let Some(b) = base.get(style) {
                font.size = b.size * text;
            }
        }
    });
}

/// The theme's colours by role, for code that draws.
pub mod palette {
    use egui::Color32;

    macro_rules! roles {
        ($($name:ident),* $(,)?) => {
            $(pub fn $name() -> Color32 { super::get().colours.$name.0 })*
        };
    }
    roles!(
        background,
        surface,
        inset,
        raised,
        hover,
        pressed,
        divider,
        outline,
        track,
        handle,
        handle_active,
        tick,
        handle_line,
        text,
        text_dim,
        text_mid,
        text_bright,
        label,
        button_text,
        deck_a,
        deck_b,
        selected,
        ok,
        warn,
        bad,
        record,
        beat_marker,
        ending,
        cue,
        play,
        loop_on,
        filter_low,
        filter_high,
        intro,
        build,
        drop,
        groove,
        outro,
    );

    pub fn break_() -> Color32 {
        super::get().colours.break_.0
    }

    pub fn stems() -> [Color32; 4] {
        super::get().colours.stems.map(|h| h.0)
    }

    pub fn deck(d: usize) -> Color32 {
        if d == 0 {
            deck_a()
        } else {
            deck_b()
        }
    }

    /// `c` with its opacity set to `a`.
    pub fn alpha(c: Color32, a: u8) -> Color32 {
        let [r, g, b, _] = c.to_srgba_unmultiplied();
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_file_may_say_only_what_it_changes() {
        let t: Theme =
            toml::from_str("text_scale = 1.3\n[readouts]\nbpm = 2.0\n[colours]\ndeck_a = \"#ff00ff\"\n").unwrap();
        assert_eq!(t.text_scale, 1.3);
        assert_eq!(t.readouts.bpm, 2.0);
        assert_eq!(t.readouts.time, 1.0);
        assert_eq!(t.colours.deck_a.0, Color32::from_rgb(255, 0, 255));
        assert_eq!(t.colours.deck_b, Colours::default().deck_b);
    }

    #[test]
    fn a_written_theme_reads_back_the_same() {
        for name in PRESETS {
            let t = Theme::preset(name);
            assert_eq!(toml::from_str::<Theme>(&t.to_toml()).unwrap(), t, "{name}");
        }
    }

    #[test]
    fn colours_are_hex_with_an_optional_alpha() {
        assert_eq!(parse_hex("#0a0B0f"), Some(Color32::from_rgb(10, 11, 15)));
        assert_eq!(parse_hex("#ffffff80"), Some(Color32::from_rgba_unmultiplied(255, 255, 255, 128)));
        assert_eq!(parse_hex("0a0b0f"), None);
        assert_eq!(parse_hex("#0a0b0"), None);
        assert_eq!(parse_hex("#é0a0b0"), None);
        assert!(toml::from_str::<Theme>("[colours]\ntext = \"red\"\n").is_err());
    }

    #[test]
    fn sizes_out_of_range_are_kept_usable() {
        let t = Theme { ui_scale: 40.0, text_scale: f32::NAN, ..Theme::default() }.clamped();
        assert_eq!(t.ui_scale, 3.0);
        assert_eq!(t.text_scale, 1.0);
    }
}
