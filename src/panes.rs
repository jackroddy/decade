use std::path::PathBuf;

use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData};

use crate::draw::{Face, Scene};
use crate::frame::fit;
use crate::palette;
use crate::palette::Rgba;

pub const BACKGROUND: Rgba = palette::BACKGROUND;
pub const FOREGROUND: Rgba = palette::FOREGROUND;
const PANE: Rgba = palette::DARK_BACKGROUND;
const OUTLINE: Rgba = palette::MUTED;
const FOCUS: Rgba = palette::ACCENT;
const DROP: Rgba = palette::CYAN;
const BAR: Rgba = palette::LIGHTER_BACKGROUND;
const BAR_FOCUSED: Rgba = palette::ACCENT;
const BAR_FOCUSED_TEXT: Rgba = palette::DARK_BACKGROUND;
const HOVER: Rgba = palette::SELECTION;
const CLOSE_HOVER: Rgba = palette::RED;
const UNPINNED: Rgba = palette::MUTED;
const DIM: Rgba = palette::LIGHT_FOREGROUND;

const MIN_PANE: u32 = 4;
const EDGE_BAND: u32 = 2;
const BUTTON_WIDTH: u32 = 4;
const PIN_ICON: char = '\u{f0403}';
const UNPINNED_ICON: char = '\u{f0931}';
const ZOOM_ICON: char = '\u{f0293}';
const UNZOOM_ICON: char = '\u{f0294}';
const CLOSE_ICON: char = '\u{f0156}';
const SCRATCH_ICON: char = '\u{f0093}';
const UNSCRATCHED_ICON: char = '\u{f0096}';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellRect {
    pub column: u32,
    pub row: u32,
    pub columns: u32,
    pub rows: u32,
}

impl CellRect {
    const EMPTY: CellRect = CellRect {
        column: 0,
        row: 0,
        columns: 0,
        rows: 0,
    };

    fn contains(&self, column: u32, row: u32) -> bool {
        column >= self.column
            && column < self.column + self.columns
            && row >= self.row
            && row < self.row + self.rows
    }

    fn center(&self) -> (i64, i64) {
        (
            (self.column * 2 + self.columns) as i64,
            (self.row * 2 + self.rows) as i64,
        )
    }

    pub fn inner(&self) -> CellRect {
        if self.columns < 3 || self.rows < 3 {
            return CellRect::EMPTY;
        }
        CellRect {
            column: self.column + 1,
            row: self.row + 1,
            columns: self.columns - 2,
            rows: self.rows - 2,
        }
    }

    fn is_empty(&self) -> bool {
        self.columns == 0 || self.rows == 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Horizontal,
    Vertical,
}

pub enum Kind {
    Anchor { app_id: String },
    Transient,
}

/// One window in a pane, shown as one of the pane's tabs.
pub struct Client {
    pub toplevel: ToplevelSurface,
    pub app_id: Option<String>,
    pub pid: Option<u32>,

    /// Whether decade started the client as a terminal tab of the pane.
    pub terminal_tab: bool,
}

impl Client {
    /// The client's title, or the last part of its app id.
    fn label(&self) -> Option<String> {
        let title = with_states(self.toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().title.clone())
        });
        title.filter(|title| !title.is_empty()).or_else(|| {
            let app_id = self.app_id.as_deref()?;
            Some(app_id.rsplit('.').next().unwrap_or(app_id).to_string())
        })
    }
}

pub struct Pane {
    pub id: usize,
    pub name: String,
    pub kind: Kind,
    pub rect: CellRect,

    /// The pane's tabs, one window each.
    pub clients: Vec<Client>,

    /// The index in `clients` of the tab drawn.
    pub shown: usize,

    /// The number in the app id of the next terminal tab decade starts.
    pub next_tab: u32,

    /// The directories of the terminal tabs to start for a restored pane.
    pub saved_tabs: Vec<PathBuf>,

    /// The app id of the last client in the pane.
    pub app_id: Option<String>,

    /// Where decade starts the pane's shell, for a pane that holds a terminal.
    pub cwd: Option<PathBuf>,

    /// The ghostty decade started for the pane.
    pub pid: Option<u32>,
}

impl Pane {
    /// Whether the pane is a pinned terminal whose shell has exited.
    pub fn restartable(&self) -> bool {
        self.clients.is_empty() && self.cwd.is_some() && matches!(self.kind, Kind::Anchor { .. })
    }

    /// The window of the tab drawn.
    pub fn toplevel(&self) -> Option<&ToplevelSurface> {
        self.clients.get(self.shown).map(|client| &client.toplevel)
    }

    /// Show the next tab, or the previous one, wrapping at the ends.
    pub fn step_tab(&mut self, forward: bool) {
        self.shown = stepped(self.shown, self.clients.len(), forward);
    }

    /// Remove the tab at `index`, showing the tab after it, or the one
    /// before it when it was the last.
    pub fn remove_client(&mut self, index: usize) {
        self.clients.remove(index);
        self.shown = shown_after_removal(self.shown, index, self.clients.len());
    }

    pub fn new(id: usize, name: String, kind: Kind) -> Pane {
        Pane {
            id,
            name,
            kind,
            rect: CellRect::EMPTY,
            clients: Vec::new(),
            shown: 0,
            next_tab: 2,
            saved_tabs: Vec::new(),
            app_id: None,
            cwd: None,
            pid: None,
        }
    }
}

pub enum Node {
    Leaf(usize),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarButton {
    Scratch,
    Pin,
    Zoom,
    Close,
    Name,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drop {
    Swap(usize),
    Split { target: usize, side: Side },
    Edge(Side),
}

pub struct Boundary {
    path: Vec<bool>,
    axis: Axis,
}

pub struct Layout {
    /// The cells the panes share, inside decade's top bar and sidebar.
    pub area: CellRect,
    pub panes: Vec<Pane>,
    pub focused: usize,
    pub zoom: Option<usize>,
    pub bar: bool,
    pub drag_cell: Option<(u32, u32)>,
    pub dragging: Option<usize>,
    pub hover: Option<(usize, BarButton)>,
    root: Node,
    next_id: usize,
}

impl Layout {
    pub fn new(area: CellRect) -> Layout {
        let mut layout = Layout {
            area,
            panes: vec![Pane::new(0, "terminal".to_string(), Kind::Transient)],
            focused: 0,
            zoom: None,
            bar: false,
            drag_cell: None,
            dragging: None,
            hover: None,
            root: Node::Leaf(0),
            next_id: 1,
        };
        layout.arrange();
        layout
    }

    pub fn restore(
        area: CellRect,
        panes: Vec<Pane>,
        root: Node,
        focused: usize,
        zoom: Option<usize>,
    ) -> Layout {
        let next_id = panes.iter().map(|pane| pane.id + 1).max().unwrap_or(0);
        let mut layout = Layout {
            area,
            panes,
            focused,
            zoom,
            bar: false,
            drag_cell: None,
            dragging: None,
            hover: None,
            root,
            next_id,
        };
        layout.arrange();
        layout
    }

    pub fn root(&self) -> &Node {
        &self.root
    }

    pub fn resize(&mut self, area: CellRect) {
        self.area = area;
        self.arrange();
    }

    fn body(&self) -> CellRect {
        self.area
    }

    pub fn arrange(&mut self) {
        for pane in &mut self.panes {
            pane.rect = CellRect::EMPTY;
        }
        let body = self.body();
        if let Some(zoom) = self.zoom {
            if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == zoom) {
                pane.rect = body;
                return;
            }
            self.zoom = None;
        }
        let mut rects = Vec::new();
        arrange_node(&self.root, body, &mut rects);
        for (id, rect) in rects {
            if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) {
                pane.rect = rect;
            }
        }
    }

