use crate::grid::Grid;
use crate::text::{Font, Glyphs};

pub fn raster(grid: &Grid, font: &Font, glyphs: &mut Glyphs) -> Vec<u8> {
    let cell_width = font.cell_width as usize;
    let cell_height = font.cell_height as usize;
    let width = grid.columns as usize * cell_width;
    let height = grid.rows as usize * cell_height;
    let mut pixels = vec![0u8; width * height * 4];
    for (i, cell) in grid.cells.iter().enumerate() {
        let column = i % grid.columns as usize;
        let row = i / grid.columns as usize;
        let coverage = glyphs.coverage(font, cell.ch);
        let bg_alpha = cell.bg[3];
        for y in 0..cell_height {
            let line = (row * cell_height + y) * width + column * cell_width;
            for x in 0..cell_width {
                let glyph = coverage[y * cell_width + x] as f32 / 255.0;
                let alpha = bg_alpha + (1.0 - bg_alpha) * glyph;
                let pixel = &mut pixels[(line + x) * 4..(line + x) * 4 + 4];
                for (channel, value) in pixel[..3].iter_mut().enumerate() {
                    let mixed =
                        cell.bg[channel] * bg_alpha * (1.0 - glyph) + cell.fg[channel] * glyph;
                    *value = (mixed * 255.0).round() as u8;
                }
                pixel[3] = (alpha * 255.0).round() as u8;
            }
        }
    }
    pixels
}
