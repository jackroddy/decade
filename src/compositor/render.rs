use std::os::unix::io::OwnedFd;
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesPixelProgram, GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, draw_render_elements};
use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
use smithay::render_elements;
use smithay::utils::{Physical, Point, Rectangle, Size, Transform};

use crate::draw::{Face, Scene, Shape, TextCache};
use crate::frame::{self, fit};
use crate::palette::{self, ACCENT, Rgba};
use crate::panes::{BACKGROUND, Chrome};
use crate::picker::{Grid, Picker};
use crate::text::Font;
use crate::workspace::Workspace;

use super::input::Drag;
use super::{State, send_frames_surface_tree};

const CLOSE_ICON: char = '\u{f0156}';
const SCRIM: Rgba = [0.0, 0.0, 0.0, 0.35];

impl State {
    /// Draw the workspace into `target`, and return the fence to wait on and
    /// whether the frame is part of an animation that wants the next one.
    pub(super) fn render(
        &mut self,
        target: &mut Dmabuf,
        workspace: usize,
    ) -> Result<(Option<OwnedFd>, bool), String> {
        let current = self.current();
        let State {
            gpu,
            size,
            workspaces,
            font,
            menu_font,
            texts,
            start,
            scale,
            drag,
            bar,
            hover,
            pointer,
            menu,
            panel,
            picker,
            settings,
            rounded,
            sidebar_open,
            ..
        } = self;
        let layout = workspaces[workspace].layout();
        let scratch = workspaces[workspace].scratch_pane();
        let (menu, panel, drag, picker) = if workspace == current {
            (
                menu.as_ref(),
                panel.as_ref(),
                drag.as_ref(),
                picker.as_ref(),
            )
        } else {
            (None, None, None, None)
        };
        let chrome = if workspace == current {
            Chrome {
                scratch,
                dragging: match drag {
                    Some(Drag::Pane { id, .. }) => Some(*id),
                    _ => None,
                },
                bar: *bar,
                hover: *hover,
            }
        } else {
            Chrome {
                scratch,
                ..Chrome::default()
            }
        };
        let scale = *scale;
        let size = *size;
        let damage = Rectangle::from_size(size);
        let mut framebuffer = gpu
            .renderer
            .bind(target)
            .map_err(|error| error.to_string())?;
        let mut paint = Paint {
            renderer: &mut gpu.renderer,
            texts,
            font,
            menu_font,
            rounded,
            scale,
        };
        let mut elements: Vec<Element> = Vec::new();

        let overlay_origin = |box_size: Size<i32, Physical>| -> Point<i32, Physical> {
            let top = (size.h / 7).min((size.h - box_size.h).max(0));
            ((size.w - box_size.w) / 2, top).into()
        };
        if let Some(menu) = menu {
            let mut scene = Scene::new(paint.menu_font);
            let box_size = menu.draw(&mut scene, paint.menu_font);
            paint.scene(&mut elements, &scene, overlay_origin(box_size), None);
        }
        if let Some(panel) = panel {
            let mut scene = Scene::new(paint.menu_font);
            let box_size = panel.draw(&mut scene, paint.menu_font, settings);
            paint.scene(&mut elements, &scene, overlay_origin(box_size), None);
        }

        if let Some(picker) = picker {
            let pointer = pointer.to_physical(scale).to_i32_round();
            paint_picker(
                &mut paint,
                &mut elements,
                picker,
                (workspaces, current),
                size,
                pointer,
            );
        }
        if menu.is_some() || panel.is_some() || picker.is_some() {
            let mut scrim = Scene::new(font);
            scrim.rect(0, 0, size.w, size.h, SCRIM);
            paint.scene(&mut elements, &scrim, Point::default(), None);
        }

        let mut animating = false;
        let dragged = match drag {
            Some(Drag::Pane {
                id, grab, start, ..
            }) => layout.pane(*id).map(|pane| (pane, *grab, *start)),
            _ => None,
        };
        if let Some((pane, grab, start)) = dragged {
            let animation = Duration::from_millis(settings.grab_ms).as_secs_f64();
            let t = if animation > 0.0 {
                (start.elapsed().as_secs_f64() / animation).min(1.0)
            } else {
                1.0
            };
            animating = t < 1.0;
            let eased = 1.0 - (1.0 - t).powi(3);
            let shrink = Shrink {
                about: pointer.to_physical(scale).to_i32_round(),
                scale: 1.0 + (settings.ghost_scale - 1.0) * eased,
                alpha: 1.0 + (settings.ghost_opacity - 1.0) * eased as f32,
            };
            let origin = shrink.about - grab;
            let cell: Point<i32, Physical> =
                (font.cell_width as i32, font.cell_height as i32).into();
            let outer: Point<i32, Physical> = (
                (pane.rect.columns * font.cell_width) as i32,
                (pane.rect.rows * font.cell_height) as i32,
            )
                .into();
            let start = shrink.point(origin + cell.downscale(2));
            let end = shrink.point(origin + outer - cell.downscale(2));
            let mut outline = Scene::new(font);
            let line = outline.line_width();
            outline.box_lines(
                start.x,
                start.y,
                end.x - start.x + line,
                end.y - start.y + line,
                ACCENT,
            );
            paint.scene(&mut elements, &outline, Point::default(), None);
            let mut ghost = Scene::new(font);
            layout.draw_ghost(pane.id, &mut ghost, chrome);
            paint.scene(&mut elements, &ghost, origin + cell, Some(shrink));
            if let Some(toplevel) = pane.toplevel() {
                let client = layout.client_rect(pane);
                let offset: Point<i32, Physical> = (
                    ((client.column - pane.rect.column) * font.cell_width) as i32,
                    ((client.row - pane.rect.row) * font.cell_height) as i32,
                )
                    .into();
                let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(
                        paint.renderer,
                        toplevel.wl_surface(),
                        origin + offset,
                        scale,
                        shrink.alpha,
                        Kind::Unspecified,
                    );
                elements.extend(surfaces.into_iter().map(|surface| {
                    Element::Ghost(RescaleRenderElement::from_element(
                        surface,
                        shrink.about,
                        shrink.scale,
                    ))
                }));
            }
        }

        let mut above = Scene::new(font);
        layout.draw_bars(&mut above, chrome);
        if let Some(Drag::Pane {
            id,
            cell: Some(cell),
            ..
        }) = drag
        {
            layout.draw_drop(&mut above, *id, *cell);
        }
        paint.scene(&mut elements, &above, Point::default(), None);

        for pane in layout.visible().filter(|_| picker.is_none()) {
            let Some(toplevel) = pane.toplevel() else {
                continue;
            };
            if chrome.dragging == Some(pane.id) {
                continue;
            }
            let rect = layout.client_rect(pane);
            if rect.columns == 0 || rect.rows == 0 {
                continue;
            }
            let origin = (
                (rect.column * font.cell_width) as i32,
                (rect.row * font.cell_height) as i32,
            );
            let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    paint.renderer,
                    toplevel.wl_surface(),
                    origin,
                    scale,
                    1.0,
                    Kind::Unspecified,
                );
            let size = (
                (rect.columns * font.cell_width) as i32,
                (rect.rows * font.cell_height) as i32,
            );
            let crop = Rectangle::new(origin.into(), size.into());
            elements.extend(
                surfaces
                    .into_iter()
                    .filter_map(|surface| CropRenderElement::from_element(surface, scale, crop))
                    .map(Element::Surface),
            );
        }

