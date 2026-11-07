use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::grid::{CLEAR, Grid, Rgba};
use crate::palette;

pub const BACKGROUND: Rgba = palette::BACKGROUND;
pub const FOREGROUND: Rgba = palette::FOREGROUND;
const PANE: Rgba = palette::DARK_BACKGROUND;
const OUTLINE: Rgba = palette::SELECTION;
const FOCUS: Rgba = palette::ACCENT;
const BAR: Rgba = palette::LIGHTER_BACKGROUND;
const STATUS: Rgba = palette::LIGHTER_BACKGROUND;
const DIM: Rgba = palette::LIGHT_FOREGROUND;

const MIN_PANE: u32 = 4;
const BAR_ZOOM: &str = " zoom ";
const BAR_CLOSE: &str = " x ";

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
    Zoom,
    Close,
    Name,
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
        let mut inner = pane.rect.inner();
        if self.bar && !inner.is_empty() {
            inner.row += 1;
            inner.rows -= 1;
        }
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
                    && matches!(&pane.kind, Kind::Anchor { app_id: wanted } if wanted == app_id)
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
            let end = start + width;
            let close = end.saturating_sub(BAR_CLOSE.len() as u32);
            let zoom = close.saturating_sub(BAR_ZOOM.len() as u32);
            let button = if column >= close {
                BarButton::Close
            } else if column >= zoom {
                BarButton::Zoom
            } else {
                BarButton::Name
            };
            return Some((pane.id, button));
        }
        None
    }

    pub fn draw(&self, grid: &mut Grid) {
        for pane in self.visible() {
            let rect = pane.rect;
            let outline = if pane.id == self.focused {
                FOCUS
            } else {
                OUTLINE
            };
            outline_box(grid, rect, outline);
            let inner = rect.inner();
            if inner.is_empty() {
                continue;
            }
            if pane.toplevel.is_none() {
                grid.fill(inner.column, inner.row, inner.columns, inner.rows, PANE);
                let text = match &pane.kind {
                    Kind::Anchor { app_id } => format!("waiting for {app_id}"),
                    Kind::Transient => "no client".to_string(),
                };
                let column = inner.column + inner.columns.saturating_sub(text.len() as u32) / 2;
                grid.text(column, inner.row + inner.rows / 2, &text, DIM, PANE);
            }
            if let Some((start, bar_row, width)) = self.bar_row(pane) {
                grid.fill(start, bar_row, width, 1, BAR);
                grid.text(start + 1, bar_row, &pane.name, FOREGROUND, BAR);
                let end = start + width;
                let close = end.saturating_sub(BAR_CLOSE.len() as u32);
                let zoom = close.saturating_sub(BAR_ZOOM.len() as u32);
                grid.text(zoom, bar_row, BAR_ZOOM, FOREGROUND, BAR);
                grid.text(close, bar_row, BAR_CLOSE, FOREGROUND, BAR);
            }
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

fn outline_box(grid: &mut Grid, rect: CellRect, color: Rgba) {
    if rect.columns < 2 || rect.rows < 2 {
        return;
    }
    let right = rect.column + rect.columns - 1;
    let bottom = rect.row + rect.rows - 1;
    let horizontal: String = "─".repeat(rect.columns as usize - 2);
    grid.text(
        rect.column,
        rect.row,
        &format!("┌{horizontal}┐"),
        color,
        BACKGROUND,
    );
    grid.text(
        rect.column,
        bottom,
        &format!("└{horizontal}┘"),
        color,
        BACKGROUND,
    );
    for row in rect.row + 1..bottom {
        grid.text(rect.column, row, "│", color, BACKGROUND);
        grid.text(right, row, "│", color, BACKGROUND);
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
    fn the_bar_takes_the_top_row_of_the_client_and_has_buttons_on_the_right() {
        let mut layout = Layout::new(100, 41);
        let pane = layout.pane(0).unwrap();
        assert_eq!(layout.client_rect(pane).rows, 38);
        layout.bar = true;
        let pane = layout.pane(0).unwrap();
        assert_eq!(layout.client_rect(pane).row, 2);
        assert_eq!(layout.client_rect(pane).rows, 37);
        assert_eq!(layout.bar_hit(48, 1), Some((0, BarButton::Close)));
        assert_eq!(layout.bar_hit(42, 1), Some((0, BarButton::Zoom)));
        assert_eq!(layout.bar_hit(2, 1), Some((0, BarButton::Name)));
        assert_eq!(layout.bar_hit(2, 5), None);
    }

    #[test]
    fn names_are_made_unique_by_counting() {
        let layout = Layout::new(100, 41);
        assert_eq!(layout.unique_name("terminal"), "terminal2");
        assert_eq!(layout.unique_name("nautilus"), "nautilus");
    }
}
