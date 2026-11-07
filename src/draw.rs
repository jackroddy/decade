use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Physical, Point, Size};
use swash::scale::ScaleContext;

use crate::panes::CellRect;
use crate::text::{Font, Rgba, raster_run};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Chrome,
    Menu,
}

pub enum Shape {
    Rect {
        at: Point<i32, Physical>,
        size: Size<i32, Physical>,
        color: Rgba,
    },
    Text {
        at: Point<i32, Physical>,
        text: String,
        face: Face,
        color: Rgba,
    },
    Rounded {
        at: Point<i32, Physical>,
        size: Size<i32, Physical>,
        radius: i32,
        fill: Rgba,
        border: Rgba,
    },
}

pub struct Scene {
    cell_width: i32,
    cell_height: i32,
    pub shapes: Vec<Shape>,
}

impl Scene {
    pub fn new(font: &Font) -> Scene {
        Scene {
            cell_width: font.cell_width as i32,
            cell_height: font.cell_height as i32,
            shapes: Vec::new(),
        }
    }

    pub fn line_width(&self) -> i32 {
        (self.cell_width / 8).max(1)
    }

    pub fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: Rgba) {
        if width > 0 && height > 0 {
            self.shapes.push(Shape::Rect {
                at: (x, y).into(),
                size: (width, height).into(),
                color,
            });
        }
    }

    pub fn rounded(
        &mut self,
        x: i32,
        y: i32,
        size: (i32, i32),
        radius: i32,
        fill: Rgba,
        border: Rgba,
    ) {
        self.shapes.push(Shape::Rounded {
            at: (x, y).into(),
            size: size.into(),
            radius,
            fill,
            border,
        });
    }

    pub fn text_px(&mut self, x: i32, y: i32, text: &str, face: Face, color: Rgba) {
        if !text.is_empty() {
            self.shapes.push(Shape::Text {
                at: (x, y).into(),
                text: text.to_string(),
                face,
                color,
            });
        }
    }

    pub fn fill(&mut self, rect: CellRect, color: Rgba) {
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        self.rect(x, y, width, height, color);
    }

    pub fn text(&mut self, column: u32, row: u32, text: &str, color: Rgba) {
        let (x, y) = self.px(column, row);
        self.text_px(x, y, text, Face::Chrome, color);
    }

    pub fn outline(&mut self, rect: CellRect, color: Rgba) {
        if rect.columns < 2 || rect.rows < 2 {
            return;
        }
        let line = self.line_width();
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        let (left, top) = (x + self.cell_width / 2, y + self.cell_height / 2);
        let (right, bottom) = (
            x + width - self.cell_width / 2,
            y + height - self.cell_height / 2,
        );
        self.box_lines(left, top, right - left + line, bottom - top + line, color);
    }

    pub fn frame(&mut self, rect: CellRect, color: Rgba) {
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        self.box_lines(x, y, width, height, color);
    }

    /// A line one stroke wide down the right edge of `rect`.
    pub fn right_edge(&mut self, rect: CellRect, color: Rgba) {
        let (x, y) = self.px(rect.column + rect.columns, rect.row);
        let (_, height) = self.px(0, rect.rows);
        let line = self.line_width();
        self.rect(x - line, y, line, height, color);
    }

    pub fn box_lines(&mut self, x: i32, y: i32, width: i32, height: i32, color: Rgba) {
        let line = self.line_width();
        self.rect(x, y, width, line, color);
        self.rect(x, y + height - line, width, line, color);
        self.rect(x, y, line, height, color);
        self.rect(x + width - line, y, line, height, color);
    }

    fn px(&self, column: u32, row: u32) -> (i32, i32) {
        (
            column as i32 * self.cell_width,
            row as i32 * self.cell_height,
        )
    }
}

pub struct TextTexture {
    pub texture: GlesTexture,
    pub offset: Point<i32, Physical>,
    pub size: Size<i32, Physical>,
}

type Key = (String, Face, [u32; 4]);

pub struct TextCache {
    context: ScaleContext,
    entries: HashMap<Key, (TextTexture, u64)>,
    frame: u64,
}

impl TextCache {
    pub fn new() -> TextCache {
        TextCache {
            context: ScaleContext::new(),
            entries: HashMap::new(),
            frame: 0,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries.retain(|_, (_, used)| frame - *used < 120);
        self.frame += 1;
    }

    pub fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        font: &Font,
        face: Face,
        text: &str,
        color: Rgba,
        scale: f64,
    ) -> Option<&TextTexture> {
        let key = (text.to_string(), face, color.map(f32::to_bits));
        if !self.entries.contains_key(&key) {
            let run = raster_run(&mut self.context, font, text, color);
            let (width, height) = (round_trip(run.width, scale), round_trip(run.height, scale));
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

pub fn round_trip(pixels: i32, scale: f64) -> i32 {
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