        if picker.is_none() {
            let mut below = Scene::new(font);
            layout.draw(&mut below, chrome);
            let grid = (
                size.w as u32 / font.cell_width,
                size.h as u32 / font.cell_height,
            );
            build_frame(workspaces, workspace, grid.0, grid.1, *sidebar_open).draw(&mut below);
            paint.scene(&mut elements, &below, Point::default(), None);
        }

        let mut frame = paint
            .renderer
            .render(&mut framebuffer, size, Transform::Normal)
            .map_err(|error| error.to_string())?;
        frame
            .clear(
                Color32F::new(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 1.0),
                &[damage],
            )
            .map_err(|error| error.to_string())?;
        draw_render_elements::<GlesRenderer, _, _>(&mut frame, scale, &elements, &[damage])
            .map_err(|error| error.to_string())?;
        let sync = frame.finish().map_err(|error| error.to_string())?;
        drop(framebuffer);
        texts.end_frame();

        let time = start.elapsed().as_millis() as u32;
        for pane in layout.visible() {
            if let Some(toplevel) = pane.toplevel() {
                send_frames_surface_tree(toplevel.wl_surface(), time);
            }
        }
        Ok((sync.export(), animating))
    }
}

render_elements! {
    Element<=GlesRenderer>;
    Surface = CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Text = TextureRenderElement<GlesTexture>,
    Rounded = PixelShaderElement,
    Solid = SolidColorRenderElement,
    Ghost = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    GhostSolid = RescaleRenderElement<SolidColorRenderElement>,
    Thumbnail = CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
}

