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
        for y in 0..cell_height {
            let line = (row * cell_height + y) * width + column * cell_width;
            for x in 0..cell_width {
                let alpha = coverage[y * cell_width + x] as f32 / 255.0;
                let pixel = &mut pixels[(line + x) * 4..(line + x) * 4 + 4];
                for (channel, value) in pixel[..3].iter_mut().enumerate() {
                    let mixed = cell.bg[channel] + (cell.fg[channel] - cell.bg[channel]) * alpha;
                    *value = (mixed * 255.0).round() as u8;
                }
                pixel[3] = 255;
            }
        }
    }
    pixels
}
