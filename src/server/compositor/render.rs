use std::os::unix::io::OwnedFd;
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
use smithay::utils::{Physical, Point, Rectangle, Size, Transform};

use crate::server::chrome::draw::{Element, Paint, Scene, Shrink};
use crate::server::chrome::frame;
use crate::server::chrome::palette::{ACCENT, Rgba};
use crate::server::chrome::picker::{Grid, Picker};
use crate::server::chrome::settings::Settings;
use crate::server::panes::{BACKGROUND, Chrome, Layout};
use crate::server::workspace::{self, Workspace};

use super::input::Drag;
use super::{Overlay, State, send_frames_surface_tree};

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
        let picking = matches!(self.overlay, Some(Overlay::Picker { .. }));
        let picker_grid = (workspace == current && picking).then(|| self.picker_grid());
        if picker_grid.is_none() && self.sidebar_open {
            let now = Instant::now();
            for shown in &mut self.workspaces {
                shown.refresh_branch(now);
            }
        }
        let frame = picker_grid.is_none().then(|| self.frame(workspace));
        let State {
            gpu,
            size,
            workspaces,
            text,
            start,
            scale,
            drag,
            bar,
            hover,
            pointer,
            overlay,
            settings,
            rounded,
            ..
        } = self;
        let layout = workspaces[workspace].layout();
        let scratch = workspaces[workspace].scratch_pane();
        let (overlay, drag) = if workspace == current {
            (overlay.as_ref(), drag.as_ref())
        } else {
            (None, None)
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
        let pointer = pointer.to_physical(scale).to_i32_round();
        let damage = Rectangle::from_size(size);
        let mut framebuffer = gpu
            .renderer
            .bind(target)
            .map_err(|error| error.to_string())?;
        let mut paint = Paint {
            renderer: &mut gpu.renderer,
            text,
            rounded,
            scale,
        };
        let mut elements: Vec<Element> = Vec::new();

        let overlay_origin = |box_size: Size<i32, Physical>| -> Point<i32, Physical> {
            let top = (size.h / 7).min((size.h - box_size.h).max(0));
            ((size.w - box_size.w) / 2, top).into()
        };
        match overlay {
            Some(Overlay::Menu(menu)) => {
                let mut scene = Scene::new(&paint.text.menu);
                let box_size = menu.draw(&mut scene);
                paint.scene(&mut elements, &scene, overlay_origin(box_size), None);
            }
            Some(Overlay::Panel(panel)) => {
                let mut scene = Scene::new(&paint.text.menu);
                let box_size = panel.draw(&mut scene, settings);
                paint.scene(&mut elements, &scene, overlay_origin(box_size), None);
            }
            Some(Overlay::Picker { picker, .. }) => {
                if let Some(grid) = &picker_grid {
                    paint_picker(
                        &mut paint,
                        &mut elements,
                        (picker, grid),
                        (workspaces, current),
                        pointer,
                    );
                }
            }
            None => {}
        }
        if overlay.is_some() {
            let mut scrim = Scene::new(&paint.text.chrome);
            scrim.rect(0, 0, size.w, size.h, SCRIM);
            paint.scene(&mut elements, &scrim, Point::default(), None);
        }

        let mut animating = false;
        if let Some(Drag::Pane {
            id, grab, start, ..
        }) = drag
            && layout.pane(*id).is_some()
        {
            let shrink;
            (shrink, animating) = ghost_shrink(settings, *start, pointer);
            paint_ghost(&mut paint, &mut elements, layout, chrome, shrink, *grab);
        }

        let mut above = Scene::new(&paint.text.chrome);
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

        if let Some(frame) = frame {
            paint_clients(&mut paint, &mut elements, layout, chrome);
            let mut below = Scene::new(&paint.text.chrome);
            layout.draw(&mut below, chrome);
            frame.draw(&mut below);
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
        paint.text.end_frame();

        let time = start.elapsed().as_millis() as u32;
        for pane in layout.visible() {
            if let Some(toplevel) = pane.toplevel() {
                send_frames_surface_tree(toplevel.wl_surface(), time);
            }
        }
        Ok((sync.export(), animating))
    }

    /// The top bar and sidebar drawn around the workspace's panes.
    pub(super) fn frame(&self, shown: usize) -> frame::Frame {
        let home = workspace::home();
        let entries = (self.workspaces.iter().enumerate())
            .map(|(i, workspace)| {
                let urgent = (workspace.router.clients.values())
                    .filter_map(|client| Some((client.status.as_ref()?, client.since)))
                    .max_by_key(|(status, _)| status.state);
                frame::Entry {
                    id: workspace.id,
                    name: workspace.name.clone(),
                    tabs: workspace.tabs.len(),
                    path: frame::short_path(&workspace.cwd, &home),
                    branch: workspace.branch.clone(),
                    state: urgent.map(|(status, _)| status.state),
                    age: urgent.map_or(Duration::ZERO, |(_, since)| since.elapsed()),
                    message: urgent.and_then(|(status, _)| status.message.clone()),
                    current: i == shown,
                }
            })
            .collect();
        let (columns, rows) = self.grid();
        let shown = &self.workspaces[shown];
        let layout = shown.layout();
        let pane = layout
            .pane(layout.focused)
            .map(|pane| pane.name.clone())
            .unwrap_or_default();
        frame::Frame::new(
            columns,
            rows,
            self.sidebar_open,
            entries,
            (shown.tabs.len(), shown.active_tab),
            pane,
            layout.zoom.is_some(),
        )
    }
}

