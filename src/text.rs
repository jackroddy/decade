use std::collections::HashMap;

use swash::scale::{Render, ScaleContext, Source};
use swash::{CacheKey, FontRef};

pub const FONT_FAMILY: &str = "JetBrainsMono Nerd Font";
pub const FONT_PX: f32 = 14.0;

pub struct Font {
    data: Vec<u8>,
    offset: u32,
    key: CacheKey,
    px: f32,
    pub cell_width: u32,
    pub cell_height: u32,
    baseline: i32,
}

impl Font {
    pub fn load(px: f32) -> Font {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(FONT_FAMILY), fontdb::Family::Monospace],
            ..Default::default()
        };
        let id = db.query(&query).expect("no monospace font installed");
        let face = db.face(id).unwrap();
        let data = match &face.source {
            fontdb::Source::File(path) => std::fs::read(path).unwrap(),
            _ => panic!("font {} is not a file on disk", FONT_FAMILY),
        };
        let font = FontRef::from_index(&data, face.index as usize).unwrap();
        let metrics = font.metrics(&[]).scale(px);
        let advance = font
            .glyph_metrics(&[])
            .scale(px)
            .advance_width(font.charmap().map('M'));
        let (offset, key) = (font.offset, font.key);
        Font {
            cell_width: advance.round().max(1.0) as u32,
            cell_height: (metrics.ascent + metrics.descent + metrics.leading)
                .round()
                .max(1.0) as u32,
            baseline: metrics.ascent.round() as i32,
            data,
            offset,
            key,
            px,
        }
    }

    fn as_ref(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }
}

pub struct Glyphs {
    cache: HashMap<(char, bool), Vec<u8>>,
    context: ScaleContext,
}

impl Glyphs {
    pub fn new() -> Glyphs {
        Glyphs {
            cache: HashMap::new(),
            context: ScaleContext::new(),
        }
    }

    pub fn coverage(&mut self, font: &Font, ch: char, right_half: bool) -> &[u8] {
        let key = (ch, right_half);
        if !self.cache.contains_key(&key) {
            let pixels = rasterize(&mut self.context, font, ch, right_half);
            self.cache.insert(key, pixels);
        }
        &self.cache[&key]
    }
}

const EDGE_BASE: u32 = 0x10FF00;
const EDGE_LEFT: u32 = 1;
const EDGE_RIGHT: u32 = 2;
const EDGE_TOP: u32 = 4;
const EDGE_BOTTOM: u32 = 8;

pub fn edge_char(left: bool, right: bool, top: bool, bottom: bool) -> char {
    let bits = [
        (left, EDGE_LEFT),
        (right, EDGE_RIGHT),
        (top, EDGE_TOP),
        (bottom, EDGE_BOTTOM),
    ]
    .into_iter()
    .filter(|&(on, _)| on)
    .fold(0, |bits, (_, bit)| bits | bit);
    char::from_u32(EDGE_BASE + bits).unwrap_or(' ')
}

fn rasterize(context: &mut ScaleContext, font: &Font, ch: char, right_half: bool) -> Vec<u8> {
    let width = font.cell_width as i32;
    let height = font.cell_height as i32;
    let mut pixels = vec![0; (width * height) as usize];
    if let Some(bits) = (ch as u32).checked_sub(EDGE_BASE).filter(|&bits| bits < 16) {
        let thickness = (width / 8).max(1);
        for y in 0..height {
            for x in 0..width {
                let on = (bits & EDGE_LEFT != 0 && x < thickness)
                    || (bits & EDGE_RIGHT != 0 && x >= width - thickness)
                    || (bits & EDGE_TOP != 0 && y < thickness)
                    || (bits & EDGE_BOTTOM != 0 && y >= height - thickness);
                if on {
                    pixels[(y * width + x) as usize] = 255;
                }
            }
        }
        return pixels;
    }
    let font_ref = font.as_ref();
    let glyph = font_ref.charmap().map(ch);
    let mut scaler = context.builder(font_ref).size(font.px).hint(true).build();
    let Some(image) = Render::new(&[Source::Outline]).render(&mut scaler, glyph) else {
        return pixels;
    };
    let image_width = image.placement.width as i32;
    let shift = if right_half { width } else { 0 };
    for y in 0..image.placement.height as i32 {
        let cy = font.baseline - image.placement.top + y;
        if cy < 0 || cy >= height {
            continue;
        }
        for x in 0..image_width {
            let cx = image.placement.left + x - shift;
            if cx < 0 || cx >= width {
                continue;
            }
            pixels[(cy * width + cx) as usize] = image.data[(y * image_width + x) as usize];
        }
    }
    pixels
}
