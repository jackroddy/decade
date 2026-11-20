use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesPixelProgram, GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Color32F, Renderer};
use smithay::render_elements;
use smithay::utils::{Physical, Point, Rectangle, Size, Transform};

use decade_ui::palette::Rgba;
use decade_ui::{Scene, Shape};

use crate::server::chrome::text::Text;

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
    pub text: &'a mut Text,
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
                        Rectangle::new(origin + px_point(*at), px_size(*size)),
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
                    border_width,
                } => {
                    let area = Rectangle::new(origin + px_point(*at), px_size(*size))
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
                            Uniform::new("border_width", *border_width as f32),
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
                    let Some(entry) = self.text.get(self.renderer, *face, text, *color) else {
                        continue;
                    };
                    let location = origin + px_point(*at) + entry.offset;
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

                // the compositor paints a pane's client at its content
                // shape before it paints the scene
                Shape::Content { .. } => {}
            }
        }
    }
}

pub fn px_point(at: decade_ui::Point) -> Point<i32, Physical> {
    (at.x, at.y).into()
}

pub fn px_size(size: decade_ui::Size) -> Size<i32, Physical> {
    (size.width, size.height).into()
}