    pub fn pane(&self, id: usize) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    pub fn pane_mut(&mut self, id: usize) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }

    pub fn by_name(&self, name: &str) -> Option<usize> {
        self.panes
            .iter()
            .find(|pane| pane.name == name)
            .map(|pane| pane.id)
    }

    pub fn visible(&self) -> impl Iterator<Item = &Pane> {
        self.panes.iter().filter(|pane| !pane.rect.is_empty())
    }

    pub fn client_rect(&self, pane: &Pane) -> CellRect {
        client_area(pane.rect, pane.clients.len())
    }

    pub fn pane_at(&self, column: u32, row: u32) -> Option<usize> {
        self.visible()
            .find(|pane| pane.rect.contains(column, row))
            .map(|pane| pane.id)
    }

    pub fn neighbour(&self, direction: Direction) -> Option<usize> {
        let (x, y) = self.pane(self.focused)?.rect.center();
        let mut best: Option<(i64, usize)> = None;
        for pane in self.visible() {
            if pane.id == self.focused {
                continue;
            }
            let (px, py) = pane.rect.center();
            let (ahead, across) = match direction {
                Direction::Left => (x - px, (y - py).abs()),
                Direction::Right => (px - x, (y - py).abs()),
                Direction::Up => (y - py, (x - px).abs()),
                Direction::Down => (py - y, (x - px).abs()),
            };
            if ahead <= 0 {
                continue;
            }
            let distance = ahead + across * 2;
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, pane.id));
            }
        }
        best.map(|(_, id)| id)
    }

    pub fn resize_toward(&mut self, id: usize, direction: Direction, cells: u32) {
        if self.zoom.is_some() {
            return;
        }
        let axis = match direction {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        };
        let mut path = Vec::new();
        let mut found = None;
        split_path(&self.root, id, axis, &mut path, &mut found);
        let Some(path) = found else {
            return;
        };
        let area = node_area(&self.root, self.body(), &path);
        let Some(Node::Split { ratio, .. }) = node_at(&mut self.root, &path) else {
            return;
        };
        let (start, length) = match axis {
            Axis::Horizontal => (area.column, area.columns),
            Axis::Vertical => (area.row, area.rows),
        };
        let edge = start + (length as f32 * *ratio).round() as u32;
        let edge = match direction {
            Direction::Right | Direction::Down => edge + cells,
            Direction::Left | Direction::Up => edge.saturating_sub(cells),
        };
        let boundary = Boundary { path, axis };
        self.drag_boundary(&boundary, edge, edge);
    }

    /// The pane a new window with this app id goes in, and whether the
    /// window is a terminal tab decade started for that pane.
    pub fn place(&self, app_id: Option<&str>, prefix: &str) -> Option<(usize, bool)> {
        let app_id = app_id?;
        if let Some(pane) = self
            .panes
            .iter()
            .find(|pane| is_terminal_tab(app_id, prefix, &pane.name))
        {
            return Some((pane.id, true));
        }

        // a restored pane's own terminal can arrive after its tabs
        if let Some(pane) = self.panes.iter().find(|pane| {
            app_id.strip_prefix(prefix) == Some(&pane.name)
                && pane.clients.iter().all(|client| client.terminal_tab)
        }) {
            return Some((pane.id, false));
        }
        self.panes
            .iter()
            .find(|pane| {
                pane.clients.is_empty()
                    && match &pane.kind {
                        Kind::Anchor { app_id: wanted } => wanted == app_id,
                        Kind::Transient => pane.app_id.as_deref() == Some(app_id),
                    }
            })
            .map(|pane| (pane.id, false))
    }

    pub fn add_transient(&mut self, name: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let at = self.focused;
        let axis = match self.pane(at) {
            Some(pane) if pane.rect.columns >= pane.rect.rows * 2 => Axis::Horizontal,
            _ => Axis::Vertical,
        };
        self.panes.push(Pane::new(id, name, Kind::Transient));
        if !split_leaf(&mut self.root, at, axis, id) {
            let old = std::mem::replace(&mut self.root, Node::Leaf(id));
            self.root = Node::Split {
                axis,
                ratio: 0.5,
                first: Box::new(old),
                second: Box::new(Node::Leaf(id)),
            };
        }
        self.zoom = None;
        self.focused = id;
        self.arrange();
        id
    }

    pub fn remove(&mut self, id: usize) {
        let Some(index) = self.panes.iter().position(|pane| pane.id == id) else {
            return;
        };
        if self.panes.len() == 1 {
            self.panes[index].clients.clear();
            self.panes[index].shown = 0;
            return;
        }
        self.panes.remove(index);
        let heir = sibling_leaf(&self.root, id);
        let root = std::mem::replace(&mut self.root, Node::Leaf(usize::MAX));
        self.root = remove_leaf(root, id);
        if self.zoom == Some(id) {
            self.zoom = None;
        }
        self.arrange();
        if self.focused == id {
            let next = heir
                .or_else(|| self.visible().next().map(|pane| pane.id))
                .or_else(|| self.panes.first().map(|pane| pane.id))
                .unwrap_or(0);
            self.focused = next;
        }
    }

    pub fn toggle_zoom(&mut self, id: usize) {
        self.zoom = if self.zoom == Some(id) {
            None
        } else {
            Some(id)
        };
        self.arrange();
    }

    pub fn boundary_at(&self, column: u32, row: u32) -> Option<Boundary> {
        if self.zoom.is_some() {
            return None;
        }
        let mut path = Vec::new();
        find_boundary(&self.root, self.body(), column, row, &mut path)
    }

    pub fn drop_at(&self, dragged: usize, column: u32, row: u32) -> Option<Drop> {
        let body = self.body();
        if self.panes.len() < 2 || row >= body.rows || body.columns <= EDGE_BAND * 2 {
            return None;
        }
        if column < EDGE_BAND {
            return Some(Drop::Edge(Side::Left));
        }
        if column >= body.columns - EDGE_BAND {
            return Some(Drop::Edge(Side::Right));
        }
        if row < EDGE_BAND {
            return Some(Drop::Edge(Side::Above));
        }
        if row >= body.rows.saturating_sub(EDGE_BAND) {
            return Some(Drop::Edge(Side::Below));
        }
        let target = self.pane_at(column, row).filter(|&id| id != dragged)?;
        drop_zones(self.pane(target)?.rect)
            .into_iter()
            .find(|(_, zone)| zone.contains(column, row))
            .map(|(side, _)| match side {
                None => Drop::Swap(target),
                Some(side) => Drop::Split { target, side },
            })
    }

    pub fn apply_drop(&mut self, dragged: usize, drop: Drop) {
        let root = std::mem::replace(&mut self.root, Node::Leaf(dragged));
        self.root = dropped(root, dragged, drop);
        self.zoom = None;
        self.arrange();
    }

    pub fn draw_drop(&self, scene: &mut Scene) {
        let (Some(dragged), Some((column, row))) = (self.dragging, self.drag_cell) else {
            return;
        };
        let active = self.drop_at(dragged, column, row);
        if let Some(target) = self.pane_at(column, row).filter(|&id| id != dragged)
            && let Some(pane) = self.pane(target)
        {
            for (side, zone) in drop_zones(pane.rect) {
                let drop = match side {
                    None => Drop::Swap(target),
                    Some(side) => Drop::Split { target, side },
                };
                draw_zone(scene, zone, active == Some(drop));
            }
        }
        let body = self.body();
        let strips = [
            (
                Side::Left,
                CellRect {
                    columns: EDGE_BAND,
                    ..body
                },
            ),
            (
                Side::Right,
                CellRect {
                    column: body.columns.saturating_sub(EDGE_BAND),
                    columns: EDGE_BAND,
                    ..body
                },
            ),
            (
                Side::Above,
                CellRect {
                    rows: EDGE_BAND,
                    ..body
                },
            ),
            (
                Side::Below,
                CellRect {
                    row: body.rows.saturating_sub(EDGE_BAND),
                    rows: EDGE_BAND,
                    ..body
                },
            ),
        ];
        for (side, strip) in strips {
            draw_zone(scene, strip, active == Some(Drop::Edge(side)));
        }
    }

    pub fn drag_boundary(&mut self, boundary: &Boundary, column: u32, row: u32) {
        let area = node_area(&self.root, self.body(), &boundary.path);
        let Some(node) = node_at(&mut self.root, &boundary.path) else {
            return;
        };
        let Node::Split { ratio, .. } = node else {
            return;
        };
        let (position, start, length) = match boundary.axis {
            Axis::Horizontal => (column, area.column, area.columns),
            Axis::Vertical => (row, area.row, area.rows),
        };
        if length < MIN_PANE * 2 {
            return;
        }
        let first = position
            .saturating_sub(start)
            .clamp(MIN_PANE, length - MIN_PANE);
        *ratio = first as f32 / length as f32;
        self.arrange();
    }

    fn bar_row(&self, pane: &Pane) -> Option<(u32, u32, u32)> {
        if !self.bar {
            return None;
        }
        let inner = pane.rect.inner();
        if inner.is_empty() {
            return None;
        }
        Some((inner.column, inner.row, inner.columns))
    }

    pub fn bar_hit(&self, column: u32, row: u32) -> Option<(usize, BarButton)> {
        for pane in self.visible() {
            let Some((start, bar_row, width)) = self.bar_row(pane) else {
                continue;
            };
            if row != bar_row || column < start || column >= start + width {
                continue;
            }
            let button = bar_buttons(start, width)
                .into_iter()
                .find(|&(_, x)| column >= x && column < x + BUTTON_WIDTH)
                .map_or(BarButton::Name, |(button, _)| button);
            return Some((pane.id, button));
        }
        None
    }

    /// The pane and tab index of the tab label under the cell.
    pub fn tab_hit(&self, column: u32, row: u32) -> Option<(usize, usize)> {
        self.visible()
            .filter(|pane| pane.clients.len() > 1)
            .find_map(|pane| {
                let inner = pane.rect.inner();
                if inner.is_empty() || row != inner.row {
                    return None;
                }
                let index = tab_at(inner, pane.clients.len(), column)?;
                Some((pane.id, index))
            })
    }

    /// Draw every visible pane's outline and, for an empty pane, what it
    /// waits for; `scratch` names the workspace's scratch pane.
    pub fn draw(&self, scene: &mut Scene, scratch: Option<&str>) {
        for pane in self.visible() {
            if self.dragging == Some(pane.id) {
                continue;
            }
            let outline = if pane.id == self.focused {
                FOCUS
            } else {
                OUTLINE
            };
            let scratched = scratch == Some(pane.name.as_str());
            self.draw_outline(scene, pane, outline, scratched);
            draw_waiting(scene, pane, pane.rect.inner(), scratched);
            draw_strip(scene, pane, pane.rect.inner(), pane.id == self.focused);
        }
    }

    /// The pane's outline, with a tag on its top edge when it is pinned,
    /// zoomed or the scratch pane.
    fn draw_outline(&self, scene: &mut Scene, pane: &Pane, color: Rgba, scratched: bool) {
        let (cell_width, cell_height) = scene.cell();
        let line = scene.line_width();
        let rect = pane.rect;
        let area = self.area;

        // half a cell where the pane meets the edge of the area and a
        // quarter where it meets another pane, so the gap between two
        // outlines is as wide as the margin at the edge
        let inset = |outer: bool, cell: i32| if outer { cell / 2 } else { cell / 4 };
        let left = inset(rect.column == area.column, cell_width);
        let top = inset(rect.row == area.row, cell_height);
        let right = inset(
            rect.column + rect.columns == area.column + area.columns,
            cell_width,
        );
        let bottom = inset(rect.row + rect.rows == area.row + area.rows, cell_height);
        scene.outline(rect, [left, top, right, bottom], color);

        let (x, y) = scene.px(rect.column, rect.row);
        let (width, _) = scene.px(rect.columns, 0);
        let line_y = y + top;
        let text_y = line_y + line / 2 - cell_height / 2;
        let mut end = x + width - right - cell_width * 2;
        let pinned = matches!(pane.kind, Kind::Anchor { .. });
        let zoomed = self.zoom == Some(pane.id);
        for (text, face) in [
            (PIN_ICON.to_string(), Face::Icon),
            ("zoom".to_string(), Face::Chrome),
            ("scratch".to_string(), Face::Chrome),
        ]
        .into_iter()
        .zip([pinned, zoomed, scratched])
        .filter_map(|(tag, shown)| shown.then_some(tag))
        {
            let start = end - text.chars().count() as i32 * cell_width;
            if start - cell_width < x + left + cell_width {
                break;
            }
            let gap = cell_width / 2;
            scene.rect(start - gap, line_y, end - start + gap * 2, line, BACKGROUND);
            scene.text_px(start, text_y, &text, face, color);
            end = start - cell_width;
        }
    }

    pub fn draw_bars(&self, scene: &mut Scene, scratch: Option<&str>) {
        if !self.bar {
            return;
        }
        for pane in self.visible() {
            if self.dragging != Some(pane.id) {
                let scratched = scratch == Some(pane.name.as_str());
                self.draw_bar(scene, pane, pane.rect.inner(), scratched);
            }
        }
    }

    pub fn draw_ghost(&self, id: usize, scene: &mut Scene, scratch: Option<&str>) {
        let Some(pane) = self.pane(id) else {
            return;
        };
        let inner = CellRect {
            column: 0,
            row: 0,
            ..pane.rect.inner()
        };
        let scratched = scratch == Some(pane.name.as_str());
        draw_waiting(scene, pane, inner, scratched);
        draw_strip(scene, pane, inner, pane.id == self.focused);
        if self.bar {
            self.draw_bar(scene, pane, inner, scratched);
        }
    }

    fn draw_bar(&self, scene: &mut Scene, pane: &Pane, inner: CellRect, scratched: bool) {
        if inner.is_empty() {
            return;
        }
        let (start, row, width) = (inner.column, inner.row, inner.columns);
        let focused = pane.id == self.focused;
        let (bg, fg) = if focused {
            (BAR_FOCUSED, BAR_FOCUSED_TEXT)
        } else {
            (BAR, DIM)
        };
        scene.fill(CellRect { rows: 1, ..inner }, bg);
        let buttons = bar_buttons(start, width);
        let room = buttons[0].1.saturating_sub(start + 2) as usize;
        scene.text(start + 1, row, &fit(&pane.name, room), fg);
        let pinned = matches!(pane.kind, Kind::Anchor { .. });
        let zoomed = self.zoom == Some(pane.id);
        for (button, x) in buttons {
            let (icon, color) = match button {
                BarButton::Scratch if focused => (
                    if scratched {
                        SCRATCH_ICON
                    } else {
                        UNSCRATCHED_ICON
                    },
                    fg,
                ),
                BarButton::Scratch if scratched => (SCRATCH_ICON, FOREGROUND),
                BarButton::Scratch => (UNSCRATCHED_ICON, UNPINNED),
                BarButton::Pin if focused => (if pinned { PIN_ICON } else { UNPINNED_ICON }, fg),
                BarButton::Pin if pinned => (PIN_ICON, FOREGROUND),
                BarButton::Pin => (UNPINNED_ICON, UNPINNED),
                BarButton::Zoom if zoomed => (UNZOOM_ICON, fg),
                BarButton::Zoom => (ZOOM_ICON, fg),
                BarButton::Close => (CLOSE_ICON, fg),
                BarButton::Name => continue,
            };
            let color = match self.hover {
                Some((id, hovered)) if id == pane.id && hovered == button => {
                    let hover = if button == BarButton::Close {
                        CLOSE_HOVER
                    } else {
                        HOVER
                    };
                    let cell = CellRect {
                        column: x,
                        row,
                        columns: BUTTON_WIDTH,
                        rows: 1,
                    };
                    scene.fill(cell, hover);
                    FOREGROUND
                }
                _ => color,
            };
            scene.icon(x + 1, row, icon, color);
        }
    }

    pub fn pin(&mut self, id: usize, app_id: String) {
        if let Some(pane) = self.pane_mut(id) {
            pane.kind = Kind::Anchor { app_id };
        }
    }

    pub fn unpin(&mut self, id: usize) {
        if let Some(pane) = self.pane_mut(id) {
            pane.kind = Kind::Transient;
        }
    }
}