pub(super) fn build_frame(
    workspaces: &[Workspace],
    shown: usize,
    columns: u32,
    rows: u32,
    open: bool,
) -> frame::Frame {
    let entries = workspaces
        .iter()
        .enumerate()
        .map(|(i, workspace)| {
            let urgent = workspace.agents.values().max_by_key(|agent| agent.state);
            frame::Entry {
                id: workspace.id,
                name: workspace.name.clone(),
                state: workspace.agent_state(),
                message: urgent.and_then(|agent| agent.message.clone()),
                current: i == shown,
            }
        })
        .collect();
    let shown = &workspaces[shown];
    let layout = shown.layout();
    let pane = layout
        .pane(layout.focused)
        .map(|pane| pane.name.clone())
        .unwrap_or_default();
    frame::Frame::new(
        columns,
        rows,
        open,
        entries,
        (shown.tabs.len(), shown.active_tab),
        pane,
        layout.zoom.is_some(),
    )
}

fn paint_picker(
    paint: &mut Paint,
    elements: &mut Vec<Element>,
    picker: &Picker,
    (workspaces, current): (&[Workspace], usize),
    size: Size<i32, Physical>,
    pointer: Point<i32, Physical>,
) {
    let (cell_width, cell_height) = (paint.font.cell_width, paint.font.cell_height);
    let (menu_width, menu_height) = (
        paint.menu_font.cell_width as i32,
        paint.menu_font.cell_height as i32,
    );
    let grid = Grid::new(workspaces.len() + 1, size, cell_height as i32);
    let mut over = Scene::new(paint.font);
    let mut under = Scene::new(paint.font);
    let line = over.line_width();
    let label_width = |text: &str| text.chars().count() as i32 * menu_width;
    let drop_target = picker.dragging.and_then(|_| grid.cell_at(pointer));
    let mut surfaces = Vec::new();
    for (i, cell) in grid.cells.iter().enumerate() {
        let (outline, stroke) = if drop_target == Some(i) {
            (palette::CYAN, line)
        } else if picker.selected == i {
            (ACCENT, line * 2)
        } else {
            (palette::MUTED, line)
        };
        for ring in 1..=stroke / line {
            over.box_lines(
                cell.loc.x - line * ring,
                cell.loc.y - line * ring,
                cell.size.w + line * ring * 2,
                cell.size.h + line * ring * 2,
                outline,
            );
        }
        under.rect(
            cell.loc.x,
            cell.loc.y,
            cell.size.w,
            cell.size.h,
            palette::DARK_BACKGROUND,
        );
        let room = (cell.size.w / menu_width).max(1) as usize;
        let label_y = cell.loc.y + cell.size.h + (grid.label - menu_height) / 2;
        let Some(workspace) = workspaces.get(i) else {
            let text = fit("+ New workspace", room);
            let x = cell.loc.x + (cell.size.w - label_width(&text)) / 2;
            let y = cell.loc.y + (cell.size.h - menu_height) / 2;
            over.text_px(x, y, &text, Face::Menu, palette::FOREGROUND);
            continue;
        };
        let confirming = picker.confirming == Some(workspace.id);
        let label = if confirming {
            "close? x again".to_string()
        } else if workspace.closing() {
            format!("{} closing", workspace.name)
        } else if i == current {
            format!("● {}", workspace.name)
        } else {
            workspace.name.clone()
        };
        let label = fit(&label, room);
        let label_color = if confirming {
            palette::RED
        } else {
            palette::FOREGROUND
        };
        let x = cell.loc.x + (cell.size.w - label_width(&label)) / 2;
        over.text_px(x, label_y, &label, Face::Menu, label_color);
        let button = grid.close_button(i, cell_height as i32);
        let icon_color = if confirming {
            palette::RED
        } else {
            palette::LIGHT_FOREGROUND
        };
        let icon_x = button.loc.x + (button.size.w - menu_width) / 2;
        let icon_y = button.loc.y + (button.size.h - menu_height) / 2;
        over.text_px(
            icon_x,
            icon_y,
            &CLOSE_ICON.to_string(),
            Face::Menu,
            icon_color,
        );

        let layout = workspace.layout();

        // the cell shows the pane area, not the top bar and sidebar, so
        // positions are taken from the area's corner and scaled to fit it
        let area = layout.area;
        let corner = Point::from((
            (area.column * cell_width) as i32,
            (area.row * cell_height) as i32,
        ));
        let (width, height) = (area.columns * cell_width, area.rows * cell_height);
        let scale = (cell.size.w as f64 / width.max(1) as f64)
            .min(cell.size.h as f64 / height.max(1) as f64);
        let anchor = cell.loc
            + Point::from((
                (cell.size.w - (width as f64 * scale) as i32) / 2,
                (cell.size.h - (height as f64 * scale) as i32) / 2,
            ));
        for pane in layout.visible() {
            let Some(toplevel) = pane.toplevel() else {
                continue;
            };
            let client = layout.client_rect(pane);
            let origin = anchor - corner
                + Point::from((
                    (client.column * cell_width) as i32,
                    (client.row * cell_height) as i32,
                ));
            let tree: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    paint.renderer,
                    toplevel.wl_surface(),
                    origin,
                    paint.scale,
                    1.0,
                    Kind::Unspecified,
                );
            surfaces.extend(tree.into_iter().filter_map(|surface| {
                let scaled = RescaleRenderElement::from_element(surface, anchor, scale);
                CropRenderElement::from_element(scaled, paint.scale, *cell).map(Element::Thumbnail)
            }));
        }
    }
    let hint = fit(
        "tab move · enter show · esc close · x close",
        (size.w / menu_width).max(1) as usize,
    );
    let hint_y = size.h - (cell_height as i32 * 2 + menu_height) / 2;
    let hint_x = (size.w - label_width(&hint)) / 2;
    over.text_px(hint_x, hint_y, &hint, Face::Menu, palette::LIGHT_FOREGROUND);
    paint.scene(elements, &over, Point::default(), None);
    elements.extend(surfaces);
    paint.scene(elements, &under, Point::default(), None);
}

