use std::sync::OnceLock;

pub type Rgba = [f32; 4];

pub const SHADOW: Rgba = [0.0, 0.0, 0.0, 0.4];

static PALETTE: OnceLock<Palette> = OnceLock::new();

/// The colours chrome is drawn in: an Omarchy theme's keys, under its
/// names, and shades blended from them.
#[derive(Clone, PartialEq, Debug)]
pub struct Palette {
    pub accent: Rgba,
    pub selection: Rgba,
    pub muted: Rgba,
    pub background: Rgba,
    pub dark_background: Rgba,
    pub lighter_background: Rgba,
    pub foreground: Rgba,
    pub light_foreground: Rgba,
    pub red: Rgba,
    pub yellow: Rgba,
    pub green: Rgba,
    pub cyan: Rgba,
    pub magenta: Rgba,
    pub faded_foreground: Rgba,
    pub raised_background: Rgba,
    pub border: Rgba,
    pub light_red: Rgba,
    pub light_yellow: Rgba,
    pub light_green: Rgba,
    pub mid_red: Rgba,
    pub mid_yellow: Rgba,
    pub mid_green: Rgba,
    pub dark_red: Rgba,
    pub dark_yellow: Rgba,
    pub dark_green: Rgba,
}

impl Palette {
    /// The palette of an Omarchy `colors.toml`, with a fixed colour for
    /// each key the text lacks or does not parse.
    pub fn from_theme(text: &str) -> Palette {
        let table: toml::Table = text.parse().unwrap_or_default();
        let key = |name: &str, fallback: u32| {
            table
                .get(name)
                .and_then(toml::Value::as_str)
                .and_then(parse_hex)
                .unwrap_or(hex(fallback))
        };
        let background = key("background", 0x1F1A24);
        let lighter_background = key("lighter_background", 0x2B2436);
        let foreground = key("foreground", 0xE7DDC8);
        let selection = key("selection", 0x37344A);
        let muted = key("muted", 0x6D6977);
        let red = key("red", 0xC26A7A);
        let yellow = key("yellow", 0xDA915C);
        let green = key("green", 0x69A676);
        Palette {
            accent: key("accent", 0xDA745B),
            selection,
            muted,
            background,
            dark_background: key("dark_background", 0x17121A),
            lighter_background,
            foreground,
            light_foreground: key("light_foreground", 0x8B8797),
            red,
            yellow,
            green,
            cyan: key("cyan", 0x57ABAD),
            magenta: key("magenta", 0xA26DC9),
            faded_foreground: mix(foreground, background, 0.15),
            raised_background: mix(background, lighter_background, 0.15),
            border: mix(selection, muted, 1.0 / 3.0),
            light_red: mix(red, foreground, 0.45),
            light_yellow: mix(yellow, foreground, 0.45),
            light_green: mix(green, foreground, 0.45),
            mid_red: mix(background, red, 0.28),
            mid_yellow: mix(background, yellow, 0.28),
            mid_green: mix(background, green, 0.28),
            dark_red: mix(background, red, 0.15),
            dark_yellow: mix(background, yellow, 0.15),
            dark_green: mix(background, green, 0.15),
        }
    }
}

/// The palette set at start, or the fixed one when none was set.
pub fn get() -> &'static Palette {
    PALETTE.get_or_init(|| Palette::from_theme(""))
}

/// Set the palette that `get` returns.
///
/// # Panics
///
/// When a palette was already set or read.
pub fn set(palette: Palette) {
    PALETTE
        .set(palette)
        .expect("the palette is set once, before it is read");
}

/// The colour `t` of the way from `from` to `to`, for `t` from 0 to 1.
pub fn mix(from: Rgba, to: Rgba, t: f32) -> Rgba {
    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
}

fn hex(rgb: u32) -> Rgba {
    [
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
        1.0,
    ]
}

fn parse_hex(text: &str) -> Option<Rgba> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_key_replaces_its_fixed_colour_and_the_shades_blended_from_it() {
        let palette = Palette::from_theme("mode = \"dark\"\n# a comment\nred = \"#FF0000\"\n");
        let fixed = Palette::from_theme("");
        assert_eq!(palette.red, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(palette.light_red, mix(palette.red, fixed.foreground, 0.45));
        assert_eq!(palette.dark_red, mix(fixed.background, palette.red, 0.15));
        assert_eq!(palette.green, fixed.green);
    }

    #[test]
    fn a_key_that_is_not_a_six_digit_hex_colour_keeps_its_fixed_colour() {
        let palette = Palette::from_theme("red = \"FF0000\"\ngreen = \"#F00\"\ncyan = 3\n");
        let fixed = Palette::from_theme("");
        assert_eq!(palette.red, fixed.red);
        assert_eq!(palette.green, fixed.green);
        assert_eq!(palette.cyan, fixed.cyan);
    }

    #[test]
    fn text_that_is_not_toml_gives_the_fixed_palette() {
        assert_eq!(Palette::from_theme("red = "), Palette::from_theme(""));
    }
}
