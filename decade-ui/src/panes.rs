use std::time::Instant;

use crate::palette::{self, Rgba};
use crate::{CellRect, Face, Point, Scene, Shape, Size, fit};

const MIN_PANE: u32 = 4;
const EDGE_BAND: u32 = 2;
const BUTTON_WIDTH: u32 = 4;
const ZOOM_ICON: char = '\u{f0293}';
const UNZOOM_ICON: char = '\u{f0294}';
const CLOSE_ICON: char = '\u{f0156}';

// the sizes below are in pixels at 18 px rows, for `Scene::scaled`
const PILL_RADIUS: i32 = 4;
const BUTTON_INSET: i32 = 2;
const TAG_PADDING: i32 = 4;
const TAG_INSET: i32 = 2;
const TAG_GAP: i32 = 4;

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

/// The id the program gives what one of a pane's tabs shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContentId(pub u64);

/// One of a pane's tabs.
pub struct Tab {
    pub content: ContentId,

    /// The text on the tab's pill.
    pub label: String,
}

pub struct Pane<A> {
    pub id: usize,
    pub name: String,

    /// The names the pane had before it was renamed, which still reach it.
    pub aliases: Vec<String>,
    pub rect: CellRect,
    pub tabs: Vec<Tab>,

    /// The index in `tabs` of the tab drawn.
    pub shown: usize,
    pub marks: Marks<A>,
}

impl<A> Pane<A> {
    /// Whether `name` is the pane's name or one it had before.
    pub fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|alias| alias == name)
    }

    /// The pane's name, then the names it had before.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }

    /// Give the pane the name `name`, keeping its current name as an alias.
    pub fn rename(&mut self, name: String) {
        self.aliases.retain(|alias| *alias != name);
        let old = std::mem::replace(&mut self.name, name);
        self.aliases.push(old);
    }

    /// The cells the pane's client fills.
    pub fn client_rect(&self) -> CellRect {
        client_area(self.rect, self.tabs.len())
    }

    /// The start column and width of each tab's pill on the strip.
    fn tab_pills(&self, inner: CellRect) -> Vec<(u32, u32)> {
        let lengths: Vec<usize> = (self.tabs.iter())
            .map(|tab| tab.label.chars().count())
            .collect();
        tab_pills(inner.column, inner.columns, &lengths)
    }

    /// What the tab drawn shows.
    pub fn content(&self) -> Option<ContentId> {
        self.tabs.get(self.shown).map(|tab| tab.content)
    }

    /// Put `tab` at `index` among the tabs and show it.
    pub fn add_tab(&mut self, index: usize, tab: Tab) {
        self.tabs.insert(index, tab);
        self.shown = index;
    }

    /// Show the tab at `index`, and return whether the pane has one.
    pub fn show(&mut self, index: usize) -> bool {
        let shown = index < self.tabs.len();
        if shown {
            self.shown = index;
        }
        shown
    }

    /// Show the next tab, or the previous one, wrapping at the ends.
    pub fn step_tab(&mut self, forward: bool) {
        self.shown = stepped(self.shown, self.tabs.len(), forward);
    }

    /// Remove the tab at `index`, showing the tab after it, or the one
    /// before it when it was the last.
    pub fn remove_tab(&mut self, index: usize) -> Tab {
        let removed = self.tabs.remove(index);
        self.shown = shown_after_removal(self.shown, index, self.tabs.len());
        removed
    }

    pub fn new(id: usize, name: String) -> Pane<A> {
        Pane {
            id,
            name,
            aliases: Vec::new(),
            rect: CellRect::EMPTY,
            tabs: Vec::new(),
            shown: 0,
            marks: Marks::default(),
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

/// What a pane's bar holds under a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarButton<A> {
    Name,
    Zoom,
    Close,

    /// One of the program's buttons, by its action.
    Action(A),
}

/// What the program shows on a pane's outline, bar and placeholder.
pub struct Marks<A> {
    /// The program's buttons on the bar, left to right, before zoom and
    /// close.
    pub buttons: Vec<Button<A>>,

    /// The icon tagged at the left of the outline's top edge.
    pub left_tag: Option<char>,

    /// The tags at the right of the outline's top edge, from the corner in.
    pub tags: Vec<Tag>,
    pub placeholder: Placeholder<A>,
}

impl<A> Default for Marks<A> {
    fn default() -> Marks<A> {
        Marks {
            buttons: Vec::new(),
            left_tag: None,
            tags: Vec::new(),
            placeholder: Placeholder {
                lines: Vec::new(),
                button: None,
            },
        }
    }
}

/// A button the program puts on a pane's bar.
pub struct Button<A> {
    pub icon: char,

    /// Whether the button is on, which draws it bright on an unfocused pane.
    pub on: bool,
    pub action: A,
}

/// A tag on the top edge of a pane's outline.
pub enum Tag {
    Icon(char),
    Text(String),

    /// The place of the zoom tag, drawn while the pane is zoomed.
    Zoom,
}

/// What a pane with no tabs shows.
pub struct Placeholder<A> {
    /// The lines of text. Without a button, the first is brighter than the
    /// rest.
    pub lines: Vec<String>,

    /// The label and action of a button below the lines.
    pub button: Option<(String, A)>,
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

/// A drag the layout follows from a press until the release.
pub enum Drag {
    Boundary(Boundary),
    Pane(PaneDrag),
}

/// A pane being dragged to a new place.
pub struct PaneDrag {
    pub pane: usize,

    /// Where the pane was grabbed, in pixels from its top left corner.
    pub grab: Point,
    pub start: Instant,

    /// Where the press was, in pixels.
    origin: (f64, f64),

    /// Whether the pointer has moved far enough for the pane to be dragged.
    moved: bool,

    /// The cell under the pointer since the drag began.
    cell: Option<(u32, u32)>,
}

/// What a left press on the layout asks of the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Press<A> {
    /// The press is on none of the layout's panes.
    Miss,
    Close(usize),
    Zoom(usize),

    /// A program's button on the bar or on an empty pane's placeholder.
    Action(usize, A),

    /// The pane and index of the tab pressed.
    Tab(usize, usize),

    /// A boundary drag began.
    Boundary,

    /// A drag of the pane began, and the pane is to take focus.
    Drag(usize),

    /// The body of the pane, where the press goes on to its content.
    Body(usize),
}

/// What the program does after the layout followed the pointer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Update {
    Nothing,
    Redraw,

    /// The panes' rects changed.
    Relayout,
}

/// What is drawn over the layout on screen besides its panes.
#[derive(Clone, Copy)]
pub struct Chrome<'a, A> {
    /// Whether the Ctrl bar is shown.
    pub bar: bool,

    /// The bar button under the pointer.
    pub hover: Option<(usize, BarButton<A>)>,

    /// The pane and index of the tab under the pointer.
    pub tab_hover: Option<(usize, usize)>,

    /// The pane whose outline pulses to draw the eye.
    pub pulse: Option<Pulse<'a>>,

    /// The pane whose left tag flashes, and how bright, from 0 to 1.
    pub flash: Option<(usize, f32)>,

    /// The panes being recorded.
    pub recordings: &'a [Recording<'a>],

    /// The pane an agent's input is playing in.
    pub driven: Option<Driven<'a>>,
}

impl<A> Default for Chrome<'_, A> {
    fn default() -> Self {
        Chrome {
            bar: false,
            hover: None,
            tab_hover: None,
            pulse: None,
            flash: None,
            recordings: &[],
            driven: None,
        }
    }
}

/// A pane being recorded: its outline pulses red, and a red tag after its
/// left tag holds a blinking icon and the time the recording has run.
#[derive(Clone, Copy)]
pub struct Recording<'a> {
    pub pane: usize,

    /// How far the outline's pulse is toward red, from 0 to 1.
    pub strength: f32,

    /// The icon, drawn while `lit`.
    pub icon: char,
    pub lit: bool,

    /// The time the recording has run, such as `0:07`.
    pub elapsed: &'a str,
}

/// A pane an agent's input is playing in: its outline pulses yellow, and
/// a yellow tag after its other left tags holds an icon and a label.
#[derive(Clone, Copy)]
pub struct Driven<'a> {
    pub pane: usize,

    /// How far the outline's pulse is toward yellow, from 0 to 1.
    pub strength: f32,
    pub icon: char,
    pub label: &'a str,
}

/// A pane whose outline pulses to draw the eye.
#[derive(Clone, Copy)]
pub struct Pulse<'a> {
    pub pane: usize,

    /// How far the pulse is toward its peak, from 0 to 1.
    pub strength: f32,

    /// The text of a tag on the outline beside the pane's own tags.
    pub note: Option<&'a str>,
}

pub struct Layout<A> {
    /// The cells the panes share.
    pub area: CellRect,
    pub panes: Vec<Pane<A>>,
    pub focused: usize,
    pub zoom: Option<usize>,
    pub drag: Option<Drag>,
    root: Node,
    next_id: usize,
}

impl<A: Copy + PartialEq> Layout<A> {
    /// A layout holding one empty pane with this name.
    pub fn new(area: CellRect, name: String) -> Layout<A> {
        Layout::restore(area, vec![Pane::new(0, name)], Node::Leaf(0), 0, None)
    }

