use std::collections::HashMap;

use decade_ui::palette::Rgba;
use decade_ui::{CellRect, Face, Fonts};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Physical, Point, Rectangle, Size};

use crate::server::chrome::settings::Settings;

/// The chrome and palette fonts at one scale, and the textures of the
/// text drawn with them.
pub struct Text {
    pub fonts: Fonts,
    scale: f64,
    entries: HashMap<Key, (TextTexture, u64)>,
    frame: u64,
}

pub struct TextTexture {
    pub texture: GlesTexture,
    pub offset: Point<i32, Physical>,
    pub size: Size<i32, Physical>,
}

type Key = (String, Face, [u32; 4]);

impl Text {
    pub fn new(settings: &Settings, scale: f64) -> Text {
        let scale_px = |px: f32| px * scale as f32;
        Text {
            fonts: Fonts::load(
                scale_px(settings.font_size),
                scale_px(settings.palette_size),
            ),
            scale,
            entries: HashMap::new(),
            frame: 0,
        }
    }

    /// The pixels the cells cover, on the chrome font's grid.
    pub fn px_rect(&self, rect: CellRect) -> Rectangle<i32, Physical> {
        let font = &self.fonts.chrome;
        Rectangle::new(
            (
                (rect.column * font.cell_width) as i32,
                (rect.row * font.cell_height) as i32,
            )
                .into(),
            (
                (rect.columns * font.cell_width) as i32,
                (rect.rows * font.cell_height) as i32,
            )
                .into(),
        )
    }

    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries.retain(|_, (_, used)| frame - *used < 120);
        self.frame += 1;
    }

    pub fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        face: Face,
        text: &str,
        color: Rgba,
    ) -> Option<&TextTexture> {
        let key = (text.to_string(), face, color.map(f32::to_bits));
        if !self.entries.contains_key(&key) {
            let run = self.fonts.rasterize(face, text, color);
            let width = round_trip(run.width, self.scale);
            let height = round_trip(run.height, self.scale);
            let pixels = pad(&run.pixels, run.width, run.height, width, height);
            let texture = renderer
                .import_memory(&pixels, Fourcc::Abgr8888, (width, height).into(), false)
                .ok()?;
            let entry = TextTexture {
                texture,
                offset: (run.left, run.top).into(),
                size: (width, height).into(),
            };
            self.entries.insert(key.clone(), (entry, self.frame));
        }
        let (entry, used) = self.entries.get_mut(&key)?;
        *used = self.frame;
        Some(entry)
    }
}

fn round_trip(pixels: i32, scale: f64) -> i32 {
    (pixels..pixels + 8)
        .find(|&candidate| {
            let logical = (candidate as f64 / scale).round();
            (logical * scale).round() as i32 == candidate
        })
        .unwrap_or(pixels)
}

fn pad(pixels: &[u8], width: i32, height: i32, to_width: i32, to_height: i32) -> Vec<u8> {
    let mut out = vec![0u8; (to_width * to_height * 4) as usize];
    for row in 0..height {
        let from = (row * width * 4) as usize;
        let to = (row * to_width * 4) as usize;
        out[to..to + (width * 4) as usize]
            .copy_from_slice(&pixels[from..from + (width * 4) as usize]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_padded_size_survives_the_trip_through_logical_pixels() {
        for pixels in 1..200 {
            let padded = round_trip(pixels, 1.25);
            assert!(padded >= pixels);
            let logical = (padded as f64 / 1.25).round();
            assert_eq!((logical * 1.25).round() as i32, padded);
        }
    }
}
