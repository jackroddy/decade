use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::files;

const VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_size: f32,
    pub palette_size: f32,
    pub bar_hold_ms: u64,
    pub ghost_scale: f64,
    pub ghost_opacity: f32,
    pub grab_ms: u64,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            font_size: 14.0,
            palette_size: 20.0,
            bar_hold_ms: 50,
            ghost_scale: 0.2,
            ghost_opacity: 0.8,
            grab_ms: 50,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Knob {
    FontSize,
    PaletteSize,
    BarHold,
    GhostScale,
    GhostOpacity,
    Grab,
}

pub const KNOBS: [Knob; 6] = [
    Knob::FontSize,
    Knob::PaletteSize,
    Knob::BarHold,
    Knob::GhostScale,
    Knob::GhostOpacity,
    Knob::Grab,
];

#[derive(Clone, Copy)]
enum Unit {
    Px,
    Ms,
    Percent,
}

/// Each knob's label, step, least and greatest value, and unit, in the
/// order of `Knob`'s variants.
const RANGES: [(&str, f64, f64, f64, Unit); 6] = [
    ("Font size", 1.0, 8.0, 40.0, Unit::Px),
    ("Palette font size", 1.0, 8.0, 40.0, Unit::Px),
    ("Ctrl bar delay", 10.0, 0.0, 1000.0, Unit::Ms),
    ("Drag size", 0.05, 0.05, 1.0, Unit::Percent),
    ("Drag opacity", 0.05, 0.05, 1.0, Unit::Percent),
    ("Drag pickup time", 10.0, 0.0, 1000.0, Unit::Ms),
];

impl Knob {
    pub fn label(self) -> &'static str {
        RANGES[self as usize].0
    }

    fn get(self, settings: &Settings) -> f64 {
        match self {
            Knob::FontSize => settings.font_size as f64,
            Knob::PaletteSize => settings.palette_size as f64,
            Knob::BarHold => settings.bar_hold_ms as f64,
            Knob::GhostScale => settings.ghost_scale,
            Knob::GhostOpacity => settings.ghost_opacity as f64,
            Knob::Grab => settings.grab_ms as f64,
        }
    }

    pub fn step(self, settings: &mut Settings, up: bool) {
        let (_, step, min, max, _) = RANGES[self as usize];
        let delta = if up { step } else { -step };
        let value = ((self.get(settings) + delta) / step).round() * step;
        let value = value.clamp(min, max);
        match self {
            Knob::FontSize => settings.font_size = value as f32,
            Knob::PaletteSize => settings.palette_size = value as f32,
            Knob::BarHold => settings.bar_hold_ms = value as u64,
            Knob::GhostScale => settings.ghost_scale = value,
            Knob::GhostOpacity => settings.ghost_opacity = value as f32,
            Knob::Grab => settings.grab_ms = value as u64,
        }
    }

    pub fn show(self, settings: &Settings) -> String {
        let value = self.get(settings);
        match RANGES[self as usize].4 {
            Unit::Px => format!("{value} px"),
            Unit::Ms => format!("{value} ms"),
            Unit::Percent => format!("{:.0}%", value * 100.0),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let config = files::xdg_home("XDG_CONFIG_HOME", ".config")?;
    Some(config.join(files::DIR).join("config.toml"))
}

pub fn load(path: &Path) -> Result<Settings, String> {
    match files::read(path)? {
        Some(text) => parse(&text),
        None => Ok(Settings::default()),
    }
}

fn parse(text: &str) -> Result<Settings, String> {
    match files::version(text)? {
        VERSION => toml::from_str(text).map_err(|error| error.to_string()),
        other => Err(format!("version {other} is not one decade reads")),
    }
}

pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    files::write(path, &render(settings))
}

fn render(settings: &Settings) -> String {
    let body = toml::to_string(settings).expect("settings serialize to TOML");
    format!("version = {VERSION}\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_settings_read_back_the_same() {
        let settings = Settings {
            font_size: 16.0,
            ghost_scale: 0.35,
            ..Settings::default()
        };
        assert_eq!(parse(&render(&settings)), Ok(settings));
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        let settings = parse("version = 1\nbar_hold_ms = 120\n").unwrap();
        assert_eq!(settings.bar_hold_ms, 120);
        assert_eq!(settings.font_size, Settings::default().font_size);
    }

    #[test]
    fn a_file_without_version_one_is_refused() {
        assert!(parse("version = 2\n").is_err());
        assert!(parse("font_size = 12.0\n").is_err());
    }

    #[test]
    fn every_knob_has_the_range_in_its_variants_place() {
        let labels = KNOBS.map(Knob::label);
        assert_eq!(labels[0], "Font size");
        assert_eq!(labels[5], "Drag pickup time");
        assert_eq!(Knob::Grab.show(&Settings::default()), "50 ms");
        assert_eq!(Knob::GhostScale.show(&Settings::default()), "20%");
    }

    #[test]
    fn stepping_moves_by_the_knobs_step_and_stops_at_its_range() {
        let mut settings = Settings::default();
        Knob::GhostOpacity.step(&mut settings, true);
        assert_eq!(settings.ghost_opacity, 0.85);
        for _ in 0..10 {
            Knob::GhostOpacity.step(&mut settings, true);
        }
        assert_eq!(settings.ghost_opacity, 1.0);
        Knob::BarHold.step(&mut settings, false);
        Knob::BarHold.step(&mut settings, false);
        assert_eq!(settings.bar_hold_ms, 30);
    }
}
