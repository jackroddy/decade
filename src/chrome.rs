use crate::grid::{Cell, Grid};
use crate::text::{Font, Glyphs};

pub fn raster(grid: &Grid, font: &Font, glyphs: &mut Glyphs) -> Vec<u8> {
    let mut pixels =
        Vec::with_capacity(grid.cells.len() * (font.cell_width * font.cell_height) as usize * 4);
    for row in grid.cells.chunks(grid.columns as usize) {
        pixels.extend(raster_run(row, font, glyphs));
    }
    pixels
}

pub fn raster_run(cells: &[Cell], font: &Font, glyphs: &mut Glyphs) -> Vec<u8> {
    let cell_width = font.cell_width as usize;
    let cell_height = font.cell_height as usize;
    let width = cells.len() * cell_width;
    let mut pixels = vec![0u8; width * cell_height * 4];
    for (column, cell) in cells.iter().enumerate() {
        let coverage = glyphs.coverage(font, cell.ch);
        let bg_alpha = cell.bg[3];
        let bg: [u8; 3] = std::array::from_fn(|c| (cell.bg[c] * bg_alpha * 255.0).round() as u8);
        for y in 0..cell_height {
            let line = y * width + column * cell_width;
            for x in 0..cell_width {
                let pixel = &mut pixels[(line + x) * 4..(line + x) * 4 + 4];
                let glyph = coverage[y * cell_width + x];
                if glyph == 0 {
                    pixel[..3].copy_from_slice(&bg);
                    pixel[3] = (bg_alpha * 255.0).round() as u8;
                    continue;
                }
                let glyph = glyph as f32 / 255.0;
                let alpha = bg_alpha + (1.0 - bg_alpha) * glyph;
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