/// The row of tab labels at the top of a pane with more than one client.
fn draw_strip(scene: &mut Scene, pane: &Pane, inner: CellRect, focused: bool) {
    if pane.clients.len() < 2 || inner.is_empty() {
        return;
    }
    scene.fill(CellRect { rows: 1, ..inner }, BAR);
    let cells = tab_cells(inner.column, inner.columns, pane.clients.len());
    for (index, (client, (column, width))) in pane.clients.iter().zip(cells).enumerate() {
        let color = if index == pane.shown {
            let (bg, fg) = if focused {
                (BAR_FOCUSED, BAR_FOCUSED_TEXT)
            } else {
                (HOVER, FOREGROUND)
            };
            let cell = CellRect {
                column,
                row: inner.row,
                columns: width,
                rows: 1,
            };
            scene.fill(cell, bg);
            fg
        } else {
            DIM
        };
        if width > 2 {
            let label = client.label().unwrap_or_else(|| (index + 1).to_string());
            let label = fit(&label, width as usize - 2);
            scene.text(column + 1, inner.row, &label, color);
        }
    }
}

/// The start column and width of each of `count` tab labels sharing a row
/// `columns` wide from `column`; the last takes the columns left over.
fn tab_cells(column: u32, columns: u32, count: usize) -> impl Iterator<Item = (u32, u32)> {
    let count = count as u32;
    let width = columns / count.max(1);
    (0..count).map(move |index| {
        let start = column + index * width;
        let end = if index + 1 == count {
            column + columns
        } else {
            start + width
        };
        (start, end - start)
    })
}