/// How far the ghost of a pane grabbed at `start` has shrunk about the
/// pointer, and whether it is still shrinking.
fn ghost_shrink(
    settings: &Settings,
    start: Instant,
    about: Point<i32, Physical>,
) -> (Shrink, bool) {
    let animation = Duration::from_millis(settings.grab_ms).as_secs_f64();
    let t = if animation > 0.0 {
        (start.elapsed().as_secs_f64() / animation).min(1.0)
    } else {
        1.0
    };
    let eased = 1.0 - (1.0 - t).powi(3);
    let shrink = Shrink {
        about,
        scale: 1.0 + (settings.ghost_scale - 1.0) * eased,
        alpha: 1.0 + (settings.ghost_opacity - 1.0) * eased as f32,
    };
    (shrink, t < 1.0)
}

/// Draw the pane being dragged as a ghost held at `grab` from the
/// pointer: its outline, its chrome and its client.
fn paint_ghost(
    paint: &mut Paint,
    elements: &mut Vec<Element>,
    layout: &Layout,
    chrome: Chrome,
    shrink: Shrink,
    grab: Point<i32, Physical>,
) {
    let Some(pane) = chrome.dragging.and_then(|id| layout.pane(id)) else {
        return;
    };
    let font = &paint.text.chrome;
    let origin = shrink.about - grab;
    let cell: Point<i32, Physical> = (font.cell_width as i32, font.cell_height as i32).into();
    let outer = font.px_rect(pane.rect).size;
    let offset = font.px_rect(pane.client_rect()).loc - font.px_rect(pane.rect).loc;
    let start = shrink.point(origin + cell.downscale(2));
    let end = shrink.point(origin + outer - cell.downscale(2));
    let mut outline = Scene::new(&paint.text.chrome);
    let line = outline.line_width();
    outline.box_lines(
        start.x,
        start.y,
        end.x - start.x + line,
        end.y - start.y + line,
        ACCENT,
    );
    paint.scene(elements, &outline, Point::default(), None);
    let mut ghost = Scene::new(&paint.text.chrome);
    layout.draw_ghost(pane.id, &mut ghost, chrome);
    paint.scene(elements, &ghost, origin + cell, Some(shrink));
    let Some(toplevel) = pane.toplevel() else {
        return;
    };
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            paint.renderer,
            toplevel.wl_surface(),
            origin + offset,
            paint.scale,
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

/// Draw the client of every visible pane but the one being dragged,
/// cropped to the pane.
fn paint_clients(paint: &mut Paint, elements: &mut Vec<Element>, layout: &Layout, chrome: Chrome) {
    for pane in layout.visible() {
        let Some(toplevel) = pane.toplevel() else {
            continue;
        };
        if chrome.dragging == Some(pane.id) {
            continue;
        }
        let rect = pane.client_rect();
        if rect.columns == 0 || rect.rows == 0 {
            continue;
        }
        let crop = paint.text.chrome.px_rect(rect);
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                paint.renderer,
                toplevel.wl_surface(),
                crop.loc,
                paint.scale,
                1.0,
                Kind::Unspecified,
            );
        elements.extend(
            surfaces
                .into_iter()
                .filter_map(|surface| CropRenderElement::from_element(surface, paint.scale, crop))
                .map(Element::Surface),
        );
    }
}

/// Draw the picker's grid with a thumbnail of each workspace's panes in
/// its cell.
fn paint_picker(
    paint: &mut Paint,
    elements: &mut Vec<Element>,
    (picker, grid): (&Picker, &Grid),
    (workspaces, current): (&[Workspace], usize),
    pointer: Point<i32, Physical>,
) {
    let (over, under) = picker.draw(grid, workspaces, current, pointer, paint.text);
    let mut surfaces = Vec::new();
    for (cell, workspace) in grid.cells.iter().zip(workspaces) {
        let layout = workspace.layout();

        // the cell shows the pane area, not the top bar and sidebar, so
        // positions are taken from the area's corner and scaled to fit it
        let area = paint.text.chrome.px_rect(layout.area);
        let (width, height) = (area.size.w, area.size.h);
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
            let client = paint.text.chrome.px_rect(pane.client_rect());
            let origin = anchor - area.loc + client.loc;
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
    paint.scene(elements, &over, Point::default(), None);
    elements.extend(surfaces);
    paint.scene(elements, &under, Point::default(), None);
}
