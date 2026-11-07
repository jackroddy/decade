#![allow(dead_code)]

use crate::grid::Rgba;

const fn hex(rgb: u32) -> Rgba {
    [
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
        1.0,
    ]
}

pub const ACCENT: Rgba = hex(0xDA745B);
pub const SELECTION: Rgba = hex(0x37344A);
pub const MUTED: Rgba = hex(0x6D6977);
pub const BACKGROUND: Rgba = hex(0x1F1A24);
pub const DARK_BACKGROUND: Rgba = hex(0x17121A);
pub const DARKER_BACKGROUND: Rgba = hex(0x09090A);
pub const LIGHTER_BACKGROUND: Rgba = hex(0x2B2436);
pub const FOREGROUND: Rgba = hex(0xE7DDC8);
pub const DARK_FOREGROUND: Rgba = hex(0x6D6977);
pub const LIGHT_FOREGROUND: Rgba = hex(0x8B8797);
pub const RED: Rgba = hex(0xC26A7A);
pub const YELLOW: Rgba = hex(0xDA915C);
pub const ORANGE: Rgba = hex(0xDA745B);
pub const GREEN: Rgba = hex(0x69A676);
pub const CYAN: Rgba = hex(0x57ABAD);
pub const BLUE: Rgba = hex(0x5C9FB9);
pub const MAGENTA: Rgba = hex(0xA26DC9);
pub const BROWN: Rgba = hex(0x8E5C4C);