fn tab_at(inner: CellRect, count: usize, column: u32) -> Option<usize> {
    tab_cells(inner.column, inner.columns, count)
        .position(|(start, width)| column >= start && column < start + width)
}

/// The cells a pane's client fills: inside the outline, below the tab
/// strip when the pane has more than one client.
fn client_area(rect: CellRect, clients: usize) -> CellRect {
    let inner = rect.inner();
    let strip = u32::from(clients > 1);
    if inner.rows <= strip {
        return CellRect::EMPTY;
    }
    CellRect {
        row: inner.row + strip,
        rows: inner.rows - strip,
        ..inner
    }
}

fn stepped(shown: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if forward {
        (shown + 1) % count
    } else {
        (shown + count - 1) % count
    }
}

fn shown_after_removal(shown: usize, removed: usize, remaining: usize) -> usize {
    if removed < shown || shown >= remaining {
        shown.saturating_sub(1)
    } else {
        shown
    }
}

/// Whether the app id is `{prefix}{pane}.t{n}`, the class of a terminal
/// decade starts as a tab of the pane.
fn is_terminal_tab(app_id: &str, prefix: &str, pane: &str) -> bool {
    app_id
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(pane))
        .and_then(|rest| rest.strip_prefix(".t"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|byte| byte.is_ascii_digit()))
}

