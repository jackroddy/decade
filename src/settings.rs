use std::path::{Path, PathBuf};
use std::{env, fs, io};

use serde::{Deserialize, Serialize};

const VERSION: i64 = 1;

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

impl Knob {
    pub fn label(self) -> &'static str {
        match self {
            Knob::FontSize => "Font size",
            Knob::PaletteSize => "Palette font size",
            Knob::BarHold => "Bar hold",
            Knob::GhostScale => "Drag size",
            Knob::GhostOpacity => "Drag opacity",
            Knob::Grab => "Grab animation",
        }
    }

    fn step_and_range(self) -> (f64, f64, f64) {
        match self {
            Knob::FontSize | Knob::PaletteSize => (1.0, 8.0, 40.0),
            Knob::BarHold | Knob::Grab => (10.0, 0.0, 1000.0),
            Knob::GhostScale | Knob::GhostOpacity => (0.05, 0.05, 1.0),
        }
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
        let (step, min, max) = self.step_and_range();
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
        match self {
            Knob::FontSize | Knob::PaletteSize => format!("{value} px"),
            Knob::BarHold | Knob::Grab => format!("{value} ms"),
            Knob::GhostScale | Knob::GhostOpacity => format!("{:.0}%", value * 100.0),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("decade").join("config.toml"))
}

pub fn load(path: &Path) -> Result<Settings, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(error) => return Err(error.to_string()),
    };
    parse(&text)
}

fn parse(text: &str) -> Result<Settings, String> {
    let table: toml::Table = toml::from_str(text).map_err(|error| error.to_string())?;
    match table
        .get("version")
        .and_then(|version| version.as_integer())
    {
        Some(VERSION) => {}
        Some(other) => return Err(format!("version {other} is not one decade reads")),
        None => return Err("the file has no version".to_string()),
    }
    toml::from_str(text).map_err(|error| error.to_string())
}

pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, render(settings))
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