#[derive(Clone, Copy)]
struct Shrink {
    about: Point<i32, Physical>,
    scale: f64,
    alpha: f32,
}

impl Shrink {
    fn point(&self, point: Point<i32, Physical>) -> Point<i32, Physical> {
        let offset = (point - self.about).to_f64().upscale(self.scale);
        (self.about.to_f64() + offset).to_i32_round()
    }
}

struct Paint<'a> {
    renderer: &'a mut GlesRenderer,
    texts: &'a mut TextCache,
    font: &'a Font,
    menu_font: &'a Font,
    rounded: &'a GlesPixelProgram,
    scale: f64,
}

impl Paint<'_> {
    fn scene(
        &mut self,
        elements: &mut Vec<Element>,
        scene: &Scene,
        origin: Point<i32, Physical>,
        shrink: Option<Shrink>,
    ) {
        let alpha = shrink.map_or(1.0, |shrink| shrink.alpha);
        for shape in scene.shapes.iter().rev() {
            match shape {
                Shape::Rect { at, size, color } => {
                    let a = color[3] * alpha;
                    let solid = SolidColorRenderElement::new(
                        Id::new(),
                        Rectangle::new(origin + *at, *size),
                        CommitCounter::default(),
                        Color32F::new(color[0] * a, color[1] * a, color[2] * a, a),
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
                    let premultiply = |c: [f32; 4]| {
                        let a = c[3] * alpha;
                        (c[0] * a, c[1] * a, c[2] * a, a)
                    };
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
