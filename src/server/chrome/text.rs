use std::collections::HashMap;
use std::rc::Rc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Physical, Point, Rectangle, Size};
use swash::scale::{Render, ScaleContext, Source};
use swash::{CacheKey, FontRef};

use crate::server::chrome::draw::Face;
use crate::server::chrome::palette::Rgba;
use crate::server::chrome::settings::Settings;
use crate::server::panes::CellRect;

const FONT_FAMILY: &str = "JetBrainsMono Nerd Font";

/// `text` cut to `max` characters, with an ellipsis where it was cut.
pub fn fit(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// `text` cut to its last `max` characters, with an ellipsis where it was cut.
pub fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count + 1 - max).collect();
    format!("…{kept}")
}

/// One face of the font file, in the form swash reads.
struct FaceFile {
    data: Rc<[u8]>,
    offset: u32,
    key: CacheKey,
}

impl FaceFile {
    fn new((data, index): (Rc<[u8]>, usize)) -> FaceFile {
        let font = FontRef::from_index(&data, index).unwrap();
        let (offset, key) = (font.offset, font.key);
        FaceFile { data, offset, key }
    }

    fn as_ref(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }
}

/// The regular and bold faces at one size, laid out on the regular
/// face's cells.
pub struct Font {
    regular: FaceFile,
    bold: FaceFile,
    pub px: f32,
    pub cell_width: u32,
    pub cell_height: u32,
    baseline: i32,
}

impl Font {
    /// The pixels the cells cover.
    pub fn px_rect(&self, rect: CellRect) -> Rectangle<i32, Physical> {
        Rectangle::new(
            (
                (rect.column * self.cell_width) as i32,
                (rect.row * self.cell_height) as i32,
            )
                .into(),
            (
                (rect.columns * self.cell_width) as i32,
                (rect.rows * self.cell_height) as i32,
            )
                .into(),
        )
    }

    #[cfg(test)]
    pub fn load(px: f32) -> Font {
        let [regular, bold] = font_files();
        Font::new(regular, bold, px)
    }

    fn new(regular: (Rc<[u8]>, usize), bold: (Rc<[u8]>, usize), px: f32) -> Font {
        let regular = FaceFile::new(regular);
        let font = regular.as_ref();
        let metrics = font.metrics(&[]).scale(px);
        let advance = font
            .glyph_metrics(&[])
            .scale(px)
            .advance_width(font.charmap().map('M'));
        Font {
            cell_width: advance.round().max(1.0) as u32,
            cell_height: (metrics.ascent + metrics.descent + metrics.leading)
                .round()
                .max(1.0) as u32,
            baseline: metrics.ascent.round() as i32,
            regular,
            bold: FaceFile::new(bold),
            px,
        }
    }
}

/// The bytes of the font files holding the regular and bold faces, and
/// the index of each face in its file.
fn font_files() -> [(Rc<[u8]>, usize); 2] {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    [fontdb::Weight::NORMAL, fontdb::Weight::BOLD].map(|weight| {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(FONT_FAMILY), fontdb::Family::Monospace],
            weight,
            ..Default::default()
        };
        let id = db.query(&query).expect("no monospace font installed");
        let face = db.face(id).unwrap();
        let data = match &face.source {
            fontdb::Source::File(path) => std::fs::read(path).unwrap(),
            _ => panic!("font {} is not a file on disk", FONT_FAMILY),
        };
        (data.into(), face.index as usize)
    })
}

/// The chrome and palette fonts at one scale, and the textures of the
/// text drawn with them.
pub struct Text {
    pub chrome: Font,
    pub menu: Font,
    scale: f64,
    context: ScaleContext,
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
        let [regular, bold] = font_files();
        let font = |px: f32| Font::new(regular.clone(), bold.clone(), px * scale as f32);
        Text {
            chrome: font(settings.font_size),
            menu: font(settings.palette_size),
            scale,
            context: ScaleContext::new(),
            entries: HashMap::new(),
            frame: 0,
        }
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
            let context = &mut self.context;
            let run = match (face, text.chars().next()) {
                (Face::Icon, Some(icon)) => raster_icon(context, &self.chrome, icon, color),
                (Face::Chrome | Face::Icon, _) => {
                    raster_run(context, &self.chrome, false, text, color)
                }
                (Face::ChromeBold, _) => raster_run(context, &self.chrome, true, text, color),
                (Face::Menu, _) => raster_run(context, &self.menu, false, text, color),
            };
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

pub struct Run {
    pub pixels: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
}

fn raster_run(context: &mut ScaleContext, font: &Font, bold: bool, text: &str, color: Rgba) -> Run {
    let cell_width = font.cell_width as i32;
    let font_ref = if bold { &font.bold } else { &font.regular }.as_ref();
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
fn raster_icon(context: &mut ScaleContext, font: &Font, icon: char, color: Rgba) -> Run {
    let font_ref = font.regular.as_ref();
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
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(fit("website-redesign", 8), "website…");
        assert_eq!(fit("nail", 8), "nail");
    }

    #[test]
    fn long_paths_keep_their_end_after_an_ellipsis() {
        assert_eq!(tail("~/projects/decade/", 10), "…s/decade/");
        assert_eq!(tail("~/", 10), "~/");
    }

    #[test]
    fn a_padded_size_survives_the_trip_through_logical_pixels() {
        for pixels in 1..200 {
            let padded = round_trip(pixels, 1.25);
            assert!(padded >= pixels);
            let logical = (padded as f64 / 1.25).round();
            assert_eq!((logical * 1.25).round() as i32, padded);
        }
    }

    #[test]
    fn a_run_covers_its_cells_and_any_glyph_that_overflows_them() {
        let font = Font::load(14.0);
        let mut context = ScaleContext::new();
        let run = raster_run(&mut context, &font, false, "ab", [1.0; 4]);
        assert!(run.width >= 2 * font.cell_width as i32);
        assert!(run.height >= font.cell_height as i32);
        assert_eq!(run.pixels.len(), (run.width * run.height * 4) as usize);
        assert!(run.pixels.iter().any(|&byte| byte > 0));
        let icon = raster_run(&mut context, &font, false, "\u{f0156}", [1.0; 4]);
        assert!(icon.width > font.cell_width as i32);
    }

    #[test]
    fn a_bold_run_has_more_ink_than_a_regular_one_on_the_same_cells() {
        let font = Font::load(14.0);
        let mut context = ScaleContext::new();
        let regular = raster_run(&mut context, &font, false, "decade", [1.0; 4]);
        let bold = raster_run(&mut context, &font, true, "decade", [1.0; 4]);
        let ink = |run: &Run| run.pixels.iter().map(|&byte| byte as u32).sum::<u32>();
        assert!(ink(&bold) > ink(&regular));
        assert_eq!(bold.width, regular.width);
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