fn draw_waiting(scene: &mut Scene, pane: &Pane, inner: CellRect, scratched: bool) {
    if inner.is_empty() || !pane.clients.is_empty() {
        return;
    }
    let centered = |text: &str| inner.column + inner.columns.saturating_sub(text.len() as u32) / 2;
    if pane.restartable() {
        scene.fill(inner, BACKGROUND);

        // the message, a blank row, then a button three rows tall
        let top = inner.row + inner.rows.saturating_sub(5) / 2;
        let text = "shell exited";
        scene.text(centered(text), top, text, FOREGROUND);
        let button = "  restart  ";
        let column = centered(button);
        let rect = CellRect {
            column,
            row: top + 2,
            columns: button.len() as u32,
            rows: 3,
        };
        scene.fill(rect, BAR_FOCUSED);
        scene.text(column, top + 3, button, BAR_FOCUSED_TEXT);
        return;
    }
    scene.fill(inner, PANE);
    let middle = inner.row + inner.rows / 2;
    if scratched {
        let hint = "agents show things here";
        scene.text(
            centered("scratch"),
            middle.saturating_sub(1),
            "scratch",
            DIM,
        );
        scene.text(centered(hint), middle, hint, UNPINNED);
        return;
    }
    let text = match &pane.kind {
        Kind::Anchor { app_id } => format!("waiting for {app_id}"),
        Kind::Transient => "no client".to_string(),
    };
    scene.text(centered(&text), middle, &text, DIM);
}

fn bar_buttons(start: u32, width: u32) -> [(BarButton, u32); 4] {
    let close = (start + width).saturating_sub(BUTTON_WIDTH);
    let zoom = close.saturating_sub(BUTTON_WIDTH);
    let pin = zoom.saturating_sub(BUTTON_WIDTH);
    let scratch = pin.saturating_sub(BUTTON_WIDTH);
    [
        (BarButton::Scratch, scratch),
        (BarButton::Pin, pin),
        (BarButton::Zoom, zoom),
        (BarButton::Close, close),
    ]
}

fn split_area(area: CellRect, axis: Axis, ratio: f32) -> (CellRect, CellRect) {
    match axis {
        Axis::Horizontal => {
            let first = ((area.columns as f32 * ratio).round() as u32).clamp(0, area.columns);
            (
                CellRect {
                    columns: first,
                    ..area
                },
                CellRect {
                    column: area.column + first,
                    columns: area.columns - first,
                    ..area
                },
            )
        }
        Axis::Vertical => {
            let first = ((area.rows as f32 * ratio).round() as u32).clamp(0, area.rows);
            (
                CellRect {
                    rows: first,
                    ..area
                },
                CellRect {
                    row: area.row + first,
                    rows: area.rows - first,
                    ..area
                },
            )
        }
    }
}

fn arrange_node(node: &Node, area: CellRect, out: &mut Vec<(usize, CellRect)>) {
    match node {
        Node::Leaf(id) => {
            let rect = if area.columns < MIN_PANE || area.rows < MIN_PANE {
                CellRect::EMPTY
            } else {
                area
            };
            out.push((*id, rect));
        }
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (a, b) = split_area(area, *axis, *ratio);
            arrange_node(first, a, out);
            arrange_node(second, b, out);
        }
    }
}