    pub fn restore(
        area: CellRect,
        panes: Vec<Pane<A>>,
        root: Node,
        focused: usize,
        zoom: Option<usize>,
    ) -> Layout<A> {
        let next_id = panes.iter().map(|pane| pane.id + 1).max().unwrap_or(0);
        let mut layout = Layout {
            area,
            panes,
            focused,
            zoom,
            drag: None,
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

    fn arrange(&mut self) {
        for pane in &mut self.panes {
            pane.rect = CellRect::EMPTY;
        }
        if let Some(zoom) = self.zoom {
            if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == zoom) {
                pane.rect = self.area;
                return;
            }
            self.zoom = None;
        }
        let mut rects = Vec::new();
        arrange_node(&self.root, self.area, &mut rects);
        for (id, rect) in rects {
            if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) {
                pane.rect = rect;
            }
        }
    }

    pub fn pane(&self, id: usize) -> Option<&Pane<A>> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    pub fn pane_mut(&mut self, id: usize) -> Option<&mut Pane<A>> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }

    pub fn by_name(&self, name: &str) -> Option<usize> {
        self.panes
            .iter()
            .find(|pane| pane.answers_to(name))
            .map(|pane| pane.id)
    }

    pub fn visible(&self) -> impl Iterator<Item = &Pane<A>> {
        self.panes.iter().filter(|pane| !pane.rect.is_empty())
    }

    pub fn pane_at(&self, column: u32, row: u32) -> Option<usize> {
        self.visible()
            .find(|pane| pane.rect.contains(column, row))
            .map(|pane| pane.id)
    }

    /// The pane whose outline or tab strip is under the cell, or the empty
    /// pane the cell is in.
    pub fn chrome_at(&self, column: u32, row: u32) -> Option<usize> {
        let pane = self
            .visible()
            .find(|pane| pane.rect.contains(column, row))?;
        let on_client = !pane.tabs.is_empty() && pane.client_rect().contains(column, row);
        (!on_client).then_some(pane.id)
    }

    /// The pane that pointer motion onto the cell gives focus, with the
    /// Ctrl bar shown when `bar` is set, or `None` when the motion leaves
    /// focus where it is.
    pub fn hover_focus(&self, column: u32, row: u32, bar: bool) -> Option<usize> {
        if self.drag.is_some() || (bar && self.bar_hit(column, row).is_some()) {
            return None;
        }
        self.pane_at(column, row).filter(|&id| id != self.focused)
    }

    /// The middle of the focused pane in pixels, for a pointer on the cell
    /// to move to, or `None` when the cell is on the focused pane already.
    pub fn pointer_target(&self, column: u32, row: u32, cell: Size) -> Option<(f64, f64)> {
        if self.pane_at(column, row) == Some(self.focused) {
            return None;
        }
        let (x, y) = self.pane(self.focused)?.rect.center();
        Some((
            (x * i64::from(cell.width)) as f64 / 2.0,
            (y * i64::from(cell.height)) as f64 / 2.0,
        ))
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
        let Some((ratio, area)) = split_at(&mut self.root, self.area, &path) else {
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

    #[cfg(test)]
    pub fn add_transient(&mut self, name: String) -> usize {
        self.add_beside(self.focused, name)
    }

    /// Split pane `at` to add a pane named `name`, focus the new pane, and
    /// return its id.
    pub fn add_beside(&mut self, at: usize, name: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let side = match self.pane(at) {
            Some(pane) if pane.rect.columns >= pane.rect.rows * 2 => Side::Right,
            _ => Side::Below,
        };
        self.panes.push(Pane::new(id, name));
        split_beside(&mut self.root, at, side, id);
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
            self.panes[index].tabs.clear();
            self.panes[index].shown = 0;
            return;
        }
        self.panes.remove(index);
        let root = std::mem::replace(&mut self.root, Node::Leaf(usize::MAX));
        let (root, heir) = remove_leaf(root, id);
        self.root = root;
        if self.zoom == Some(id) {
            self.zoom = None;
        }
        if self.dragged().is_some_and(|drag| drag.pane == id) {
            self.drag = None;
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

    fn boundary_at(&self, column: u32, row: u32) -> Option<Boundary> {
        if self.zoom.is_some() {
            return None;
        }
        let mut path = Vec::new();
        find_boundary(&self.root, self.area, column, row, &mut path)
    }

    fn drop_at(&self, dragged: usize, column: u32, row: u32) -> Option<Drop> {
        if self.panes.len() < 2 || self.area.columns <= EDGE_BAND * 2 {
            return None;
        }
        if let Some((side, _)) = edge_strips(self.area)
            .into_iter()
            .find(|(_, strip)| strip.contains(column, row))
        {
            return Some(Drop::Edge(side));
        }
        let target = self.pane_at(column, row).filter(|&id| id != dragged)?;
        drop_zones(target, self.pane(target)?.rect)
            .into_iter()
            .find(|(_, zone)| zone.contains(column, row))
            .map(|(drop, _)| drop)
    }

    pub fn apply_drop(&mut self, dragged: usize, drop: Drop) {
        let root = std::mem::replace(&mut self.root, Node::Leaf(dragged));
        self.root = dropped(root, dragged, drop);
        self.zoom = None;
        self.arrange();
    }

    /// Start what a left press at `at`, in pixels, begins on cells `cell` in
    /// size, with the Ctrl bar shown when `bar` is set.
    pub fn press(&mut self, at: (f64, f64), cell: Size, bar: bool, ctrl: bool) -> Press<A> {
        let (column, row) = cell_at(at, cell);
        let bar = bar.then(|| self.bar_hit(column, row)).flatten();
        match bar {
            Some((id, BarButton::Close)) => return Press::Close(id),
            Some((id, BarButton::Zoom)) => return Press::Zoom(id),
            Some((id, BarButton::Action(action))) => return Press::Action(id, action),
            Some((_, BarButton::Name)) => {}
            None => {
                if let Some((id, index)) = self.tab_hit(column, row) {
                    return Press::Tab(id, index);
                }
            }
        }
        if let Some(boundary) = self.boundary_at(column, row) {
            self.drag = Some(Drag::Boundary(boundary));
            return Press::Boundary;
        }
        let Some(pane) = self.visible().find(|pane| pane.rect.contains(column, row)) else {
            return Press::Miss;
        };
        let id = pane.id;
        if ctrl || bar.is_some() {
            let grab = Point {
                x: at.0.round() as i32 - pane.rect.column as i32 * cell.width,
                y: at.1.round() as i32 - pane.rect.row as i32 * cell.height,
            };
            self.drag = Some(Drag::Pane(PaneDrag {
                pane: id,
                grab,
                start: Instant::now(),
                origin: at,
                moved: ctrl,
                cell: None,
            }));
            return Press::Drag(id);
        }
        if let Some(action) = self.placeholder_action(id) {
            return Press::Action(id, action);
        }
        Press::Body(id)
    }

    /// Follow the drag to the pointer at `at`, or return `None` when there
    /// is no drag.
    pub fn motion(&mut self, at: (f64, f64), cell: Size) -> Option<Update> {
        let (column, row) = cell_at(at, cell);
        match self.drag.take()? {
            Drag::Boundary(boundary) => {
                self.drag_boundary(&boundary, column, row);
                self.drag = Some(Drag::Boundary(boundary));
                Some(Update::Relayout)
            }
            Drag::Pane(mut drag) => {
                let threshold = f64::from(cell.height / 2);
                if !drag.moved
                    && ((at.0 - drag.origin.0).abs() >= threshold
                        || (at.1 - drag.origin.1).abs() >= threshold)
                {
                    drag.moved = true;
                    drag.start = Instant::now();
                }
                let update = if drag.moved {
                    drag.cell = Some((column, row));
                    Update::Redraw
                } else {
                    Update::Nothing
                };
                self.drag = Some(Drag::Pane(drag));
                Some(update)
            }
        }
    }

    /// End the drag, dropping a dragged pane where it was last moved to, or
    /// return `None` when there is no drag.
    pub fn release(&mut self) -> Option<Update> {
        let Drag::Pane(PaneDrag { pane, cell, .. }) = self.drag.take()? else {
            return Some(Update::Nothing);
        };
        match cell.and_then(|(column, row)| self.drop_at(pane, column, row)) {
            Some(drop) => {
                self.apply_drop(pane, drop);
                Some(Update::Relayout)
            }
            None => Some(Update::Redraw),
        }
    }

    /// The pane being dragged, drawn as a ghost rather than in its slot.
    /// A press on a pane's name is not a drag until the pointer moves.
    pub fn dragged(&self) -> Option<&PaneDrag> {
        match &self.drag {
            Some(Drag::Pane(drag)) if drag.moved => Some(drag),
            _ => None,
        }
    }

    /// Draw the places the pane being dragged could drop.
    pub fn draw_drop(&self, scene: &mut Scene) {
        let Some(&PaneDrag {
            pane: dragged,
            cell: Some((column, row)),
            ..
        }) = self.dragged()
        else {
            return;
        };
        let active = self.drop_at(dragged, column, row);
        if let Some(target) = self.pane_at(column, row).filter(|&id| id != dragged)
            && let Some(pane) = self.pane(target)
        {
            for (drop, zone) in drop_zones(target, pane.rect) {
                draw_zone(scene, zone, active == Some(drop));
            }
        }
        for (side, strip) in edge_strips(self.area) {
            draw_zone(scene, strip, active == Some(Drop::Edge(side)));
        }
    }

    fn drag_boundary(&mut self, boundary: &Boundary, column: u32, row: u32) {
        let Some((ratio, area)) = split_at(&mut self.root, self.area, &boundary.path) else {
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

    /// Where pane `id`'s name starts on its bar, as a column and row, and
    /// the cells the name may take.
    pub fn name_cell(&self, id: usize) -> Option<(u32, u32, u32)> {
        let pane = self.pane(id)?;
        let inner = pane.rect.inner();
        if inner.is_empty() {
            return None;
        }
        let buttons = bar_buttons(pane, inner.column, inner.columns);
        let room = buttons[0].1.saturating_sub(inner.column + 2);
        Some((inner.column + 1, inner.row, room))
    }

    /// The pane and Ctrl bar button under the cell, were the bar shown.
    pub fn bar_hit(&self, column: u32, row: u32) -> Option<(usize, BarButton<A>)> {
        for pane in self.visible() {
            let inner = pane.rect.inner();
            let (start, width) = (inner.column, inner.columns);
            if inner.is_empty() || row != inner.row || column < start || column >= start + width {
                continue;
            }
            let button = bar_buttons(pane, start, width)
                .into_iter()
                .find(|&(_, x)| column >= x && column < x + BUTTON_WIDTH)
                .map_or(BarButton::Name, |(button, _)| button);
            return Some((pane.id, button));
        }
        None
    }

    /// The action of the button on pane `id`'s placeholder, which a press
    /// anywhere on the pane runs while it has no tabs.
    pub fn placeholder_action(&self, id: usize) -> Option<A> {
        let pane = self.pane(id).filter(|pane| pane.tabs.is_empty())?;
        pane.marks
            .placeholder
            .button
            .as_ref()
            .map(|&(_, action)| action)
    }

    /// The pane and tab index of the tab label under the cell.
    pub fn tab_hit(&self, column: u32, row: u32) -> Option<(usize, usize)> {
        self.visible()
            .filter(|pane| pane.tabs.len() > 1)
            .find_map(|pane| {
                let inner = pane.rect.inner();
                if inner.is_empty() || row != inner.row {
                    return None;
                }
                let index = tab_at(&pane.tab_pills(inner), column)?;
                Some((pane.id, index))
            })
    }

    /// Draw every visible pane's outline, its placeholder when it is empty,
    /// and the place of its shown tab's content.
    pub fn draw(&self, scene: &mut Scene, chrome: Chrome<A>) {
        let dragged = self.dragged().map(|drag| drag.pane);
        for pane in self.visible() {
            if dragged == Some(pane.id) {
                continue;
            }
            let outline = if pane.id == self.focused {
                palette::get().cyan
            } else {
                palette::get().muted
            };
            let pulse = chrome.pulse.filter(|pulse| pulse.pane == pane.id);
            let flash = (chrome.flash)
                .filter(|&(id, _)| id == pane.id)
                .map_or(0.0, |(_, flash)| flash);
            let recording = (chrome.recordings.iter()).find(|recording| recording.pane == pane.id);
            let driven = chrome.driven.filter(|driven| driven.pane == pane.id);
            self.draw_outline(scene, pane, outline, flash, pulse, (recording, driven));
            draw_placeholder(scene, pane, pane.rect.inner());
            let hover = (chrome.tab_hover)
                .filter(|&(id, _)| id == pane.id)
                .map(|(_, index)| index);
            draw_strip(
                scene,
                pane,
                pane.rect.inner(),
                pane.id == self.focused,
                hover,
            );
            draw_content(scene, pane, (0, 0));
        }
    }

    /// The pane's outline with its tags on its top edge, the left one
    /// brightened by `flash`, pulsing with its note when `pulse` is at it,
    /// red with the recording's tag while `recording` is at it, and yellow
    /// with the driven tag while `driven` is at it.
    fn draw_outline(
        &self,
        scene: &mut Scene,
        pane: &Pane<A>,
        color: Rgba,
        flash: f32,
        pulse: Option<Pulse>,
        (recording, driven): (Option<&Recording>, Option<Driven>),
    ) {
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
        let strength = pulse.map_or(0.0, |pulse| pulse.strength);
        let color = palette::mix(color, palette::get().magenta, strength);
        let red = recording.map_or(0.0, |recording| recording.strength);
        let color = palette::mix(color, palette::get().red, red);
        let yellow = driven.map_or(0.0, |driven| driven.strength);
        let color = palette::mix(color, palette::get().yellow, yellow);
        scene.outline(rect, [left, top, right, bottom], color);
        if pulse.is_some() {
            let [r, g, b, _] = palette::get().magenta;
            let inner = [left + line, top + line, right + line, bottom + line];
            scene.outline(rect, inner, [r, g, b, strength]);
        }

        let (x, y) = scene.px(rect.column, rect.row);
        let (width, _) = scene.px(rect.columns, 0);
        let line_y = y + top;
        let text_y = line_y + line / 2 - cell_height / 2;
        let (padding, radius) = (scene.scaled(TAG_PADDING), scene.scaled(PILL_RADIUS));
        let height = cell_height - 2 * scene.scaled(TAG_INSET);
        let pill_y = line_y + line / 2 - height / 2;

        // the left tag takes its room first, so the
        // tags on the right are dropped before it is
        let mut limit = x + left + cell_width;
        let left_tag = x + left + cell_width * 2;
        let left_end = left_tag + cell_width;
        let left_fits = left_end + padding < x + width - right - cell_width * 2;
        if let Some(icon) = pane.marks.left_tag
            && left_fits
        {
            let size = (cell_width + padding * 2, height);
            let fill = palette::mix(color, palette::get().foreground, flash);
            scene.rounded(left_tag - padding, pill_y, size, radius, fill, fill);
            let icon = icon.to_string();
            scene.text_px(
                left_tag,
                text_y,
                &icon,
                Face::Icon,
                palette::get().dark_background,
            );
            limit = left_end + padding + scene.scaled(TAG_GAP);
        }
        if let Some(recording) = recording {
            let start = limit + padding;
            let cells = 2 + recording.elapsed.chars().count() as i32;
            let end = start + cells * cell_width;
            if end + padding < x + width - right - cell_width * 2 {
                let size = (end - start + padding * 2, height);
                let red = palette::get().red;
                scene.rounded(start - padding, pill_y, size, radius, red, red);
                let ink = palette::get().dark_background;
                if recording.lit {
                    let icon = recording.icon.to_string();
                    scene.text_px(start, text_y, &icon, Face::Icon, ink);
                }
                let time = start + cell_width * 2;
                scene.text_px(time, text_y, recording.elapsed, Face::Chrome, ink);
                limit = end + padding + scene.scaled(TAG_GAP);
            }
        }
        if let Some(driven) = driven {
            let start = limit + padding;
            let cells = 2 + driven.label.chars().count() as i32;
            let end = start + cells * cell_width;
            if end + padding < x + width - right - cell_width * 2 {
                let size = (end - start + padding * 2, height);
                let yellow = palette::get().yellow;
                scene.rounded(start - padding, pill_y, size, radius, yellow, yellow);
                let ink = palette::get().dark_background;
                let icon = driven.icon.to_string();
                scene.text_px(start, text_y, &icon, Face::Icon, ink);
                let label = start + cell_width * 2;
                scene.text_px(label, text_y, driven.label, Face::Chrome, ink);
                limit = end + padding + scene.scaled(TAG_GAP);
            }
        }
        let mut end = x + width - right - cell_width * 2;
        let zoomed = self.zoom == Some(pane.id);
        for (text, face) in pane.marks.tags.iter().filter_map(|tag| match tag {
            Tag::Icon(icon) => Some((icon.to_string(), Face::Icon)),
            Tag::Text(text) => Some((text.clone(), Face::Chrome)),
            Tag::Zoom => zoomed.then(|| ("zoom".to_string(), Face::Chrome)),
        }) {
            let start = end - text.chars().count() as i32 * cell_width;
            if start - cell_width < limit {
                break;
            }
            let size = (end - start + padding * 2, height);
            scene.rounded(start - padding, pill_y, size, radius, color, color);
            scene.text_px(start, text_y, &text, face, palette::get().dark_background);
            end = start - padding * 2 - scene.scaled(TAG_GAP);
        }
        let Some(note) = pulse.and_then(|pulse| pulse.note) else {
            return;
        };
        let room = (end - limit - cell_width) / cell_width;
        if room < 2 {
            return;
        }
        let note = fit(note, room as usize);
        let start = end - note.chars().count() as i32 * cell_width;
        let size = (end - start + padding * 2, height);
        scene.rounded(
            start - padding,
            pill_y,
            size,
            radius,
            palette::get().magenta,
            palette::get().magenta,
        );
        scene.text_px(
            start,
            text_y,
            &note,
            Face::Chrome,
            palette::get().dark_background,
        );
    }

    pub fn draw_bars(&self, scene: &mut Scene, chrome: Chrome<A>) {
        if !chrome.bar {
            return;
        }
        let dragged = self.dragged().map(|drag| drag.pane);
        for pane in self.visible() {
            if dragged != Some(pane.id) {
                self.draw_bar(scene, pane, pane.rect.inner(), chrome);
            }
        }
    }

    pub fn draw_ghost(&self, id: usize, scene: &mut Scene, chrome: Chrome<A>) {
        let Some(pane) = self.pane(id) else {
            return;
        };
        let inner = CellRect {
            column: 0,
            row: 0,
            ..pane.rect.inner()
        };
        draw_content(scene, pane, (pane.rect.column + 1, pane.rect.row + 1));
        draw_placeholder(scene, pane, inner);
        draw_strip(scene, pane, inner, pane.id == self.focused, None);
        if chrome.bar {
            self.draw_bar(scene, pane, inner, chrome);
        }
    }

    fn draw_bar(&self, scene: &mut Scene, pane: &Pane<A>, inner: CellRect, chrome: Chrome<A>) {
        if inner.is_empty() {
            return;
        }
        let (start, row, width) = (inner.column, inner.row, inner.columns);
        let focused = pane.id == self.focused;
        let (fg, face) = if focused {
            (palette::get().foreground, Face::ChromeBold)
        } else {
            (palette::get().light_foreground, Face::Chrome)
        };
        scene.fill(
            CellRect { rows: 1, ..inner },
            palette::get().lighter_background,
        );
        let buttons = bar_buttons(pane, start, width);
        let room = buttons[0].1.saturating_sub(start + 2) as usize;
        let (name_x, y) = scene.px(start + 1, row);
        scene.text_px(name_x, y, &fit(&pane.name, room), face, fg);
        let zoomed = self.zoom == Some(pane.id);
        for (index, (button, x)) in buttons.into_iter().enumerate() {
            let (icon, color) = match button {
                BarButton::Action(_) => {
                    let Button { icon, on, .. } = pane.marks.buttons[index];
                    (
                        icon,
                        if focused || on {
                            palette::get().foreground
                        } else {
                            palette::get().muted
                        },
                    )
                }
                BarButton::Zoom if zoomed => (UNZOOM_ICON, fg),
                BarButton::Zoom => (ZOOM_ICON, fg),
                BarButton::Close => (CLOSE_ICON, fg),
                BarButton::Name => continue,
            };
            let hovered = chrome.hover == Some((pane.id, button));
            let color = match button {
                BarButton::Close if hovered => palette::get().light_red,
                _ if hovered => palette::get().foreground,
                _ => color,
            };
            if hovered {
                let background = if button == BarButton::Close {
                    palette::get().dark_red
                } else {
                    palette::get().selection
                };
                draw_button_hover(scene, x + 1, row, background);
            }
            scene.icon(x + 1, row, icon, color);
        }
    }
}

/// The column and row of the cell under `at`, in pixels, on cells `cell` in
/// size.
fn cell_at(at: (f64, f64), cell: Size) -> (u32, u32) {
    (
        (at.0.max(0.0) / cell.width as f64) as u32,
        (at.1.max(0.0) / cell.height as f64) as u32,
    )
}

/// The row of tab pills at the top of a pane with more than one client,
/// with the tab at `hover` under the pointer.
fn draw_strip<A>(
    scene: &mut Scene,
    pane: &Pane<A>,
    inner: CellRect,
    focused: bool,
    hover: Option<usize>,
) {
    if pane.tabs.len() < 2 || inner.is_empty() {
        return;
    }
    scene.fill(
        CellRect { rows: 1, ..inner },
        palette::get().lighter_background,
    );
    let pills = pane.tab_pills(inner);
    let (cell_width, cell_height) = scene.cell();
    let radius = scene.scaled(PILL_RADIUS);
    for (index, (tab, (column, width))) in pane.tabs.iter().zip(pills).enumerate() {
        let shown = index == pane.shown;
        let (x, y) = scene.px(column, inner.row);
        if shown || hover == Some(index) {
            let size = (width as i32 * cell_width, cell_height);
            scene.rounded(
                x,
                y,
                size,
                radius,
                palette::get().selection,
                palette::get().selection,
            );
        }
        let (color, face) = match (shown, focused) {
            (true, true) => (palette::get().foreground, Face::ChromeBold),
            (true, false) => (palette::get().foreground, Face::Chrome),
            (false, _) => (palette::get().light_foreground, Face::Chrome),
        };
        if width > 2 {
            let label = fit(&tab.label, width as usize - 2);
            scene.text_px(x + cell_width, y, &label, face, color);
        }
    }
}

/// A rounded square behind the bar icon in the cell at `column`.
fn draw_button_hover(scene: &mut Scene, column: u32, row: u32, color: Rgba) {
    let (cell_width, cell_height) = scene.cell();
    let (x, y) = scene.px(column, row);
    let inset = scene.scaled(BUTTON_INSET);
    let side = cell_height - 2 * inset;
    let left = x + cell_width / 2 - side / 2;
    let radius = scene.scaled(PILL_RADIUS);
    scene.rounded(left, y + inset, (side, side), radius, color, color);
}

/// The start column and width of each tab pill on a strip `columns` wide
/// from `column`, for labels `lengths` characters long: each label with a
/// column either side and a column between pills, every label cut to the
/// longest length that fits. When no length fits, the tabs share the strip
/// in equal cells.
fn tab_pills(column: u32, columns: u32, lengths: &[usize]) -> Vec<(u32, u32)> {
    let gaps = (lengths.len() as u32).saturating_sub(1);
    let width = |length: usize, cut: u32| (length as u32).min(cut) + 2;
    let fits = |cut: u32| lengths.iter().map(|&n| width(n, cut)).sum::<u32>() + gaps <= columns;
    let longest = lengths.iter().copied().max().unwrap_or(0) as u32;
    let Some(cut) = (1..=longest.max(1)).rev().find(|&cut| fits(cut)) else {
        return tab_cells(column, columns, lengths.len()).collect();
    };
    let mut start = column;
    (lengths.iter())
        .map(|&length| {
            let pill = (start, width(length, cut));
            start += pill.1 + 1;
            pill
        })
        .collect()
}

/// The index of the pill among `pills` that holds `column`.
fn tab_at(pills: &[(u32, u32)], column: u32) -> Option<usize> {
    (pills.iter()).position(|&(start, width)| column >= start && column < start + width)
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

/// Where the pane's shown tab is drawn, in pixels from the cell `origin`.
fn draw_content<A>(scene: &mut Scene, pane: &Pane<A>, origin: (u32, u32)) {
    let Some(id) = pane.content() else {
        return;
    };
    let rect = pane.client_rect();
    let (x, y) = scene.px(rect.column, rect.row);
    let (left, top) = scene.px(origin.0, origin.1);
    let (width, height) = scene.px(rect.columns, rect.rows);
    scene.shapes.push(Shape::Content {
        id,
        at: Point {
            x: x - left,
            y: y - top,
        },
        size: Size { width, height },
    });
}

/// The placeholder of a pane with no tabs: its lines in the middle, or
/// with a button, its lines above the button.
fn draw_placeholder<A>(scene: &mut Scene, pane: &Pane<A>, inner: CellRect) {
    if inner.is_empty() || !pane.tabs.is_empty() {
        return;
    }
    let Placeholder { lines, button } = &pane.marks.placeholder;
    let count = lines.len() as u32;
    let centered = |text: &str| inner.column + inner.columns.saturating_sub(text.len() as u32) / 2;
    if let Some((label, _)) = button {
        scene.fill(inner, palette::get().background);

        // the lines, a blank row, then a button three rows tall
        let top = inner.row + inner.rows.saturating_sub(count + 4) / 2;
        for (row, line) in (top..).zip(lines) {
            scene.text(centered(line), row, line, palette::get().foreground);
        }
        let button = format!("  {label}  ");
        let column = centered(&button);
        let rect = CellRect {
            column,
            row: top + count + 1,
            columns: button.len() as u32,
            rows: 3,
        };
        scene.fill(rect, palette::get().accent);
        scene.text(
            column,
            top + count + 2,
            &button,
            palette::get().dark_background,
        );
        return;
    }
    scene.fill(inner, palette::get().dark_background);
    let top = (inner.row + inner.rows / 2).saturating_sub(count.saturating_sub(1));
    for (index, line) in lines.iter().enumerate() {
        let color = if index == 0 {
            palette::get().light_foreground
        } else {
            palette::get().muted
        };
        scene.text(centered(line), top + index as u32, line, color);
    }
}

/// Each button on the pane's bar `width` wide from `start`, left to right,
/// and its column: the program's buttons, then zoom and close.
fn bar_buttons<A: Copy>(pane: &Pane<A>, start: u32, width: u32) -> Vec<(BarButton<A>, u32)> {
    let close = (start + width).saturating_sub(BUTTON_WIDTH);
    let zoom = close.saturating_sub(BUTTON_WIDTH);
    let mut column = zoom;
    let mut buttons: Vec<_> = (pane.marks.buttons.iter().rev())
        .map(|button| {
            column = column.saturating_sub(BUTTON_WIDTH);
            (BarButton::Action(button.action), column)
        })
        .collect();
    buttons.reverse();
    buttons.extend([(BarButton::Zoom, zoom), (BarButton::Close, close)]);
    buttons
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

fn drop_zones(target: usize, rect: CellRect) -> [(Drop, CellRect); 5] {
    let (quarter_columns, quarter_rows) = (rect.columns / 4, rect.rows / 4);
    let middle_columns = rect.columns - quarter_columns * 2;
    let middle_rows = rect.rows - quarter_rows * 2;
    let zone = |column: u32, row: u32, columns: u32, rows: u32| CellRect {
        column: rect.column + column,
        row: rect.row + row,
        columns,
        rows,
    };
    let split = |side| Drop::Split { target, side };
    [
        (
            Drop::Swap(target),
            zone(quarter_columns, quarter_rows, middle_columns, middle_rows),
        ),
        (
            split(Side::Left),
            zone(0, quarter_rows, quarter_columns, middle_rows),
        ),
        (
            split(Side::Right),
            zone(
                quarter_columns + middle_columns,
                quarter_rows,
                quarter_columns,
                middle_rows,
            ),
        ),
        (
            split(Side::Above),
            zone(quarter_columns, 0, middle_columns, quarter_rows),
        ),
        (
            split(Side::Below),
            zone(
                quarter_columns,
                quarter_rows + middle_rows,
                middle_columns,
                quarter_rows,
            ),
        ),
    ]
}

/// The bands along the edges of the area where a dropped pane spans that
/// side of every other pane.
fn edge_strips(area: CellRect) -> [(Side, CellRect); 4] {
    [
        (
            Side::Left,
            CellRect {
                columns: EDGE_BAND,
                ..area
            },
        ),
        (
            Side::Right,
            CellRect {
                column: area.column + area.columns.saturating_sub(EDGE_BAND),
                columns: EDGE_BAND,
                ..area
            },
        ),
        (
            Side::Above,
            CellRect {
                rows: EDGE_BAND,
                ..area
            },
        ),
        (
            Side::Below,
            CellRect {
                row: area.row + area.rows.saturating_sub(EDGE_BAND),
                rows: EDGE_BAND,
                ..area
            },
        ),
    ]
}

fn draw_zone(scene: &mut Scene, zone: CellRect, active: bool) {
    if active {
        scene.fill(
            zone,
            [
                palette::get().accent[0],
                palette::get().accent[1],
                palette::get().accent[2],
                0.35,
            ],
        );
    }
    scene.frame(zone, palette::get().accent);
}

fn dropped(root: Node, dragged: usize, drop: Drop) -> Node {
    match drop {
        Drop::Swap(target) => {
            let mut root = root;
            swap_leaves(&mut root, dragged, target);
            root
        }
        Drop::Split { target, side } => {
            let (mut root, _) = remove_leaf(root, dragged);
            split_beside(&mut root, target, side, dragged);
            root
        }
        Drop::Edge(side) => {
            let rest = Box::new(remove_leaf(root, dragged).0);
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

/// The tree without the leaf, and the first leaf of the sibling that took
/// its place.
fn remove_leaf(node: Node, id: usize) -> (Node, Option<usize>) {
    let Node::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return (node, None);
    };
    if matches!(*first, Node::Leaf(leaf) if leaf == id) {
        let heir = first_leaf(&second);
        return (*second, Some(heir));
    }
    if matches!(*second, Node::Leaf(leaf) if leaf == id) {
        let heir = first_leaf(&first);
        return (*first, Some(heir));
    }
    let (first, in_first) = remove_leaf(*first, id);
    let (second, in_second) = remove_leaf(*second, id);
    let split = Node::Split {
        axis,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    };
    (split, in_first.or(in_second))
}

fn first_leaf(node: &Node) -> usize {
    match node {
        Node::Leaf(id) => *id,
        Node::Split { first, .. } => first_leaf(first),
    }
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

/// The ratio of the split at the end of the path, and the cells it divides.
fn split_at<'a>(
    node: &'a mut Node,
    area: CellRect,
    path: &[bool],
) -> Option<(&'a mut f32, CellRect)> {
    let Node::Split {
        axis,
        ratio,
        first,
        second,
    } = node
    else {
        return None;
    };
    let Some((&in_second, rest)) = path.split_first() else {
        return Some((ratio, area));
    };
    let (a, b) = split_area(area, *axis, *ratio);
    if in_second {
        split_at(second, b, rest)
    } else {
        split_at(first, a, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: CellRect = CellRect {
        column: 0,
        row: 0,
        columns: 100,
        rows: 40,
    };

    type Layout = super::Layout<u8>;

    fn three_anchors<A: Copy + PartialEq>() -> super::Layout<A> {
        let mut layout = super::Layout::new(AREA, "chat".to_string());
        layout.add_transient("terminal".to_string());
        layout.add_transient("code".to_string());
        layout.focused = 1;
        layout
    }

    const CELL: Size = Size {
        width: 10,
        height: 20,
    };

    /// The middle of the cell, in pixels.
    fn px(column: u32, row: u32) -> (f64, f64) {
        (
            (column as i32 * CELL.width + CELL.width / 2) as f64,
            (row as i32 * CELL.height + CELL.height / 2) as f64,
        )
    }

    fn pane_drag(pane: usize, cell: Option<(u32, u32)>) -> Drag {
        Drag::Pane(PaneDrag {
            pane,
            grab: Point { x: 0, y: 0 },
            start: Instant::now(),
            origin: (0.0, 0.0),
            moved: true,
            cell,
        })
    }

    /// Three buttons, with actions 0, 1 and 2, the second one on.
    fn three_buttons() -> Vec<Button<u8>> {
        (0..3)
            .map(|action| Button {
                icon: char::from(b'a' + action),
                on: action == 1,
                action,
            })
            .collect()
    }

    #[test]
    fn pointer_motion_focuses_the_pane_under_it_bar_included_but_not_during_a_drag() {
        let mut layout: Layout = three_anchors();
        assert_eq!(layout.hover_focus(10, 10, false), Some(0));
        assert_eq!(layout.hover_focus(10, 0, false), Some(0));
        assert_eq!(layout.hover_focus(60, 25, false), Some(2));
        assert_eq!(layout.hover_focus(60, 5, false), None);
        assert_eq!(layout.hover_focus(200, 5, false), None);
        layout.drag = Some(pane_drag(1, None));
        assert_eq!(layout.hover_focus(10, 10, false), None);
        layout.drag = layout.boundary_at(50, 10).map(Drag::Boundary);
        assert!(layout.drag.is_some());
        assert_eq!(layout.hover_focus(10, 10, false), None);
    }

    #[test]
    fn pointer_motion_over_a_shown_ctrl_bar_leaves_focus_alone() {
        let layout: Layout = three_anchors();
        let inner = layout.pane(0).unwrap().rect.inner();
        assert_eq!(layout.hover_focus(10, inner.row, true), None);
        assert_eq!(layout.hover_focus(10, inner.row, false), Some(0));
        assert_eq!(layout.hover_focus(10, inner.row + 1, true), Some(0));
    }

    #[test]
    fn the_pointer_target_is_the_focused_panes_middle_unless_the_pointer_is_on_it() {
        let mut layout: Layout = three_anchors();
        assert_eq!(layout.pointer_target(10, 10, CELL), Some((750.0, 200.0)));
        assert_eq!(layout.pointer_target(60, 5, CELL), None);
        layout.focused = 0;
        assert_eq!(layout.pointer_target(60, 5, CELL), Some((250.0, 400.0)));
        assert_eq!(layout.pointer_target(200, 50, CELL), Some((250.0, 400.0)));
    }

    #[test]
    fn the_three_anchors_fill_the_body_above_the_status_line() {
        let layout: Layout = three_anchors();
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
        let mut layout: Layout = three_anchors();
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
    fn removing_the_focused_pane_focuses_the_first_pane_of_its_sibling() {
        let mut layout: Layout = three_anchors();
        layout.focused = 0;
        layout.remove(0);
        assert_eq!(layout.focused, 1);
        let mut layout: Layout = three_anchors();
        layout.focused = 1;
        layout.remove(1);
        assert_eq!(layout.focused, 2);
        let mut layout: Layout = three_anchors();
        layout.focused = 2;
        layout.remove(1);
        assert_eq!(layout.focused, 2);
    }

    #[test]
    fn a_new_pane_beside_a_wide_pane_goes_right_and_beside_a_tall_one_below() {
        let mut layout: Layout = three_anchors();
        layout.focused = 1;
        let wide = layout.pane(1).unwrap().rect;
        let right = layout.add_transient("right".to_string());
        let rect = layout.pane(right).unwrap().rect;
        assert_eq!(
            (rect.column, rect.row),
            (wide.column + wide.columns / 2, wide.row)
        );
        layout.focused = 0;
        let tall = layout.pane(0).unwrap().rect;
        let below = layout.add_transient("below".to_string());
        let rect = layout.pane(below).unwrap().rect;
        assert_eq!(
            (rect.column, rect.row),
            (tall.column, tall.row + tall.rows / 2)
        );
    }

    #[test]
    fn zoom_gives_one_pane_the_body_and_hides_the_rest() {
        let mut layout: Layout = three_anchors();
        layout.toggle_zoom(2);
        assert_eq!(layout.pane(2).unwrap().rect.columns, 100);
        assert_eq!(layout.visible().count(), 1);
        layout.toggle_zoom(2);
        assert_eq!(layout.visible().count(), 3);
    }

    #[test]
    fn the_boundary_between_chat_and_the_right_column_can_be_dragged() {
        let mut layout: Layout = three_anchors();
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
    fn a_press_on_a_boundary_drags_it_and_each_motion_changes_the_layout() {
        let mut layout: Layout = three_anchors();
        assert_eq!(
            layout.press(px(50, 10), CELL, false, false),
            Press::Boundary
        );
        assert_eq!(layout.motion(px(30, 10), CELL), Some(Update::Relayout));
        assert_eq!(layout.pane(0).unwrap().rect.columns, 30);
        assert_eq!(layout.pane(1).unwrap().rect.column, 30);
        assert_eq!(layout.release(), Some(Update::Nothing));
        assert_eq!(layout.motion(px(40, 10), CELL), None);
        assert_eq!(layout.release(), None);
    }

    #[test]
    fn a_ctrl_press_on_a_pane_body_drags_the_pane_from_where_it_was_grabbed() {
        let mut layout: Layout = three_anchors();
        let code = layout.pane(2).unwrap().rect;
        let at = px(code.column + 5, code.row + 5);
        assert_eq!(
            layout.press(at, CELL, false, false),
            Press::Body(2),
            "without Ctrl the press reaches the pane's body"
        );
        assert!(layout.dragged().is_none());
        assert_eq!(layout.press(at, CELL, false, true), Press::Drag(2));
        let drag = layout.dragged().unwrap();
        assert_eq!(drag.pane, 2);
        assert_eq!(drag.grab, Point { x: 55, y: 110 });
        assert_eq!(layout.focused, 1, "the program focuses the dragged pane");
    }

    #[test]
    fn a_press_on_a_pane_name_in_the_ctrl_bar_drags_the_pane() {
        let mut layout: Layout = three_anchors();
        assert_eq!(layout.press(px(2, 1), CELL, true, false), Press::Drag(0));
    }

    #[test]
    fn a_press_on_a_pane_name_is_not_a_drag_until_the_pointer_moves_a_few_pixels() {
        let mut layout: Layout = three_anchors();
        let at = px(2, 1);
        assert_eq!(layout.press(at, CELL, true, false), Press::Drag(0));
        assert!(layout.dragged().is_none());
        let near = (at.0 + 1.0, at.1);
        assert_eq!(layout.motion(near, CELL), Some(Update::Nothing));
        assert!(layout.dragged().is_none());
        let far = (at.0 + f64::from(CELL.height), at.1);
        assert_eq!(layout.motion(far, CELL), Some(Update::Redraw));
        assert_eq!(layout.dragged().unwrap().pane, 0);
    }

    #[test]
    fn a_click_on_a_pane_name_drops_nothing() {
        let mut layout: Layout = three_anchors();
        let before = layout.pane(0).unwrap().rect;
        layout.press(px(2, 1), CELL, true, false);
        layout.motion(px(2, 1), CELL);
        assert_eq!(layout.release(), Some(Update::Redraw));
        assert_eq!(layout.pane(0).unwrap().rect, before);
        assert!(layout.dragged().is_none());
    }

    #[test]
    fn a_release_drops_the_dragged_pane_where_drop_at_says() {
        let mut layout: Layout = three_anchors();
        let chat = layout.pane(0).unwrap().rect;
        let code = layout.pane(2).unwrap().rect;
        let middle = (code.column + code.columns / 2, code.row + code.rows / 2);
        assert_eq!(layout.press(px(5, 5), CELL, false, true), Press::Drag(0));
        assert_eq!(
            layout.release(),
            Some(Update::Redraw),
            "a pane never moved drops nowhere"
        );

        layout.press(px(5, 5), CELL, false, true);
        assert_eq!(
            layout.motion(px(middle.0, middle.1), CELL),
            Some(Update::Redraw)
        );
        assert_eq!(layout.release(), Some(Update::Relayout));
        assert_eq!(layout.pane(0).unwrap().rect, code);
        assert_eq!(layout.pane(2).unwrap().rect, chat);
        assert!(layout.drag.is_none());

        let chat = layout.pane(0).unwrap().rect;
        assert_eq!(layout.press(px(5, 5), CELL, false, true), Press::Drag(2));
        layout.motion(px(chat.column + 3, chat.row + chat.rows / 2), CELL);
        assert_eq!(layout.release(), Some(Update::Relayout));
        let code = layout.pane(2).unwrap().rect;
        let chat = layout.pane(0).unwrap().rect;
        assert_eq!(code.row, chat.row);
        assert_eq!(code.column + code.columns, chat.column);
    }

    #[test]
    fn removing_the_dragged_pane_ends_the_drag_so_its_release_drops_nothing() {
        let mut layout: Layout = three_anchors();
        let code = layout.pane(2).unwrap().rect;
        assert_eq!(layout.press(px(5, 5), CELL, false, true), Press::Drag(0));
        layout.motion(
            px(code.column + code.columns / 2, code.row + code.rows / 2),
            CELL,
        );
        layout.remove(0);
        assert!(layout.drag.is_none());
        assert_eq!(layout.release(), None);
        assert!(layout.pane(0).is_none());
        assert_eq!(layout.panes.len(), 2);
    }

    #[test]
    fn a_press_on_a_bar_button_returns_what_the_button_does() {
        let mut layout: Layout = three_anchors();
        layout.panes[0].marks.buttons = three_buttons();
        assert_eq!(layout.press(px(48, 1), CELL, true, false), Press::Close(0));
        assert_eq!(layout.press(px(42, 1), CELL, true, false), Press::Zoom(0));
        assert_eq!(
            layout.press(px(33, 1), CELL, true, false),
            Press::Action(0, 1)
        );
        assert_eq!(
            layout.press(px(48, 1), CELL, false, false),
            Press::Body(0),
            "without the bar the cell is the pane's"
        );
        assert!(layout.drag.is_none());
    }

    #[test]
    fn a_press_on_an_empty_pane_runs_its_placeholder_button() {
        let mut layout: Layout = three_anchors();
        layout.panes[1].marks.placeholder.button = Some(("restart".to_string(), 7));
        let terminal = layout.pane(1).unwrap().rect;
        let at = px(terminal.column + 10, terminal.row + 10);
        assert_eq!(layout.press(at, CELL, false, false), Press::Action(1, 7));
        assert_eq!(layout.press(at, CELL, false, true), Press::Drag(1));
    }

    #[test]
    fn the_ctrl_bar_buttons_are_hit_on_the_top_row_of_the_client() {
        let mut layout: Layout = three_anchors();
        layout.panes[0].marks.buttons = three_buttons();
        let pane = layout.pane(0).unwrap();
        let before = pane.client_rect();
        assert_eq!(layout.bar_hit(48, before.row), Some((0, BarButton::Close)));
        assert_eq!(
            layout.bar_hit(38, before.row),
            Some((0, BarButton::Action(2)))
        );
        assert_eq!(layout.bar_hit(48, 1), Some((0, BarButton::Close)));
        assert_eq!(layout.bar_hit(42, 1), Some((0, BarButton::Zoom)));
        let inner = pane.rect.inner();
        let (button, middle) = bar_buttons(pane, inner.column, inner.columns)[1];
        assert_eq!((button, middle), (BarButton::Action(1), 33));
        assert_eq!(layout.bar_hit(middle, 1), Some((0, BarButton::Action(1))));
        assert_eq!(
            layout.bar_hit(middle - 1, 1),
            Some((0, BarButton::Action(0)))
        );
        assert_eq!(layout.bar_hit(middle - 5, 1), Some((0, BarButton::Name)));
        assert_eq!(layout.bar_hit(2, 1), Some((0, BarButton::Name)));
        assert_eq!(layout.bar_hit(2, 5), None);
    }

    #[test]
    fn a_pane_without_buttons_of_its_own_has_its_name_up_to_zoom() {
        let layout: Layout = three_anchors();
        let inner = layout.pane(1).unwrap().rect.inner();
        let zoom = inner.column + inner.columns - 2 * BUTTON_WIDTH;
        assert_eq!(layout.bar_hit(zoom, inner.row), Some((1, BarButton::Zoom)));
        assert_eq!(
            layout.bar_hit(zoom - 1, inner.row),
            Some((1, BarButton::Name))
        );
        assert_eq!(layout.name_cell(1).unwrap().2, zoom - inner.column - 2);
    }

    #[test]
    fn swapping_two_panes_exchanges_their_rects_and_nothing_else() {
        let mut layout: Layout = three_anchors();
        let chat = layout.pane(0).unwrap().rect;
        let code = layout.pane(2).unwrap().rect;
        layout.apply_drop(0, Drop::Swap(2));
        assert_eq!(layout.pane(0).unwrap().rect, code);
        assert_eq!(layout.pane(2).unwrap().rect, chat);
        assert_eq!(layout.pane(0).unwrap().name, "chat");
        assert_eq!(layout.pane(2).unwrap().name, "code");
    }

    #[test]
    fn swapping_a_pane_with_itself_changes_nothing() {
        let mut layout: Layout = three_anchors();
        let rects: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        layout.apply_drop(1, Drop::Swap(1));
        let after: Vec<CellRect> = layout.panes.iter().map(|pane| pane.rect).collect();
        assert_eq!(after, rects);
    }

    #[test]
    fn a_dragged_pane_leaves_its_slot_empty_and_its_ghost_is_the_inside_of_the_pane() {
        let mut layout: Layout = three_anchors();
        layout.panes[0].marks.placeholder.lines = vec!["waiting".to_string()];
        layout.drag = Some(pane_drag(0, None));
        let chrome = Chrome {
            bar: true,
            ..Chrome::default()
        };
        let font = crate::Font::load(14.0);
        let mut drawn = Scene::new(&font);
        layout.draw(&mut drawn, chrome);
        let waiting = drawn
            .shapes
            .iter()
            .any(|shape| matches!(shape, crate::Shape::Text { text, .. } if text == "waiting"));
        assert!(!waiting);
        let mut ghost = Scene::new(&font);
        layout.draw_ghost(0, &mut ghost, chrome);
        let name = ghost.shapes.iter().find_map(|shape| match shape {
            crate::Shape::Text { at, text, .. } if text == "chat" => Some(*at),
            _ => None,
        });
        assert_eq!(
            name,
            Some(crate::Point {
                x: font.cell_width as i32,
                y: 0
            })
        );
    }

    #[test]
    fn the_middle_of_a_pane_swaps_and_its_edges_split() {
        let layout: Layout = three_anchors();
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
    fn the_edge_strips_are_hit_where_they_are_drawn_when_the_area_is_inset() {
        let area = CellRect {
            column: 24,
            row: 1,
            columns: 96,
            rows: 40,
        };
        let mut layout = Layout::new(area, "terminal".to_string());
        layout.add_transient("code".to_string());
        assert_eq!(layout.drop_at(1, 24, 20), Some(Drop::Edge(Side::Left)));
        assert_eq!(layout.drop_at(1, 0, 20), None);
        assert_eq!(layout.drop_at(1, 119, 20), Some(Drop::Edge(Side::Right)));
        assert_ne!(layout.drop_at(1, 94, 20), Some(Drop::Edge(Side::Right)));
        assert_eq!(layout.drop_at(1, 30, 1), Some(Drop::Edge(Side::Above)));
        assert_eq!(layout.drop_at(1, 30, 40), Some(Drop::Edge(Side::Below)));

        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.drag = Some(pane_drag(1, Some((24, 20))));
        layout.draw_drop(&mut scene);
        let active = scene.shapes.iter().find_map(|shape| match shape {
            crate::Shape::Rect { at, color, .. } if color[3] < 1.0 => Some(*at),
            _ => None,
        });
        assert_eq!(
            active,
            Some(crate::Point {
                x: 24 * font.cell_width as i32,
                y: font.cell_height as i32,
            })
        );
    }

    #[test]
    fn splitting_a_pane_gives_the_dropped_pane_the_half_on_that_side() {
        let mut layout: Layout = three_anchors();
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
        let mut layout: Layout = three_anchors();
        layout.apply_drop(2, Drop::Edge(Side::Left));
        let code = layout.pane(2).unwrap().rect;
        assert_eq!((code.column, code.row), (0, 0));
        assert_eq!(code.rows, 40);
        assert_eq!(code.columns, 33);
    }

    #[test]
    fn resizing_moves_the_nearest_divider_on_that_axis() {
        let mut layout: Layout = three_anchors();
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
                crate::Shape::Text { text, .. } => Some(text.trim().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_long_name_on_the_ctrl_bar_is_cut_before_the_icons() {
        let mut layout = Layout::new(AREA, "terminal".to_string());
        layout.panes[0].name = "x".repeat(200);
        layout.panes[0].marks.buttons = three_buttons();
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        let chrome = Chrome {
            bar: true,
            ..Chrome::default()
        };
        layout.draw_bars(&mut scene, chrome);
        let inner = layout.panes[0].rect.inner();
        let first = bar_buttons(&layout.panes[0], inner.column, inner.columns)[0].1;
        let name = texts(&scene)
            .into_iter()
            .find(|text| text.starts_with('x'))
            .unwrap();
        assert!(name.ends_with('…'));
        assert!(inner.column + 1 + name.chars().count() as u32 <= first);
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
    fn tab_pills_are_as_wide_as_their_labels_and_a_click_between_them_picks_none() {
        let pills = tab_pills(1, 40, &[4, 3, 6]);
        assert_eq!(pills, [(1, 6), (8, 5), (14, 8)]);
        assert_eq!(tab_at(&pills, 0), None);
        assert_eq!(tab_at(&pills, 1), Some(0));
        assert_eq!(tab_at(&pills, 6), Some(0));
        assert_eq!(tab_at(&pills, 7), None);
        assert_eq!(tab_at(&pills, 8), Some(1));
        assert_eq!(tab_at(&pills, 21), Some(2));
        assert_eq!(tab_at(&pills, 22), None);
        let layout: Layout = three_anchors();
        assert_eq!(layout.tab_hit(2, 1), None);
    }

    #[test]
    fn tab_labels_are_cut_to_the_longest_length_that_fits_every_pill() {
        let pills = tab_pills(0, 20, &[10, 3, 10]);
        assert_eq!(pills, [(0, 6), (7, 5), (13, 6)]);
        let (start, width) = pills[2];
        assert!(start + width <= 20);
    }

    #[test]
    fn tabs_too_many_for_a_pill_each_share_the_strip_in_equal_cells() {
        let pills = tab_pills(1, 10, &[5, 5, 5]);
        assert_eq!(pills, [(1, 3), (4, 3), (7, 4)]);
        assert_eq!(tab_at(&pills, 10), Some(2));
    }

    fn rounded(scene: &Scene) -> Vec<(i32, i32, i32, i32, Rgba)> {
        (scene.shapes.iter())
            .filter_map(|shape| match shape {
                crate::Shape::Rounded { at, size, fill, .. } => {
                    Some((at.x, at.y, size.width, size.height, *fill))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_hovered_close_button_is_a_dark_red_square_inside_its_cell() {
        let layout = Layout::new(AREA, "terminal".to_string());
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        let chrome = Chrome {
            bar: true,
            hover: Some((0, BarButton::Close)),
            ..Chrome::default()
        };
        layout.draw_bars(&mut scene, chrome);
        let (cell_width, cell_height) = scene.cell();
        let inner = layout.panes[0].rect.inner();
        let close = inner.column + inner.columns - BUTTON_WIDTH;
        let [(x, y, width, height, fill)] = rounded(&scene)[..] else {
            panic!("one hover square");
        };
        assert_eq!(fill, palette::get().dark_red);
        assert_eq!(width, height);
        assert!(height < cell_height);
        let (left, top) = scene.px(close, inner.row);
        assert!(x >= left && x + width <= left + BUTTON_WIDTH as i32 * cell_width);
        assert!(y > top && y + height < top + cell_height);
    }

    #[test]
    fn a_zoomed_pane_with_a_tag_has_two_tags_on_its_top_line_in_its_outline_colour() {
        let mut layout = Layout::new(AREA, "terminal".to_string());
        layout.panes[0].marks.tags = vec![Tag::Icon('p'), Tag::Zoom];
        let font = crate::Font::load(14.0);
        let drawn = |layout: &Layout| {
            let mut scene = Scene::new(&font);
            layout.draw(&mut scene, Chrome::default());
            scene
        };
        assert_eq!(rounded(&drawn(&layout)).len(), 1);
        layout.toggle_zoom(0);
        let scene = drawn(&layout);
        let tags = rounded(&scene);
        assert_eq!(tags.len(), 2);
        let [_, (zoom_x, ..)] = tags[..] else {
            panic!("two tags");
        };
        assert!(zoom_x < tags[0].0);
        assert!(texts(&scene).contains(&"zoom".to_string()));
        let (_, cell_height) = scene.cell();
        let line_y = cell_height / 2;
        for (_, y, _, height, fill) in tags {
            assert_eq!(fill, palette::get().cyan);
            assert!(y < line_y && y + height > line_y);
        }
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

    #[test]
    fn a_pane_with_a_left_tag_has_it_at_the_left_of_its_top_line_and_it_flashes() {
        let mut layout: Layout = three_anchors();
        layout.focused = 0;
        layout.panes[0].marks.left_tag = Some('s');
        let font = crate::Font::load(14.0);
        let tags = |flash: Option<(usize, f32)>| {
            let mut scene = Scene::new(&font);
            layout.draw(
                &mut scene,
                Chrome {
                    flash,
                    ..Chrome::default()
                },
            );
            rounded(&scene)
        };
        let [(x, _, _, _, fill)] = tags(None)[..] else {
            panic!("one tag");
        };
        let (cell_width, _) = Scene::new(&font).cell();
        assert!(x < 4 * cell_width);
        assert_eq!(fill, palette::get().cyan);
        let [(_, _, _, _, flashed)] = tags(Some((0, 1.0)))[..] else {
            panic!("one tag");
        };
        assert_eq!(flashed, palette::get().foreground);
        assert_eq!(tags(Some((1, 1.0)))[0].4, palette::get().cyan);
    }

    #[test]
    fn a_recorded_pane_has_a_red_tag_after_its_left_tag_with_the_time_and_a_blinking_icon() {
        let mut layout: Layout = three_anchors();
        layout.focused = 0;
        layout.panes[0].marks.left_tag = Some('s');
        let font = crate::Font::load(14.0);
        let drawn = |lit: bool, strength: f32| {
            let recordings = [Recording {
                pane: 0,
                strength,
                icon: 'r',
                lit,
                elapsed: "0:07",
            }];
            let mut scene = Scene::new(&font);
            let chrome = Chrome {
                recordings: &recordings,
                ..Chrome::default()
            };
            layout.draw(&mut scene, chrome);
            scene
        };
        let scene = drawn(true, 0.0);
        let [(camera, ..), (tag, _, _, _, fill)] = rounded(&scene)[..] else {
            panic!("two tags");
        };
        assert!(camera < tag);
        assert_eq!(fill, palette::get().red);
        let lit = texts(&scene);
        assert!(lit.contains(&"r".to_string()) && lit.contains(&"0:07".to_string()));
        let unlit = texts(&drawn(false, 0.0));
        assert!(!unlit.contains(&"r".to_string()) && unlit.contains(&"0:07".to_string()));
        let outline = |scene: &Scene| {
            (scene.shapes.iter()).find_map(|shape| match shape {
                crate::Shape::Rect { color, .. } => Some(*color),
                _ => None,
            })
        };
        assert_eq!(outline(&drawn(true, 0.0)), Some(palette::get().cyan));
        assert_eq!(outline(&drawn(true, 1.0)), Some(palette::get().red));
    }

    #[test]
    fn a_driven_pane_has_a_yellow_tag_after_the_recording_tag_and_a_yellow_outline() {
        let mut layout: Layout = three_anchors();
        layout.focused = 0;
        let font = crate::Font::load(14.0);
        let recordings = [Recording {
            pane: 0,
            strength: 0.0,
            icon: 'r',
            lit: true,
            elapsed: "0:07",
        }];
        let drawn = |strength: f32| {
            let mut scene = Scene::new(&font);
            let chrome = Chrome {
                recordings: &recordings,
                driven: Some(Driven {
                    pane: 0,
                    strength,
                    icon: 'd',
                    label: "coax · Esc stops",
                }),
                ..Chrome::default()
            };
            layout.draw(&mut scene, chrome);
            scene
        };
        let scene = drawn(0.0);
        let [(recorded, _, _, _, red), (driven, _, _, _, yellow)] = rounded(&scene)[..] else {
            panic!("two tags");
        };
        assert!(recorded < driven);
        assert_eq!((red, yellow), (palette::get().red, palette::get().yellow));
        let texts = texts(&scene);
        assert!(texts.contains(&"d".to_string()));
        assert!(texts.contains(&"coax · Esc stops".to_string()));
        let outline = |scene: &Scene| {
            (scene.shapes.iter()).find_map(|shape| match shape {
                crate::Shape::Rect { color, .. } => Some(*color),
                _ => None,
            })
        };
        let full = outline(&drawn(1.0)).unwrap();
        let yellow = palette::get().yellow;
        assert!((0..4).all(|i| (full[i] - yellow[i]).abs() < 1e-6));
    }

    #[test]
    fn on_a_narrow_pane_the_tags_on_the_right_are_dropped_before_the_left_tag() {
        let narrow = CellRect {
            columns: 12,
            ..AREA
        };
        let mut layout = Layout::new(narrow, "terminal".to_string());
        layout.toggle_zoom(0);
        layout.panes[0].marks.left_tag = Some('s');
        layout.panes[0].marks.tags = vec![Tag::Icon('p'), Tag::Zoom];
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw(&mut scene, Chrome::default());
        let texts = texts(&scene);
        assert!(texts.contains(&"s".to_string()));
        assert!(!texts.contains(&"zoom".to_string()));
    }

    #[test]
    fn a_pulsing_pane_has_its_note_in_a_tag_inside_its_own_tags() {
        let mut layout = Layout::new(AREA, "terminal".to_string());
        layout.panes[0].marks.tags = vec![Tag::Text("mark".to_string())];
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        let pulse = Pulse {
            pane: 0,
            strength: 1.0,
            note: Some("look"),
        };
        let chrome = Chrome {
            pulse: Some(pulse),
            ..Chrome::default()
        };
        layout.draw(&mut scene, chrome);
        let [(mark, ..), (note, _, _, _, fill)] = rounded(&scene)[..] else {
            panic!("two tags");
        };
        assert_eq!(fill, palette::get().magenta);
        assert!(note < mark);
        assert!(texts(&scene).contains(&"look".to_string()));
    }

    #[test]
    fn a_placeholder_with_a_button_shows_its_lines_above_the_button_on_the_background() {
        let mut layout = Layout::new(AREA, "terminal".to_string());
        layout.panes[0].marks.placeholder = Placeholder {
            lines: vec!["stopped".to_string()],
            button: Some(("again".to_string(), 7)),
        };
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw(&mut scene, Chrome::default());
        let inner = layout.panes[0].rect.inner();
        let top = inner.row + (inner.rows - 5) / 2;
        let text_rows: Vec<(String, i32, Rgba)> = (scene.shapes.iter())
            .filter_map(|shape| match shape {
                crate::Shape::Text {
                    text, at, color, ..
                } => Some((text.clone(), at.y, *color)),
                _ => None,
            })
            .collect();
        let row = |row: u32| scene.px(0, row).1;
        assert_eq!(
            text_rows,
            [
                ("stopped".to_string(), row(top), palette::get().foreground),
                (
                    "  again  ".to_string(),
                    row(top + 3),
                    palette::get().dark_background
                ),
            ]
        );
        assert!(scene.shapes.iter().any(|shape| matches!(
            shape,
            crate::Shape::Rect { color, .. } if *color == palette::get().accent
        )));
        assert_eq!(layout.placeholder_action(0), Some(7));
        layout.panes[0].add_tab(
            0,
            Tab {
                content: ContentId(0),
                label: String::new(),
            },
        );
        assert_eq!(layout.placeholder_action(0), None);
    }

    #[test]
    fn a_placeholder_without_a_button_has_its_first_line_brighter_than_the_rest() {
        let mut layout = Layout::new(AREA, "terminal".to_string());
        layout.panes[0].marks.placeholder.lines = vec!["title".to_string(), "hint".to_string()];
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        layout.draw(&mut scene, Chrome::default());
        let inner = layout.panes[0].rect.inner();
        let middle = inner.row + inner.rows / 2;
        let row = |row: u32| scene.px(0, row).1;
        let lines: Vec<(String, i32, Rgba)> = (scene.shapes.iter())
            .filter_map(|shape| match shape {
                crate::Shape::Text {
                    text, at, color, ..
                } => Some((text.clone(), at.y, *color)),
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            [
                (
                    "title".to_string(),
                    row(middle - 1),
                    palette::get().light_foreground
                ),
                ("hint".to_string(), row(middle), palette::get().muted),
            ]
        );
        assert_eq!(layout.placeholder_action(0), None);
    }

    #[test]
    fn a_button_that_is_off_is_muted_on_an_unfocused_pane_and_bright_on_the_focused_one() {
        let mut layout: Layout = three_anchors();
        layout.panes[0].marks.buttons = three_buttons();
        layout.panes[1].marks.buttons = three_buttons();
        let font = crate::Font::load(14.0);
        let mut scene = Scene::new(&font);
        let chrome = Chrome {
            bar: true,
            ..Chrome::default()
        };
        layout.draw_bars(&mut scene, chrome);
        let colours = |id: usize| -> Vec<Rgba> {
            let inner = layout.pane(id).unwrap().rect.inner();
            let (left, row) = scene.px(inner.column, inner.row);
            let right = scene.px(inner.column + inner.columns, 0).0;
            (scene.shapes.iter())
                .filter_map(|shape| match shape {
                    crate::Shape::Text {
                        text,
                        at,
                        face: Face::Icon,
                        color,
                    } if text.len() == 1 && at.y == row && (left..right).contains(&at.x) => {
                        Some(*color)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            colours(0),
            [
                palette::get().muted,
                palette::get().foreground,
                palette::get().muted
            ]
        );
        assert_eq!(colours(1), [palette::get().foreground; 3]);
    }
}
