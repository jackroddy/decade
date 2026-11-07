use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::grid::{CLEAR, Grid, Rgba};
use crate::palette;
use crate::text::edge_char;

pub const BACKGROUND: Rgba = palette::BACKGROUND;
pub const FOREGROUND: Rgba = palette::FOREGROUND;
const PANE: Rgba = palette::DARK_BACKGROUND;
const OUTLINE: Rgba = palette::SELECTION;
const FOCUS: Rgba = palette::ACCENT;
const DROP: Rgba = palette::CYAN;
const BAR: Rgba = palette::LIGHTER_BACKGROUND;
const BAR_FOCUSED: Rgba = palette::ACCENT;
const BAR_FOCUSED_TEXT: Rgba = palette::DARK_BACKGROUND;
const HOVER: Rgba = palette::SELECTION;
const CLOSE_HOVER: Rgba = palette::RED;
const UNPINNED: Rgba = palette::MUTED;
const STATUS: Rgba = palette::LIGHTER_BACKGROUND;
const DIM: Rgba = palette::LIGHT_FOREGROUND;

const MIN_PANE: u32 = 4;
const EDGE_BAND: u32 = 2;
const BUTTON_WIDTH: u32 = 4;
const PIN_ICON: char = '\u{f0403}';
const UNPINNED_ICON: char = '\u{f0930}';
const ZOOM_ICON: char = '\u{f0293}';
const UNZOOM_ICON: char = '\u{f0294}';
const CLOSE_ICON: char = '\u{f0156}';

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

pub struct Pane {
    pub id: usize,
    pub name: String,
    pub kind: Kind,
    pub rect: CellRect,
    pub toplevel: Option<ToplevelSurface>,
}

enum Node {
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
    pub columns: u32,
    pub rows: u32,
    pub panes: Vec<Pane>,
    pub focused: usize,
    pub zoom: Option<usize>,
    pub bar: bool,
    pub palette: bool,
    pub drag_cell: Option<(u32, u32)>,
    pub dragging: Option<usize>,
    pub hover: Option<(usize, BarButton)>,
    root: Node,
    next_id: usize,
}

