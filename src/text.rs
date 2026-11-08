use swash::scale::{Render, ScaleContext, Source};
use swash::{CacheKey, FontRef};

pub const FONT_FAMILY: &str = "JetBrainsMono Nerd Font";

pub struct Font {
    data: Vec<u8>,
    offset: u32,
    key: CacheKey,
    pub px: f32,
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

pub type Rgba = [f32; 4];

pub struct Run {
    pub pixels: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
}

pub fn raster_run(context: &mut ScaleContext, font: &Font, text: &str, color: Rgba) -> Run {
    let cell_width = font.cell_width as i32;
    let font_ref = font.as_ref();
    let mut scaler = context.builder(font_ref).size(font.px).hint(true).build();
    let mut images = Vec::new();
    let (mut left, mut top) = (0, 0);
    let mut right = text.chars().count() as i32 * cell_width;
    let mut bottom = font.cell_height as i32;
    for (i, ch) in text.chars().enumerate() {
        let glyph = font_ref.charmap().map(ch);
        let Some(image) = Render::new(&[Source::Outline]).render(&mut scaler, glyph) else {
            continue;
        };
        let x = i as i32 * cell_width + image.placement.left;
        let y = font.baseline - image.placement.top;
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + image.placement.width as i32);
        bottom = bottom.max(y + image.placement.height as i32);
        images.push((x, y, image));
    }
    let (width, height) = (right - left, bottom - top);
    let mut coverage = vec![0u8; (width * height) as usize];
    for (x, y, image) in &images {
        let image_width = image.placement.width as i32;
        for row in 0..image.placement.height as i32 {
            for column in 0..image_width {
                let value = image.data[(row * image_width + column) as usize];
                let at = ((y - top + row) * width + (x - left + column)) as usize;
                coverage[at] = coverage[at].max(value);
            }
        }
    }
    Run {
        pixels: tint(&coverage, color),
        width,
        height,
        left,
        top,
    }
}

/// The ink height every icon is drawn at, as a fraction of the font size.
//
// the height of the close and fullscreen glyphs in the Material Design
// set; the pin glyphs are taller and are drawn at this height too
const ICON_HEIGHT: f32 = 0.68;

/// One glyph rasterized so its ink is no taller than `ICON_HEIGHT` and
/// centred in a cell.
pub fn raster_icon(context: &mut ScaleContext, font: &Font, icon: char, color: Rgba) -> Run {
    let font_ref = font.as_ref();
    let glyph = font_ref.charmap().map(icon);
    let render = |context: &mut ScaleContext, px: f32| {
        let mut scaler = context.builder(font_ref).size(px).hint(true).build();
        Render::new(&[Source::Outline]).render(&mut scaler, glyph)
    };
    let Some(mut image) = render(context, font.px) else {
        return Run {
            pixels: Vec::new(),
            width: 0,
            height: 0,
            left: 0,
            top: 0,
        };
    };
    let most = font.px * ICON_HEIGHT;
    if image.placement.height as f32 > most + 0.5 {
        let px = font.px * most / image.placement.height as f32;
        if let Some(smaller) = render(context, px) {
            image = smaller;
        }
    }
    let (width, height) = (image.placement.width as i32, image.placement.height as i32);
    Run {
        pixels: tint(&image.data, color),
        width,
        height,
        left: (font.cell_width as i32 - width) / 2,
        top: (font.cell_height as i32 - height) / 2,
    }
}

fn tint(coverage: &[u8], color: Rgba) -> Vec<u8> {
    coverage
        .iter()
        .flat_map(|&value| {
            let alpha = value as f32 / 255.0 * color[3];
            let channel = |c: f32| (c * alpha * 255.0).round() as u8;
            [
                channel(color[0]),
                channel(color[1]),
                channel(color[2]),
                (alpha * 255.0).round() as u8,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_covers_its_cells_and_any_glyph_that_overflows_them() {
        let font = Font::load(14.0);
        let mut context = ScaleContext::new();
        let run = raster_run(&mut context, &font, "ab", [1.0; 4]);
        assert!(run.width >= 2 * font.cell_width as i32);
        assert!(run.height >= font.cell_height as i32);
        assert_eq!(run.pixels.len(), (run.width * run.height * 4) as usize);
        assert!(run.pixels.iter().any(|&byte| byte > 0));
        let icon = raster_run(&mut context, &font, "\u{f0156}", [1.0; 4]);
        assert!(icon.width > font.cell_width as i32);
    }

    #[test]
    fn every_icon_is_drawn_no_taller_than_the_shared_icon_height() {
        let font = Font::load(28.0);
        let mut context = ScaleContext::new();
        let heights: Vec<i32> = [
            '\u{f0403}',
            '\u{f0931}',
            '\u{f0293}',
            '\u{f0156}',
            '\u{f0093}',
            '\u{f0096}',
        ]
        .into_iter()
        .map(|icon| raster_icon(&mut context, &font, icon, [1.0; 4]).height)
        .collect();
        let most = (28.0 * ICON_HEIGHT).round() as i32 + 1;
        assert!(heights.iter().all(|&height| height > 0 && height <= most));
    }
}
