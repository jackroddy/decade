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
    cache: HashMap<char, Vec<u8>>,
    context: ScaleContext,
}

impl Glyphs {
    pub fn new() -> Glyphs {
        Glyphs {
            cache: HashMap::new(),
            context: ScaleContext::new(),
        }
    }

    pub fn coverage(&mut self, font: &Font, ch: char) -> &[u8] {
        if !self.cache.contains_key(&ch) {
            let pixels = rasterize(&mut self.context, font, ch);
            self.cache.insert(ch, pixels);
        }
        &self.cache[&ch]
    }
}

fn rasterize(context: &mut ScaleContext, font: &Font, ch: char) -> Vec<u8> {
    let width = font.cell_width as i32;
    let height = font.cell_height as i32;
    let mut pixels = vec![0; (width * height) as usize];
    let font_ref = font.as_ref();
    let glyph = font_ref.charmap().map(ch);
    let mut scaler = context.builder(font_ref).size(font.px).hint(true).build();
    let Some(image) = Render::new(&[Source::Outline]).render(&mut scaler, glyph) else {
        return pixels;
    };
    let image_width = image.placement.width as i32;
    for y in 0..image.placement.height as i32 {
        let cy = font.baseline - image.placement.top + y;
        if cy < 0 || cy >= height {
            continue;
        }
        for x in 0..image_width {
            let cx = image.placement.left + x;
            if cx < 0 || cx >= width {
                continue;
            }
            pixels[(cy * width + cx) as usize] = image.data[(y * image_width + x) as usize];
        }
    }
    pixels
}
