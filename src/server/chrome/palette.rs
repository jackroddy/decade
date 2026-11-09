pub type Rgba = [f32; 4];

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
pub const LIGHTER_BACKGROUND: Rgba = hex(0x2B2436);
pub const FOREGROUND: Rgba = hex(0xE7DDC8);
pub const LIGHT_FOREGROUND: Rgba = hex(0x8B8797);
pub const RED: Rgba = hex(0xC26A7A);
pub const YELLOW: Rgba = hex(0xDA915C);
pub const GREEN: Rgba = hex(0x69A676);
pub const CYAN: Rgba = hex(0x57ABAD);
pub const FADED_FOREGROUND: Rgba = hex(0xC9C0B0);
pub const LIGHT_RED: Rgba = hex(0xE39AA6);
pub const LIGHT_YELLOW: Rgba = hex(0xE8AC80);
pub const LIGHT_GREEN: Rgba = hex(0x8CC497);
pub const DARK_RED: Rgba = hex(0x3A2530);
pub const DARK_YELLOW: Rgba = hex(0x3A2E24);
pub const DARK_GREEN: Rgba = hex(0x2A3229);
pub const RAISED_BACKGROUND: Rgba = hex(0x211B26);
pub const BORDER: Rgba = hex(0x4A4458);
pub const MID_RED: Rgba = hex(0x4D2F3E);
pub const MID_YELLOW: Rgba = hex(0x4D3B2C);
pub const MID_GREEN: Rgba = hex(0x354234);
pub const SHADOW: Rgba = [0.0, 0.0, 0.0, 0.4];