impl Layout {
    pub fn new(columns: u32, rows: u32) -> Layout {
        let anchor = |id: usize, name: &str| Pane {
            id,
            name: name.to_string(),
            kind: Kind::Anchor {
                app_id: format!("decade.{name}"),
            },
            rect: CellRect::EMPTY,
            toplevel: None,
        };
        let mut layout = Layout {
            columns,
            rows,
            panes: vec![anchor(0, "chat"), anchor(1, "terminal"), anchor(2, "code")],
            focused: 1,
            zoom: None,
            bar: false,
            palette: false,
            drag_cell: None,
            dragging: None,
            hover: None,
            root: Node::Split {
                axis: Axis::Horizontal,
                ratio: 0.5,
                first: Box::new(Node::Leaf(0)),
                second: Box::new(Node::Split {
                    axis: Axis::Vertical,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(1)),
                    second: Box::new(Node::Leaf(2)),
                }),
            },
            next_id: 3,
        };
        layout.arrange();
        layout
    }

    pub fn resize(&mut self, columns: u32, rows: u32) {
        self.columns = columns;
        self.rows = rows;
        self.arrange();
    }

    fn body(&self) -> CellRect {
        CellRect {
            column: 0,
            row: 0,
            columns: self.columns,
            rows: self.rows.saturating_sub(1),
        }
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
        let inner = pane.rect.inner();
        if inner.rows < 1 {
            return CellRect::EMPTY;
        }
        inner
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

    pub fn place(&self, app_id: Option<&str>) -> Option<usize> {
        let app_id = app_id?;
        self.panes
            .iter()
            .find(|pane| {
                pane.toplevel.is_none()
                    && match &pane.kind {
                        Kind::Anchor { app_id: wanted } => wanted == app_id,
                        Kind::Transient => app_id.strip_prefix("decade.") == Some(&pane.name),
                    }
            })
            .map(|pane| pane.id)
    }

    pub fn pane_of(&self, toplevel: &ToplevelSurface) -> Option<usize> {
        self.panes
            .iter()
            .find(|pane| pane.toplevel.as_ref() == Some(toplevel))
            .map(|pane| pane.id)
    }

    pub fn unique_name(&self, base: &str) -> String {
        if self.by_name(base).is_none() {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}{n}"))
            .find(|name| self.by_name(name).is_none())
            .unwrap()
    }

    pub fn add_transient(&mut self, name: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let at = self.focused;
        let axis = match self.pane(at) {
            Some(pane) if pane.rect.columns >= pane.rect.rows * 2 => Axis::Horizontal,
            _ => Axis::Vertical,
        };
        self.panes.push(Pane {
            id,
            name,
            kind: Kind::Transient,
            rect: CellRect::EMPTY,
            toplevel: None,
        });
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
            self.panes[index].toplevel = None;
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

    pub fn draw_drop(&self, grid: &mut Grid) {
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
                draw_zone(grid, zone, active == Some(drop));
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
            draw_zone(grid, strip, active == Some(Drop::Edge(side)));
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

    pub fn draw(&self, grid: &mut Grid) {
        for pane in self.visible() {
            if self.dragging == Some(pane.id) {
                continue;
            }
            let outline = if pane.id == self.focused {
                FOCUS
            } else {
                OUTLINE
            };
            draw_pane(grid, pane, pane.rect, outline);
        }
        if self.rows == 0 {
            return;
        }
        let status_row = self.rows - 1;
        grid.fill(0, status_row, self.columns, 1, STATUS);
        let focused = self
            .pane(self.focused)
            .map(|pane| pane.name.as_str())
            .unwrap_or("");
        let zoom = if self.zoom.is_some() { "  zoom" } else { "" };
        let status = format!(" decade  {focused}{zoom}  Ctrl+Space palette");
        grid.text(0, status_row, &status, FOREGROUND, STATUS);
    }

    pub fn draw_bars(&self, grid: &mut Grid) {
        if !self.bar {
            return;
        }
        for pane in self.visible() {
            if self.dragging != Some(pane.id) {
                self.draw_bar(grid, pane, pane.rect.inner());
            }
        }
    }

    pub fn draw_ghost(&self, id: usize) -> Option<Grid> {
        let pane = self.pane(id)?;
        let inner = CellRect {
            column: 0,
            row: 0,
            ..pane.rect.inner()
        };
        let mut grid = Grid::new(inner.columns, inner.rows, FOREGROUND, CLEAR);
        draw_waiting(&mut grid, pane, inner);
        if self.bar {
            self.draw_bar(&mut grid, pane, inner);
        }
        Some(grid)
    }

    fn draw_bar(&self, grid: &mut Grid, pane: &Pane, inner: CellRect) {
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
        grid.fill(start, row, width, 1, bg);
        grid.text(start + 1, row, &pane.name, fg, bg);
        let pinned = matches!(pane.kind, Kind::Anchor { .. });
        let zoomed = self.zoom == Some(pane.id);
        for (button, x) in bar_buttons(start, width) {
            let (icon, color) = match button {
                BarButton::Pin if pinned => (PIN_ICON, if focused { fg } else { FOREGROUND }),
                BarButton::Pin => (UNPINNED_ICON, UNPINNED),
                BarButton::Zoom if zoomed => (UNZOOM_ICON, fg),
                BarButton::Zoom => (ZOOM_ICON, fg),
                BarButton::Close => (CLOSE_ICON, fg),
                BarButton::Name => continue,
            };
            let (color, bg) = match self.hover {
                Some((id, hovered)) if id == pane.id && hovered == button => {
                    let hover = if button == BarButton::Close {
                        CLOSE_HOVER
                    } else {
                        HOVER
                    };
                    (FOREGROUND, hover)
                }
                _ => (color, bg),
            };
            grid.fill(x, row, BUTTON_WIDTH, 1, bg);
            grid.icon(x + 1, row, icon, color, bg);
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

    pub fn draw_overlay(&self, grid: &mut Grid) {
        if !self.palette {
            return;
        }
        let body = self.body();
        let width = body.columns.min(60);
        let height = 5;
        if body.columns < 8 || body.rows < height {
            return;
        }
        let column = (body.columns - width) / 2;
        let row = (body.rows - height) / 2;
        grid.fill(column, row, width, height, PANE);
        outline_box(
            grid,
            CellRect {
                column,
                row,
                columns: width,
                rows: height,
            },
            FOCUS,
            BACKGROUND,
        );
        grid.text(column + 2, row + 1, "command palette", FOREGROUND, PANE);
        grid.text(
            column + 2,
            row + 3,
            "nothing here yet. Esc closes.",
            DIM,
            PANE,
        );
    }
}

fn draw_pane(grid: &mut Grid, pane: &Pane, rect: CellRect, outline: Rgba) {
    outline_box(grid, rect, outline, BACKGROUND);
    draw_waiting(grid, pane, rect.inner());
}

fn draw_waiting(grid: &mut Grid, pane: &Pane, inner: CellRect) {
    if inner.is_empty() || pane.toplevel.is_some() {
        return;
    }
    grid.fill(inner.column, inner.row, inner.columns, inner.rows, PANE);
    let text = match &pane.kind {
        Kind::Anchor { app_id } => format!("waiting for {app_id}"),
        Kind::Transient => "no client".to_string(),
    };
    let column = inner.column + inner.columns.saturating_sub(text.len() as u32) / 2;
    grid.text(column, inner.row + inner.rows / 2, &text, DIM, PANE);
}

fn bar_buttons(start: u32, width: u32) -> [(BarButton, u32); 3] {
    let close = (start + width).saturating_sub(BUTTON_WIDTH);
    let zoom = close.saturating_sub(BUTTON_WIDTH);
    let pin = zoom.saturating_sub(BUTTON_WIDTH);
    [
        (BarButton::Pin, pin),
        (BarButton::Zoom, zoom),
        (BarButton::Close, close),
    ]
}

fn outline_box(grid: &mut Grid, rect: CellRect, color: Rgba, bg: Rgba) {
    if rect.columns < 2 || rect.rows < 2 {
        return;
    }
    let right = rect.column + rect.columns - 1;
    let bottom = rect.row + rect.rows - 1;
    let horizontal: String = "─".repeat(rect.columns as usize - 2);
    grid.text(rect.column, rect.row, &format!("┌{horizontal}┐"), color, bg);
    grid.text(rect.column, bottom, &format!("└{horizontal}┘"), color, bg);
    for row in rect.row + 1..bottom {
        grid.text(rect.column, row, "│", color, bg);
        grid.text(right, row, "│", color, bg);
    }
}

pub fn overlay_grid(columns: u32, rows: u32) -> Grid {
    Grid::new(columns, rows, FOREGROUND, CLEAR)
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

fn draw_zone(grid: &mut Grid, zone: CellRect, active: bool) {
    let bg = if active {
        [DROP[0], DROP[1], DROP[2], 0.35]
    } else {
        CLEAR
    };
    if active {
        grid.fill(zone.column, zone.row, zone.columns, zone.rows, bg);
    }
    let (right, bottom) = (zone.column + zone.columns, zone.row + zone.rows);
    for row in zone.row..bottom {
        for column in zone.column..right {
            let sides = (
                column == zone.column,
                column + 1 == right,
                row == zone.row,
                row + 1 == bottom,
            );
            if sides.0 || sides.1 || sides.2 || sides.3 {
                let edge = edge_char(sides.0, sides.1, sides.2, sides.3);
                grid.text(column, row, &edge.to_string(), DROP, bg);
            }
        }
    }
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
mod tests {
    use super::*;

    #[test]
    fn the_three_anchors_fill_the_body_above_the_status_line() {
        let layout = Layout::new(100, 41);
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
        let mut layout = Layout::new(100, 41);
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
        let mut layout = Layout::new(100, 41);
        layout.toggle_zoom(2);
        assert_eq!(layout.pane(2).unwrap().rect.columns, 100);
        assert_eq!(layout.visible().count(), 1);
        layout.toggle_zoom(2);
        assert_eq!(layout.visible().count(), 3);
    }

    #[test]
    fn the_boundary_between_chat_and_the_right_column_can_be_dragged() {
        let mut layout = Layout::new(100, 41);
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
        let mut layout = Layout::new(100, 41);
        let pane = layout.pane(0).unwrap();
        let before = layout.client_rect(pane);
        layout.bar = true;
        let pane = layout.pane(0).unwrap();
        assert_eq!(layout.client_rect(pane), before);
        assert_eq!(layout.bar_hit(48, before.row), Some((0, BarButton::Close)));
        assert_eq!(layout.bar_hit(38, before.row), Some((0, BarButton::Pin)));
        assert_eq!(layout.bar_hit(48, 1), Some((0, BarButton::Close)));
        assert_eq!(layout.bar_hit(42, 1), Some((0, BarButton::Zoom)));
        assert_eq!(layout.bar_hit(2, 1), Some((0, BarButton::Name)));
        assert_eq!(layout.bar_hit(2, 5), None);
    }

    #[test]
    fn a_transient_pane_is_placed_by_the_app_id_of_the_terminal_started_for_it() {
        let mut layout = Layout::new(100, 41);
        let name = layout.unique_name("terminal");
        let id = layout.add_transient(name);
        assert_eq!(layout.place(Some("decade.terminal2")), Some(id));
        assert_eq!(layout.place(Some("decade.terminal3")), None);
    }

    #[test]
    fn swapping_two_panes_exchanges_their_rects_and_nothing_else() {
        let mut layout = Layout::new(100, 41);
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
        let mut layout = Layout::new(100, 41);
        let rects: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        layout.apply_drop(1, Drop::Swap(1));
        let after: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        assert_eq!(after, rects);
    }

    #[test]
    fn a_dragged_pane_leaves_its_slot_empty_and_its_ghost_is_the_inside_of_the_pane() {
        let mut layout = Layout::new(100, 41);
        layout.bar = true;
        let rect = layout.pane(0).unwrap().rect;
        layout.dragging = Some(0);
        let mut chrome = Grid::new(100, 41, FOREGROUND, BACKGROUND);
        layout.draw(&mut chrome);
        assert!(chrome.cells[(rect.row * 100 + rect.column) as usize].ch != '┌');
        let ghost = layout.draw_ghost(0).unwrap();
        let inner = rect.inner();
        assert_eq!((ghost.columns, ghost.rows), (inner.columns, inner.rows));
        assert_eq!(ghost.cells[1].ch, 'c');
    }

    #[test]
    fn a_pinned_transient_pane_waits_for_its_clients_app_id() {
        let mut layout = Layout::new(100, 41);
        let id = layout.add_transient("nautilus".to_string());
        layout.pin(id, "org.gnome.Nautilus".to_string());
        assert_eq!(layout.place(Some("org.gnome.Nautilus")), Some(id));
        assert_eq!(layout.pane(id).unwrap().name, "nautilus");
    }

    #[test]
    fn an_unpinned_anchor_is_transient() {
        let mut layout = Layout::new(100, 41);
        layout.unpin(0);
        assert!(matches!(layout.pane(0).unwrap().kind, Kind::Transient));
    }

    #[test]
    fn a_bar_icon_takes_two_cells() {
        let mut layout = Layout::new(100, 41);
        layout.bar = true;
        let rect = layout.pane(0).unwrap().rect;
        let mut grid = Grid::new(100, 41, FOREGROUND, CLEAR);
        layout.draw_bars(&mut grid);
        let row = rect.inner().row;
        let close = (row * 100 + 46) as usize;
        assert_eq!(grid.cells[close].ch, CLOSE_ICON);
        assert!(!grid.cells[close].right_half);
        assert_eq!(grid.cells[close + 1].ch, CLOSE_ICON);
        assert!(grid.cells[close + 1].right_half);
    }

    #[test]
    fn the_middle_of_a_pane_swaps_and_its_edges_split() {
        let layout = Layout::new(100, 41);
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
        let mut layout = Layout::new(100, 41);
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
        let mut layout = Layout::new(100, 41);
        layout.apply_drop(2, Drop::Edge(Side::Left));
        let code = layout.pane(2).unwrap().rect;
        assert_eq!((code.column, code.row), (0, 0));
        assert_eq!(code.rows, 40);
        assert_eq!(code.columns, 33);
    }

    #[test]
    fn names_are_made_unique_by_counting() {
        let layout = Layout::new(100, 41);
        assert_eq!(layout.unique_name("terminal"), "terminal2");
        assert_eq!(layout.unique_name("nautilus"), "nautilus");
    }
}