fn split_leaf(node: &mut Node, at: usize, axis: Axis, new: usize) -> bool {
    match node {
        Node::Leaf(id) if *id == at => {
            *node = Node::Split {
                axis,
                ratio: 0.5,
                first: Box::new(Node::Leaf(at)),
                second: Box::new(Node::Leaf(new)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            split_leaf(first, at, axis, new) || split_leaf(second, at, axis, new)
        }
    }
}

fn split_path(
    node: &Node,
    id: usize,
    axis: Axis,
    path: &mut Vec<bool>,
    found: &mut Option<Vec<bool>>,
) -> bool {
    let Node::Split {
        axis: here,
        first,
        second,
        ..
    } = node
    else {
        return matches!(node, Node::Leaf(leaf) if *leaf == id);
    };
    for (side, child) in [(false, first), (true, second)] {
        path.push(side);
        let inside = split_path(child, id, axis, path, found);
        path.pop();
        if inside {
            if *here == axis && found.is_none() {
                *found = Some(path.clone());
            }
            return true;
        }
    }
    false
}

fn swap_leaves(node: &mut Node, a: usize, b: usize) {
    match node {
        Node::Leaf(id) if *id == a => *id = b,
        Node::Leaf(id) if *id == b => *id = a,
        Node::Leaf(_) => {}
        Node::Split { first, second, .. } => {
            swap_leaves(first, a, b);
            swap_leaves(second, a, b);
        }
    }
}

fn drop_zones(rect: CellRect) -> [(Option<Side>, CellRect); 5] {
    let (quarter_columns, quarter_rows) = (rect.columns / 4, rect.rows / 4);
    let middle_columns = rect.columns - quarter_columns * 2;
    let middle_rows = rect.rows - quarter_rows * 2;
    let zone = |column: u32, row: u32, columns: u32, rows: u32| CellRect {
        column: rect.column + column,
        row: rect.row + row,
        columns,
        rows,
    };
    [
        (
            None,
            zone(quarter_columns, quarter_rows, middle_columns, middle_rows),
        ),
        (
            Some(Side::Left),
            zone(0, quarter_rows, quarter_columns, middle_rows),
        ),
        (
            Some(Side::Right),
            zone(
                quarter_columns + middle_columns,
                quarter_rows,
                quarter_columns,
                middle_rows,
            ),
        ),
        (
            Some(Side::Above),
            zone(quarter_columns, 0, middle_columns, quarter_rows),
        ),
        (
            Some(Side::Below),
            zone(
                quarter_columns,
                quarter_rows + middle_rows,
                middle_columns,
                quarter_rows,
            ),
        ),
    ]
}

fn draw_zone(scene: &mut Scene, zone: CellRect, active: bool) {
    if active {
        scene.fill(zone, [DROP[0], DROP[1], DROP[2], 0.35]);
    }
    scene.frame(zone, DROP);
}

fn dropped(root: Node, dragged: usize, drop: Drop) -> Node {
    match drop {
        Drop::Swap(target) => {
            let mut root = root;
            swap_leaves(&mut root, dragged, target);
            root
        }
        Drop::Split { target, side } => {
            let mut root = remove_leaf(root, dragged);
            split_beside(&mut root, target, side, dragged);
            root
        }
        Drop::Edge(side) => {
            let rest = Box::new(remove_leaf(root, dragged));
            let new = Box::new(Node::Leaf(dragged));
            let (axis, before) = side_axis(side);
            let (ratio, first, second) = if before {
                (1.0 / 3.0, new, rest)
            } else {
                (2.0 / 3.0, rest, new)
            };
            Node::Split {
                axis,
                ratio,
                first,
                second,
            }
        }
    }
}

fn side_axis(side: Side) -> (Axis, bool) {
    match side {
        Side::Left => (Axis::Horizontal, true),
        Side::Right => (Axis::Horizontal, false),
        Side::Above => (Axis::Vertical, true),
        Side::Below => (Axis::Vertical, false),
    }
}

fn split_beside(node: &mut Node, target: usize, side: Side, new: usize) -> bool {
    match node {
        Node::Leaf(id) if *id == target => {
            let (axis, before) = side_axis(side);
            let (first, second) = if before { (new, target) } else { (target, new) };
            *node = Node::Split {
                axis,
                ratio: 0.5,
                first: Box::new(Node::Leaf(first)),
                second: Box::new(Node::Leaf(second)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            split_beside(first, target, side, new) || split_beside(second, target, side, new)
        }
    }
}

fn remove_leaf(node: Node, id: usize) -> Node {
    match node {
        Node::Leaf(_) => node,
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if matches!(*first, Node::Leaf(leaf) if leaf == id) {
                return *second;
            }
            if matches!(*second, Node::Leaf(leaf) if leaf == id) {
                return *first;
            }
            Node::Split {
                axis,
                ratio,
                first: Box::new(remove_leaf(*first, id)),
                second: Box::new(remove_leaf(*second, id)),
            }
        }
    }
}

fn first_leaf(node: &Node) -> usize {
    match node {
        Node::Leaf(id) => *id,
        Node::Split { first, .. } => first_leaf(first),
    }
}

fn sibling_leaf(node: &Node, id: usize) -> Option<usize> {
    let Node::Split { first, second, .. } = node else {
        return None;
    };
    if matches!(**first, Node::Leaf(leaf) if leaf == id) {
        return Some(first_leaf(second));
    }
    if matches!(**second, Node::Leaf(leaf) if leaf == id) {
        return Some(first_leaf(first));
    }
    sibling_leaf(first, id).or_else(|| sibling_leaf(second, id))
}

fn find_boundary(
    node: &Node,
    area: CellRect,
    column: u32,
    row: u32,
    path: &mut Vec<bool>,
) -> Option<Boundary> {
    let Node::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let (a, b) = split_area(area, *axis, *ratio);
    let on_boundary = match axis {
        Axis::Horizontal => {
            let edge = b.column;
            area.rows > 0
                && row >= area.row
                && row < area.row + area.rows
                && edge >= 1
                && (column == edge || column == edge - 1)
        }
        Axis::Vertical => {
            let edge = b.row;
            area.columns > 0
                && column >= area.column
                && column < area.column + area.columns
                && edge >= 1
                && (row == edge || row == edge - 1)
        }
    };
    if on_boundary {
        return Some(Boundary {
            path: path.clone(),
            axis: *axis,
        });
    }
    path.push(false);
    if let Some(found) = find_boundary(first, a, column, row, path) {
        return Some(found);
    }
    path.pop();
    path.push(true);
    let found = find_boundary(second, b, column, row, path);
    path.pop();
    found
}

fn node_area(node: &Node, area: CellRect, path: &[bool]) -> CellRect {
    let Some((&second, rest)) = path.split_first() else {
        return area;
    };
    match node {
        Node::Leaf(_) => area,
        Node::Split {
            axis,
            ratio,
            first,
            second: second_node,
        } => {
            let (a, b) = split_area(area, *axis, *ratio);
            if second {
                node_area(second_node, b, rest)
            } else {
                node_area(first, a, rest)
            }
        }
    }
}

fn node_at<'a>(node: &'a mut Node, path: &[bool]) -> Option<&'a mut Node> {
    let Some((&second, rest)) = path.split_first() else {
        return Some(node);
    };
    match node {
        Node::Leaf(_) => None,
        Node::Split {
            first,
            second: second_node,
            ..
        } => {
            if second {
                node_at(second_node, rest)
            } else {
                node_at(first, rest)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const AREA: CellRect = CellRect {
        column: 0,
        row: 0,
        columns: 100,
        rows: 40,
    };

    fn three_anchors() -> Layout {
        let mut layout = Layout::new(AREA);
        layout.panes[0].name = "chat".to_string();
        layout.add_transient("terminal".to_string());
        layout.add_transient("code".to_string());
        for (id, name) in [(0, "chat"), (1, "terminal"), (2, "code")] {
            layout.pin(id, format!("decade.{name}"));
        }
        layout.focused = 1;
        layout
    }

    #[test]
    fn the_three_anchors_fill_the_body_above_the_status_line() {
        let layout = three_anchors();
        let rects: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        assert_eq!(
            rects[0],
            CellRect {
                column: 0,
                row: 0,
                columns: 50,
                rows: 40
            }
        );
        assert_eq!(
            rects[1],
            CellRect {
                column: 50,
                row: 0,
                columns: 50,
                rows: 20
            }
        );
        assert_eq!(
            rects[2],
            CellRect {
                column: 50,
                row: 20,
                columns: 50,
                rows: 20
            }
        );
    }

    #[test]
    fn a_transient_pane_splits_the_focused_pane_and_leaves_with_its_client() {
        let mut layout = three_anchors();
        layout.focused = 0;
        let id = layout.add_transient("terminal2".into());
        assert_eq!(layout.focused, id);
        let chat = layout.pane(0).unwrap().rect;
        let new = layout.pane(id).unwrap().rect;
        assert_eq!(chat.rows + new.rows, 40);
        assert_eq!(chat.columns, 50);
        layout.remove(id);
        assert_eq!(layout.pane(0).unwrap().rect.rows, 40);
        assert!(layout.pane(id).is_none());
        assert_eq!(layout.focused, 0);
        layout.focused = 2;
        layout.remove(2);
        assert_eq!(layout.focused, 1);
    }

    #[test]
    fn zoom_gives_one_pane_the_body_and_hides_the_rest() {
        let mut layout = three_anchors();
        layout.toggle_zoom(2);
        assert_eq!(layout.pane(2).unwrap().rect.columns, 100);
        assert_eq!(layout.visible().count(), 1);
        layout.toggle_zoom(2);
        assert_eq!(layout.visible().count(), 3);
    }

    #[test]
    fn the_boundary_between_chat_and_the_right_column_can_be_dragged() {
        let mut layout = three_anchors();
        let boundary = layout
            .boundary_at(50, 10)
            .expect("the outline cells are a boundary");
        assert_eq!(boundary.axis, Axis::Horizontal);
        layout.drag_boundary(&boundary, 30, 10);
        assert_eq!(layout.pane(0).unwrap().rect.columns, 30);
        assert_eq!(layout.pane(1).unwrap().rect.column, 30);
        assert!(layout.boundary_at(25, 10).is_none());
    }

    #[test]
    fn the_bar_covers_the_top_row_of_the_client_and_leaves_its_size_alone() {
        let mut layout = three_anchors();
        let pane = layout.pane(0).unwrap();
        let before = layout.client_rect(pane);
        layout.bar = true;
        let pane = layout.pane(0).unwrap();
        assert_eq!(layout.client_rect(pane), before);
        assert_eq!(layout.bar_hit(48, before.row), Some((0, BarButton::Close)));
        assert_eq!(layout.bar_hit(38, before.row), Some((0, BarButton::Pin)));
        assert_eq!(layout.bar_hit(48, 1), Some((0, BarButton::Close)));
        assert_eq!(layout.bar_hit(42, 1), Some((0, BarButton::Zoom)));
        let inner = layout.pane(0).unwrap().rect.inner();
        let (_, scratch) = bar_buttons(inner.column, inner.columns)[0];
        assert_eq!(scratch, 33);
        assert_eq!(layout.bar_hit(scratch, 1), Some((0, BarButton::Scratch)));
        assert_eq!(layout.bar_hit(scratch - 1, 1), Some((0, BarButton::Name)));
        assert_eq!(layout.bar_hit(2, 1), Some((0, BarButton::Name)));
        assert_eq!(layout.bar_hit(2, 5), None);
    }

    #[test]
    fn a_transient_pane_is_placed_by_the_app_id_of_the_terminal_started_for_it() {
        let mut layout = three_anchors();
        let id = layout.add_transient("terminal2".to_string());
        assert_eq!(
            layout.place(Some("decade.w0.terminal2"), "decade.w0."),
            Some((id, false))
        );
        assert_eq!(
            layout.place(Some("decade.w0.terminal3"), "decade.w0."),
            None
        );
    }

    #[test]
    fn swapping_two_panes_exchanges_their_rects_and_nothing_else() {
        let mut layout = three_anchors();
        let chat = layout.pane(0).unwrap().rect;
        let code = layout.pane(2).unwrap().rect;
        layout.apply_drop(0, Drop::Swap(2));
        assert_eq!(layout.pane(0).unwrap().rect, code);
        assert_eq!(layout.pane(2).unwrap().rect, chat);
        assert_eq!(layout.pane(0).unwrap().name, "chat");
        assert!(
            matches!(&layout.pane(2).unwrap().kind, Kind::Anchor { app_id } if app_id == "decade.code")
        );
    }

    #[test]
    fn swapping_a_pane_with_itself_changes_nothing() {
        let mut layout = three_anchors();
        let rects: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        layout.apply_drop(1, Drop::Swap(1));
        let after: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        assert_eq!(after, rects);
    }

    #[test]
    fn a_dragged_pane_leaves_its_slot_empty_and_its_ghost_is_the_inside_of_the_pane() {
        let mut layout = three_anchors();
        layout.bar = true;
        layout.dragging = Some(0);
        let font = crate::text::Font::load(14.0);
        let mut chrome = Scene::new(&font);
        layout.draw(&mut chrome, None);
        let waiting = chrome.shapes.iter().any(|shape| {
            matches!(shape, crate::draw::Shape::Text { text, .. } if text.contains("decade.chat"))
        });
        assert!(!waiting);
        let mut ghost = Scene::new(&font);
        layout.draw_ghost(0, &mut ghost, None);
        let name = ghost.shapes.iter().find_map(|shape| match shape {
            crate::draw::Shape::Text { at, text, .. } if text == "chat" => Some(*at),
            _ => None,
        });
        assert_eq!(name, Some((font.cell_width as i32, 0).into()));
    }

    #[test]
    fn a_pinned_transient_pane_waits_for_its_clients_app_id() {
        let mut layout = three_anchors();
        let id = layout.add_transient("nautilus".to_string());
        layout.pin(id, "org.gnome.Nautilus".to_string());
        assert_eq!(
            layout.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some((id, false))
        );
        assert_eq!(layout.pane(id).unwrap().name, "nautilus");
    }

    #[test]
    fn an_unpinned_anchor_is_transient() {
        let mut layout = three_anchors();
        layout.unpin(0);
        assert!(matches!(layout.pane(0).unwrap().kind, Kind::Transient));
    }

    #[test]
    fn the_middle_of_a_pane_swaps_and_its_edges_split() {
        let layout = three_anchors();
        let code = layout.pane(2).unwrap().rect;
        let middle = (code.column + code.columns / 2, code.row + code.rows / 2);
        assert_eq!(layout.drop_at(0, middle.0, middle.1), Some(Drop::Swap(2)));
        assert_eq!(
            layout.drop_at(0, code.column + 3, middle.1),
            Some(Drop::Split {
                target: 2,
                side: Side::Left
            })
        );
        assert_eq!(layout.drop_at(1, 0, 10), Some(Drop::Edge(Side::Left)));
        assert_eq!(layout.drop_at(0, code.column + 3, code.row + 3), None);
        assert_eq!(layout.drop_at(0, 20, 20), None);
    }

    #[test]
    fn splitting_a_pane_gives_the_dropped_pane_the_half_on_that_side() {
        let mut layout = three_anchors();
        let code = layout.pane(2).unwrap().rect;
        let drop = Drop::Split {
            target: 2,
            side: Side::Right,
        };
        layout.apply_drop(0, drop);
        let chat = layout.pane(0).unwrap().rect;
        assert_eq!(layout.pane(2).unwrap().rect.column, 0);
        assert!(chat.column > 0);
        assert_eq!(chat.row, layout.pane(2).unwrap().rect.row);
        assert!(chat.columns < code.columns * 2);
    }

    #[test]
    fn a_drop_on_the_window_edge_spans_that_side() {
        let mut layout = three_anchors();
        layout.apply_drop(2, Drop::Edge(Side::Left));
        let code = layout.pane(2).unwrap().rect;
        assert_eq!((code.column, code.row), (0, 0));
        assert_eq!(code.rows, 40);
        assert_eq!(code.columns, 33);
    }

    #[test]
    fn resizing_moves_the_nearest_divider_on_that_axis() {
        let mut layout = three_anchors();
        let chat = layout.pane(0).unwrap().rect.columns;
        layout.resize_toward(0, Direction::Right, 2);
        assert_eq!(layout.pane(0).unwrap().rect.columns, chat + 2);
        let terminal = layout.pane(1).unwrap().rect.rows;
        layout.resize_toward(2, Direction::Up, 2);
        assert_eq!(layout.pane(1).unwrap().rect.rows, terminal - 2);
    }

    fn texts(scene: &Scene) -> Vec<String> {
        scene
            .shapes
            .iter()
            .filter_map(|shape| match shape {
                crate::draw::Shape::Text { text, .. } => Some(text.trim().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_pinned_terminal_whose_shell_exited_shows_a_restart_button() {
        let mut layout = Layout::new(AREA);
        layout.panes[0].cwd = Some(PathBuf::from("/home/jack"));
        layout.pin(0, "decade.w0.terminal".to_string());
        let font = crate::text::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw(&mut scene, None);
        let texts = texts(&scene);
        assert!(texts.iter().any(|text| text == "shell exited"));
        assert!(texts.iter().any(|text| text == "restart"));
    }

    #[test]
    fn a_long_name_on_the_ctrl_bar_is_cut_before_the_icons() {
        let mut layout = Layout::new(AREA);
        layout.panes[0].name = "x".repeat(200);
        layout.bar = true;
        let font = crate::text::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw_bars(&mut scene, None);
        let inner = layout.panes[0].rect.inner();
        let first = bar_buttons(inner.column, inner.columns)[0].1;
        let name = texts(&scene)
            .into_iter()
            .find(|text| text.starts_with('x'))
            .unwrap();
        assert!(name.ends_with('…'));
        assert!(inner.column + 1 + name.chars().count() as u32 <= first);
    }

    #[test]
    fn a_pinned_gui_pane_without_its_window_waits_and_offers_no_restart() {
        let mut layout = Layout::new(AREA);
        layout.pin(0, "org.gnome.Nautilus".to_string());
        assert!(!layout.panes[0].restartable());
        let font = crate::text::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw(&mut scene, None);
        let texts = texts(&scene);
        assert!(
            texts
                .iter()
                .any(|text| text == "waiting for org.gnome.Nautilus")
        );
        assert!(!texts.iter().any(|text| text == "restart"));
    }

    #[test]
    fn an_empty_scratch_pane_says_it_is_the_scratch_pane() {
        let layout = Layout::new(AREA);
        let font = crate::text::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw(&mut scene, Some("terminal"));
        let texts = texts(&scene);
        assert!(texts.iter().any(|text| text == "agents show things here"));
        assert!(!texts.iter().any(|text| text == "no client"));
    }

    #[test]
    fn a_window_with_a_tab_class_goes_into_its_pane_even_when_the_pane_has_a_client() {
        let mut layout = three_anchors();
        let id = layout.add_transient("terminal2".to_string());
        assert_eq!(
            layout.place(Some("decade.w0.terminal2.t2"), "decade.w0."),
            Some((id, true))
        );
        assert_eq!(
            layout.place(Some("decade.w0.terminal.t13"), "decade.w0."),
            Some((1, true))
        );
        assert_eq!(
            layout.place(Some("decade.w0.terminal2.t"), "decade.w0."),
            None
        );
        assert_eq!(
            layout.place(Some("decade.w0.terminal3.t2"), "decade.w0."),
            None
        );
        assert_eq!(
            layout.place(Some("decade.w1.terminal2.t2"), "decade.w0."),
            None
        );
    }

    #[test]
    fn showing_the_next_or_previous_tab_wraps_at_the_ends() {
        assert_eq!(stepped(0, 3, true), 1);
        assert_eq!(stepped(2, 3, true), 0);
        assert_eq!(stepped(0, 3, false), 2);
        assert_eq!(stepped(1, 3, false), 0);
        assert_eq!(stepped(0, 1, true), 0);
        assert_eq!(stepped(0, 0, false), 0);
    }

    #[test]
    fn a_tab_whose_client_exits_hands_the_pane_to_a_neighbour() {
        // shown is the middle of three and goes: the tab after it moves
        // into its index
        assert_eq!(shown_after_removal(1, 1, 2), 1);
        // shown is the last and goes: the one before it
        assert_eq!(shown_after_removal(2, 2, 2), 1);
        // a tab before the shown one goes: the shown one keeps showing
        assert_eq!(shown_after_removal(2, 0, 2), 1);
        // a tab after the shown one goes
        assert_eq!(shown_after_removal(0, 2, 2), 0);
        assert_eq!(shown_after_removal(0, 0, 0), 0);
    }

    #[test]
    fn a_click_on_the_strip_picks_the_tab_whose_label_holds_the_column() {
        let inner = CellRect {
            column: 1,
            row: 1,
            columns: 10,
            rows: 5,
        };
        let cells: Vec<(u32, u32)> = tab_cells(inner.column, inner.columns, 3).collect();
        assert_eq!(cells, [(1, 3), (4, 3), (7, 4)]);
        assert_eq!(tab_at(inner, 3, 1), Some(0));
        assert_eq!(tab_at(inner, 3, 4), Some(1));
        assert_eq!(tab_at(inner, 3, 10), Some(2));
        assert_eq!(tab_at(inner, 3, 11), None);
        assert_eq!(tab_at(inner, 3, 0), None);
        let layout = three_anchors();
        assert_eq!(layout.tab_hit(2, 1), None);
    }

    #[test]
    fn a_pane_with_two_or_more_clients_gives_the_strip_its_top_row() {
        let rect = CellRect {
            column: 0,
            row: 0,
            columns: 20,
            rows: 10,
        };
        let one = client_area(rect, 1);
        assert_eq!(one, rect.inner());
        assert_eq!(client_area(rect, 0), one);
        let two = client_area(rect, 2);
        assert_eq!(two.row, one.row + 1);
        assert_eq!(two.rows, one.rows - 1);
        assert_eq!((two.column, two.columns), (one.column, one.columns));
        assert_eq!(client_area(rect, 3), two);
        let short = CellRect { rows: 3, ..rect };
        assert_eq!(client_area(short, 2), CellRect::EMPTY);
    }
}
