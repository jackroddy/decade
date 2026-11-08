use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesPixelProgram, GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Color32F, ImportMem, Renderer};
use smithay::render_elements;
use smithay::utils::{Physical, Point, Rectangle, Size, Transform};
use swash::scale::ScaleContext;

use crate::palette::Rgba;
use crate::panes::CellRect;
use crate::text::{Font, raster_icon, raster_run};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Chrome,
    Menu,

    /// A single chrome glyph, sized to the height every icon shares and
    /// centred in one chrome cell.
    Icon,
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

    pub fn cell(&self) -> (i32, i32) {
        (self.cell_width, self.cell_height)
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

    pub fn icon(&mut self, column: u32, row: u32, icon: char, color: Rgba) {
        let (x, y) = self.px(column, row);
        self.text_px(x, y, &icon.to_string(), Face::Icon, color);
    }

    /// A box one stroke wide inside `rect`, `inset` pixels in from its
    /// left, top, right and bottom sides.
    pub fn outline(&mut self, rect: CellRect, inset: [i32; 4], color: Rgba) {
        if rect.columns < 2 || rect.rows < 2 {
            return;
        }
        let [left, top, right, bottom] = inset;
        let (x, y) = self.px(rect.column, rect.row);
        let (width, height) = self.px(rect.columns, rect.rows);
        self.box_lines(
            x + left,
            y + top,
            width - left - right,
            height - top - bottom,
            color,
        );
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

    pub fn px(&self, column: u32, row: u32) -> (i32, i32) {
        (
            column as i32 * self.cell_width,
            row as i32 * self.cell_height,
        )
    }
}

pub struct TextTexture {
    texture: GlesTexture,
    offset: Point<i32, Physical>,
    size: Size<i32, Physical>,
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
            let run = match (face, text.chars().next()) {
                (Face::Icon, Some(icon)) => raster_icon(&mut self.context, font, icon, color),
                _ => raster_run(&mut self.context, font, text, color),
            };
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

render_elements! {
    pub Element<=GlesRenderer>;
    Surface = CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Text = TextureRenderElement<GlesTexture>,
    Rounded = PixelShaderElement,
    Solid = SolidColorRenderElement,
    Ghost = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    GhostSolid = RescaleRenderElement<SolidColorRenderElement>,
    Thumbnail = CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
}

#[derive(Clone, Copy)]
pub struct Shrink {
    pub about: Point<i32, Physical>,
    pub scale: f64,
    pub alpha: f32,
}

impl Shrink {
    pub fn point(&self, point: Point<i32, Physical>) -> Point<i32, Physical> {
        let offset = (point - self.about).to_f64().upscale(self.scale);
        (self.about.to_f64() + offset).to_i32_round()
    }
}

pub struct Paint<'a> {
    pub renderer: &'a mut GlesRenderer,
    pub texts: &'a mut TextCache,
    pub font: &'a Font,
    pub menu_font: &'a Font,
    pub rounded: &'a GlesPixelProgram,
    pub scale: f64,
}

impl Paint<'_> {
    pub fn scene(
        &mut self,
        elements: &mut Vec<Element>,
        scene: &Scene,
        origin: Point<i32, Physical>,
        shrink: Option<Shrink>,
    ) {
        let alpha = shrink.map_or(1.0, |shrink| shrink.alpha);
        let premultiply = |color: Rgba| {
            let a = color[3] * alpha;
            (color[0] * a, color[1] * a, color[2] * a, a)
        };
        for shape in scene.shapes.iter().rev() {
            match shape {
                Shape::Rect { at, size, color } => {
                    let (r, g, b, a) = premultiply(*color);
                    let solid = SolidColorRenderElement::new(
                        Id::new(),
                        Rectangle::new(origin + *at, *size),
                        CommitCounter::default(),
                        Color32F::new(r, g, b, a),
                        Kind::Unspecified,
                    );
                    elements.push(match shrink {
                        Some(shrink) => Element::GhostSolid(RescaleRenderElement::from_element(
                            solid,
                            shrink.about,
                            shrink.scale,
                        )),
                        None => Element::Solid(solid),
                    });
                }
                Shape::Rounded {
                    at,
                    size,
                    radius,
                    fill,
                    border,
                } => {
                    let area = Rectangle::new(origin + *at, *size)
                        .to_f64()
                        .to_logical(self.scale)
                        .to_i32_round();
                    elements.push(Element::Rounded(PixelShaderElement::new(
                        self.rounded.clone(),
                        area,
                        None,
                        1.0,
                        vec![
                            Uniform::new("fill", premultiply(*fill)),
                            Uniform::new("border", premultiply(*border)),
                            Uniform::new("radius", *radius as f32),
                            Uniform::new("border_width", self.font.cell_width as f32 / 8.0),
                        ],
                        Kind::Unspecified,
                    )));
                }
                Shape::Text {
                    at,
                    text,
                    face,
                    color,
                } => {
                    if shrink.is_some() {
                        continue;
                    }
                    let font = match face {
                        Face::Menu => self.menu_font,
                        Face::Chrome | Face::Icon => self.font,
                    };
                    let Some(entry) =
                        self.texts
                            .get(self.renderer, font, *face, text, *color, self.scale)
                    else {
                        continue;
                    };
                    let location = origin + *at + entry.offset;
                    let texture = TextureRenderElement::from_static_texture(
                        Id::new(),
                        self.renderer.context_id(),
                        location.to_f64(),
                        entry.texture.clone(),
                        1,
                        Transform::Normal,
                        None,
                        Some(Rectangle::from_size(entry.size.to_f64().to_logical(1.0))),
                        Some(entry.size.to_f64().to_logical(self.scale).to_i32_round()),
                        None,
                        Kind::Unspecified,
                    );
                    elements.push(Element::Text(texture));
                }
            }
        }
    }
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
