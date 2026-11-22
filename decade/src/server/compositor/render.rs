use std::os::unix::io::OwnedFd;
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
use smithay::utils::{Physical, Point, Rectangle, Transform};

use decade_ui::palette::{ACCENT, BACKGROUND, Rgba};
use decade_ui::{Chrome, Pulse, Scene, Shape};

use crate::chrome::frame;
use crate::chrome::picker::{self, Grid, Picker};
use crate::chrome::settings::Settings;
use crate::server::agents;
use crate::server::chrome::draw::{Element, Paint, Shrink, px_point, px_size};
use crate::server::workspace::{self, PaneAction, Tab, Workspace};

use super::input::SidebarDrag;
use super::{Overlay, State, popups_over, send_frames_surface_tree, window_origin};

const SCRIM: Rgba = [0.0, 0.0, 0.0, 0.35];

impl State {
    /// Draw the workspace into `target`, and return the fence to wait on and
    /// whether the frame is part of an animation that wants the next one.
    pub(super) fn render<T>(
        &mut self,
        target: &mut T,
        workspace: usize,
    ) -> Result<(Option<OwnedFd>, bool), String>
    where
        GlesRenderer: Bind<T>,
    {
        let current = self.current();
        let picking = matches!(self.overlay, Some(Overlay::Picker { .. }));
        let picker_grid = (workspace == current && picking).then(|| self.picker_grid());
        if picker_grid.is_none() {
            let now = Instant::now();
            if self.sidebar_open {
                for shown in &mut self.workspaces {
                    shown.refresh_branch(now);
                }
            }
            self.workspaces[workspace].refresh_running(now, &workspace::home());
            self.workspaces[workspace].name_agents();
        }
        let frame = picker_grid.is_none().then(|| self.frame(workspace));
        let prompt = self.prompt();
        let State {
            gpu,
            size,
            workspaces,
            text,
            start,
            scale,
            bar,
            hover,
            tab_hover,
            pointer,
            overlay,
            settings,
            rounded,
            ..
        } = self;
        let tab = workspaces[workspace].tab();
        let layout = &tab.layout;
        let now = Instant::now();
        let named = |name: &str| (layout.panes.iter()).find(|pane| pane.name == name);
        let pulse = (workspaces[workspace].point.as_ref()).and_then(|point| {
            Some(Pulse {
                pane: named(&point.pane)?.id,
                strength: point.pulse(now),
                note: point.note.as_deref(),
            })
        });
        let flash = (workspaces[workspace].flash_at(now))
            .and_then(|(name, flash)| Some((named(name)?.id, flash)));
        let shutter = (workspaces[workspace].shutter_at(now))
            .and_then(|(name, alpha)| Some((layout.pane(layout.by_name(name)?)?, alpha)))
            .map(|(pane, alpha)| (text.px_rect(pane.client_rect()), alpha));
        let overlay = if workspace == current {
            overlay.as_ref()
        } else {
            None
        };
        let chrome = if workspace == current {
            Chrome {
                bar: *bar,
                hover: *hover,
                tab_hover: *tab_hover,
                pulse,
                flash,
            }
        } else {
            Chrome {
                pulse,
                flash,
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
        if let Some(prompt) = &prompt {
            let mut scene = Scene::new(&paint.text.fonts.menu);
            prompt.draw(&mut scene);
            paint.scene(&mut elements, &scene, Point::default(), None);
        }

        let window = (size.w, size.h);
        match overlay {
            Some(Overlay::Menu(menu)) => {
                let mut scene = Scene::new(&paint.text.fonts.menu);
                menu.draw(&mut scene, window);
                paint.scene(&mut elements, &scene, Point::default(), None);
            }
            Some(Overlay::Context(menu)) => {
                let mut scene = Scene::new(&paint.text.fonts.chrome);
                menu.draw(&mut scene);
                paint.scene(&mut elements, &scene, Point::default(), None);
            }
            Some(Overlay::Rename { field, .. }) => {
                let mut scene = Scene::new(&paint.text.fonts.chrome);
                field.draw(&mut scene);
                paint.scene(&mut elements, &scene, Point::default(), None);
            }
            Some(Overlay::Panel(panel)) => {
                let mut scene = Scene::new(&paint.text.fonts.menu);
                panel.draw(&mut scene, settings, window);
                paint.scene(&mut elements, &scene, Point::default(), None);
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
        let scrimmed =
            |overlay: &Overlay| !matches!(overlay, Overlay::Context(_) | Overlay::Rename { .. });
        if prompt.is_some() || overlay.is_some_and(scrimmed) {
            let mut scrim = Scene::new(&paint.text.fonts.chrome);
            scrim.rect(0, 0, size.w, size.h, SCRIM);
            paint.scene(&mut elements, &scrim, Point::default(), None);
        }

        let mut animating = false;
        if let Some(drag) = layout.dragged()
            && layout.pane(drag.pane).is_some()
        {
            let shrink;
            (shrink, animating) = ghost_shrink(settings, drag.start, pointer);
            paint_ghost(&mut paint, &mut elements, tab, chrome, shrink, drag.grab);
        }

        let mut above = Scene::new(&paint.text.fonts.chrome);
        layout.draw_bars(&mut above, chrome);
        if let Some(frame) = &frame {
            frame.draw_above(&mut above);
        }
        layout.draw_drop(&mut above);
        paint.scene(&mut elements, &above, Point::default(), None);

        if let Some((rect, alpha)) = shutter {
            let mut layer = Scene::new(&paint.text.fonts.chrome);
            layer.rect(
                rect.loc.x,
                rect.loc.y,
                rect.size.w,
                rect.size.h,
                [1.0, 1.0, 1.0, alpha],
            );
            paint.scene(&mut elements, &layer, Point::default(), None);
        }

        if let Some(frame) = frame {
            let mut below = Scene::new(&paint.text.fonts.chrome);
            layout.draw(&mut below, chrome);
            frame.draw(&mut below);
            paint_clients(&mut paint, &mut elements, tab, &below);
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
            if let Some(toplevel) = tab.toplevel(pane.id) {
                send_frames_surface_tree(toplevel.wl_surface(), time);
                for (popup, _) in popups_over(toplevel.wl_surface()) {
                    send_frames_surface_tree(popup.wl_surface(), time);
                }
            }
        }
        Ok((sync.export(), animating))
    }

    /// The top bar and sidebar drawn around the workspace's panes.
    pub(super) fn frame(&self, shown: usize) -> frame::Frame {
        let on_screen = shown == self.current();
        let hover = self.frame_hover.filter(|_| on_screen);
        let home = workspace::home();
        let now = Instant::now();
        let pulse =
            |workspace: &Workspace| (workspace.point.as_ref()).map(|point| point.pulse(now));
        let entries = (self.workspaces.iter().enumerate())
            .map(|(i, workspace)| frame::Entry {
                id: workspace.id,
                name: workspace.name.clone(),
                path: frame::short_path(&workspace.cwd, &home),
                branch: workspace.branch.clone(),
                agents: (workspace.router.agents.list.iter())
                    .map(|agent| card_agent(agent, now))
                    .collect(),
                current: i == shown,
                pulse: pulse(workspace).filter(|_| i != shown),
                group: workspace.group,
                asleep: workspace.asleep.is_some(),
            })
            .collect();
        let shown = &self.workspaces[shown];
        let pointed_tab = (shown.point.as_ref())
            .and_then(|point| shown.by_name(&point.pane))
            .map(|(tab, _)| tab)
            .filter(|&tab| tab != shown.active_tab);
        let tabs = (shown.tabs.iter().enumerate())
            .map(|(index, tab)| {
                let names: Vec<_> = (tab.layout.panes.iter())
                    .map(|pane| pane.name.as_str())
                    .collect();
                let state = (shown.router.agents.list.iter())
                    .filter(|agent| {
                        agent.connected
                            && (agent.pane.as_deref()).is_some_and(|pane| names.contains(&pane))
                    })
                    .filter_map(|agent| agent.report.state)
                    .max();
                let label = match &tab.name {
                    Some(name) => name.clone(),
                    None => tab.running_label(tab.layout.focused).unwrap_or_default(),
                };
                frame::Tab {
                    label,
                    state,
                    pulse: pulse(shown).filter(|_| pointed_tab == Some(index)),
                }
            })
            .collect();
        let layout = shown.layout();
        let title = shown.tab().title(layout.focused).unwrap_or_default();
        let mut frame = frame::Frame::new(
            self.grid(),
            self.sidebar_open,
            (entries, self.groups.clone()),
            (tabs, shown.active_tab),
            title,
            layout.zoom.is_some(),
            (
                self.text.fonts.chrome.cell_width as i32,
                self.text.fonts.chrome.cell_height as i32,
            ),
        );
        frame.hover = hover;
        frame.breath = frame::breath(self.start.elapsed());
        if let Some(SidebarDrag {
            hit, moved: true, ..
        }) = self.sidebar_drag.as_ref().filter(|_| on_screen)
        {
            let pointer = self.pointer.to_physical(self.scale);
            frame.drag = Some((*hit, (pointer.x as i32, pointer.y as i32)));
        }
        frame
    }
}

/// The agent as its workspace's card and popover show it at `now`.
fn card_agent(agent: &agents::Agent, now: Instant) -> frame::Agent {
    let dim = match (agent.connected, agent.pane.is_some()) {
        (_, false) => Some(frame::Dim::NoPane),
        (false, true) => Some(frame::Dim::Disconnected),
        (true, true) => None,
    };
    let since = if dim.is_some() {
        agent.last_seen
    } else {
        agent.since
    };
    let report = &agent.report;
    frame::Agent {
        name: agent.name.clone(),
        state: report.state,
        age: now.saturating_duration_since(since),
        message: report.message().map(str::to_string),
        model: report.model.clone(),
        context: report.context,
        tokens: report.tokens,
        dim,
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
    tab: &Tab,
    chrome: Chrome<PaneAction>,
    shrink: Shrink,
    grab: decade_ui::Point,
) {
    let layout = &tab.layout;
    let Some(pane) = layout.dragged().and_then(|drag| layout.pane(drag.pane)) else {
        return;
    };
    let font = &paint.text.fonts.chrome;
    let origin = shrink.about - Point::from((grab.x, grab.y));
    let cell: Point<i32, Physical> = (font.cell_width as i32, font.cell_height as i32).into();
    let outer = paint.text.px_rect(pane.rect).size;
    let start = shrink.point(origin + cell.downscale(2));
    let end = shrink.point(origin + outer - cell.downscale(2));
    let mut outline = Scene::new(&paint.text.fonts.chrome);
    let line = outline.line_width();
    outline.box_lines(
        start.x,
        start.y,
        end.x - start.x + line,
        end.y - start.y + line,
        ACCENT,
    );
    paint.scene(elements, &outline, Point::default(), None);
    let mut ghost = Scene::new(&paint.text.fonts.chrome);
    layout.draw_ghost(pane.id, &mut ghost, chrome);
    paint.scene(elements, &ghost, origin + cell, Some(shrink));
    for (id, at, _) in contents(&ghost) {
        let toplevel = &tab.clients[&id].toplevel;
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                paint.renderer,
                toplevel.wl_surface(),
                origin + cell + px_point(at),
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
}

/// The content shapes in `scene`, in the order it draws them.
fn contents(
    scene: &Scene,
) -> impl Iterator<Item = (decade_ui::ContentId, decade_ui::Point, decade_ui::Size)> {
    scene.shapes.iter().filter_map(|shape| match *shape {
        Shape::Content { id, at, size } => Some((id, at, size)),
        _ => None,
    })
}

/// Draw the client at each content shape of `scene`, with its popups in
/// front, cropped to the shape.
fn paint_clients(paint: &mut Paint, elements: &mut Vec<Element>, tab: &Tab, scene: &Scene) {
    for (id, at, size) in contents(scene) {
        if size.width == 0 || size.height == 0 {
            continue;
        }
        let toplevel = &tab.clients[&id].toplevel;
        let crop = Rectangle::new(px_point(at), px_size(size));
        let mut surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
        for (popup, at) in popups_over(toplevel.wl_surface()) {
            surfaces.extend(render_elements_from_surface_tree(
                paint.renderer,
                popup.wl_surface(),
                crop.loc + at.to_physical_precise_round(paint.scale),
                paint.scale,
                1.0,
                Kind::Unspecified,
            ));
        }

        // the pane shows the window geometry, so the buffer's
        // corner sits up and left of the pane by the margin
        // the client draws its shadow in
        let origin = window_origin(toplevel.wl_surface()).to_physical_precise_round(paint.scale);
        surfaces.extend(render_elements_from_surface_tree(
            paint.renderer,
            toplevel.wl_surface(),
            crop.loc - origin,
            paint.scale,
            1.0,
            Kind::Unspecified,
        ));
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
    let entries: Vec<picker::Entry> = workspaces
        .iter()
        .map(|workspace| picker::Entry {
            id: workspace.id,
            name: &workspace.name,
            state: workspace.agent_state(),
            closing: workspace.closing(),
        })
        .collect();
    let pointer = decade_ui::Point {
        x: pointer.x,
        y: pointer.y,
    };
    let (over, under) = picker.draw(grid, &entries, current, pointer, &paint.text.fonts);
    let mut surfaces = Vec::new();
    for (cell, workspace) in grid.cells.iter().zip(workspaces) {
        let cell = Rectangle::<i32, Physical>::new(
            (cell.at.x, cell.at.y).into(),
            (cell.size.width, cell.size.height).into(),
        );
        let tab = workspace.tab();
        let layout = &tab.layout;

        // the cell shows the pane area, not the top bar and sidebar, so
        // positions are taken from the area's corner and scaled to fit it
        let area = paint.text.px_rect(layout.area);
        let (width, height) = (area.size.w, area.size.h);
        let scale = (cell.size.w as f64 / width.max(1) as f64)
            .min(cell.size.h as f64 / height.max(1) as f64);
        let anchor = cell.loc
            + Point::from((
                (cell.size.w - (width as f64 * scale) as i32) / 2,
                (cell.size.h - (height as f64 * scale) as i32) / 2,
            ));
        for pane in layout.visible() {
            let Some(toplevel) = tab.toplevel(pane.id) else {
                continue;
            };
            let client = paint.text.px_rect(pane.client_rect());
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
                CropRenderElement::from_element(scaled, paint.scale, cell).map(Element::Thumbnail)
            }));
        }
    }
    paint.scene(elements, &over, Point::default(), None);
    elements.extend(surfaces);
    paint.scene(elements, &under, Point::default(), None);
}
