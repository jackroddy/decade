use decade_channel::AgentState;
use decade_ui::overlay::{LARGE, MenuKey, draw_hint, hint_width};
use decade_ui::palette;
use decade_ui::{Face, Fonts, Point, Scene, Size, fit};

use crate::chrome::frame::mark;
use crate::chrome::menu::{Action, Outcome};

const DRAG_THRESHOLD: i32 = 8;

/// The least width and height in physical pixels of a cell's close button.
const MIN_BUTTON: i32 = 24;

const CLOSE_ICON: char = '\u{f0156}';

// the sizes below are in pixels at the large size's mockup cell, for
// `LARGE.px`
const FRAME_RADIUS: i32 = 10;

/// The gap between a cell's thumbnail and the inside of its frame.
const FRAME_GAP: i32 = 4;

/// A workspace, as its cell shows it.
pub struct Entry<'a> {
    pub id: u32,
    pub name: &'a str,
    pub state: Option<AgentState>,
    pub closing: bool,
}

/// A rectangle in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub at: Point,
    pub size: Size,
}

impl Rect {
    fn contains(&self, point: Point) -> bool {
        point.x >= self.at.x
            && point.x < self.at.x + self.size.width
            && point.y >= self.at.y
            && point.y < self.at.y + self.size.height
    }
}

pub struct Picker {
    /// The selected cell: a workspace, or past the last one the new-workspace cell.
    selected: usize,

    /// The workspace whose close waits for a second press on its x.
    confirming: Option<u32>,

    /// The cell a press landed on, and where.
    press: Option<(usize, Point)>,

    /// The workspace cell being dragged to a new place.
    dragging: Option<usize>,
}

pub struct Grid {
    /// The thumbnail of each cell, workspaces first, then the new-workspace cell.
    pub cells: Vec<Rect>,

    pub columns: usize,

    /// The height of the label row under each thumbnail.
    pub label: i32,

    /// The side of each cell's close button.
    button: i32,

    size: Size,
}

impl Grid {
    pub fn new(count: usize, size: Size, cell_height: i32) -> Grid {
        let pad = cell_height * 2;
        let label = cell_height * 2;
        let columns = (count as f64).sqrt().ceil().max(1.0) as usize;
        let rows = count.div_ceil(columns).max(1);
        let free_w = size.width - pad * (columns as i32 + 1);
        let free_h = size.height - pad * (rows as i32 + 1) - label * rows as i32;
        let scale = (free_w as f64 / columns as f64 / size.width as f64)
            .min(free_h as f64 / rows as f64 / size.height as f64)
            .max(0.01);
        let width = (size.width as f64 * scale) as i32;
        let height = (size.height as f64 * scale) as i32;
        let grid_w = columns as i32 * width + (columns as i32 - 1) * pad;
        let grid_h = rows as i32 * (height + label) + (rows as i32 - 1) * pad;
        let left = (size.width - grid_w) / 2;
        let top = (size.height - grid_h) / 2;
        let cells = (0..count)
            .map(|i| {
                let (column, row) = ((i % columns) as i32, (i / columns) as i32);
                let x = left + column * (width + pad);
                let y = top + row * (height + label + pad);
                Rect {
                    at: Point { x, y },
                    size: Size { width, height },
                }
            })
            .collect();
        Grid {
            cells,
            columns,
            label,
            button: (cell_height * 3 / 2).max(MIN_BUTTON),
            size,
        }
    }

    fn cell_at(&self, point: Point) -> Option<usize> {
        self.cells.iter().position(|cell| cell.contains(point))
    }

    fn close_button(&self, cell: usize) -> Rect {
        let thumbnail = self.cells[cell];
        let side = self.button;
        Rect {
            at: Point {
                x: thumbnail.at.x + thumbnail.size.width - side,
                y: thumbnail.at.y,
            },
            size: Size {
                width: side,
                height: side,
            },
        }
    }
}

impl Picker {
    pub fn new(selected: usize) -> Picker {
        Picker {
            selected,
            confirming: None,
            press: None,
            dragging: None,
        }
    }

    /// Keep the selection on a cell after the grid shrank to `cells`.
    pub fn clamp(&mut self, cells: usize) {
        self.selected = self.selected.min(cells - 1);
    }

    pub fn key(&mut self, key: MenuKey, ids: &[u32], grid: &Grid) -> Outcome {
        let count = ids.len() + 1;
        let columns = grid.columns;
        let selected = self.selected;
        match key {
            MenuKey::Escape if self.confirming.is_some() => self.confirming = None,
            MenuKey::Escape => return Outcome::Close,
            MenuKey::Enter => {
                if let Some(id) = self.confirming.take() {
                    return Outcome::Apply(Action::CloseWorkspace(id));
                }
                return Outcome::Run(open(ids, self.selected));
            }
            MenuKey::Tab => self.selected = (self.selected + 1) % count,
            MenuKey::Left => self.selected = self.selected.saturating_sub(1),
            MenuKey::Right => self.selected = (self.selected + 1).min(count - 1),
            MenuKey::Up if self.selected >= columns => self.selected -= columns,
            MenuKey::Down => self.selected = (self.selected + columns).min(count - 1),
            MenuKey::Up | MenuKey::Home | MenuKey::End | MenuKey::Backspace | MenuKey::Char(_) => {}
        }
        if self.selected != selected {
            self.confirming = None;
        }
        Outcome::Open
    }

    pub fn press(&mut self, point: Point, ids: &[u32], grid: &Grid) -> Outcome {
        let Some(cell) = grid.cell_at(point) else {
            self.confirming = None;
            return Outcome::Open;
        };
        self.selected = cell;
        if let Some(&id) = ids.get(cell)
            && grid.close_button(cell).contains(point)
        {
            if self.confirming == Some(id) {
                self.confirming = None;
                return Outcome::Apply(Action::CloseWorkspace(id));
            }
            self.confirming = Some(id);
            return Outcome::Open;
        }
        self.confirming = None;
        self.press = Some((cell, point));
        Outcome::Open
    }

    /// Whether the motion started or continued a drag.
    pub fn motion(&mut self, point: Point, ids: &[u32]) -> bool {
        let Some((cell, at)) = self.press else {
            return false;
        };
        let moved = (point.x - at.x).abs().max((point.y - at.y).abs());
        if self.dragging.is_none() && cell < ids.len() && moved > DRAG_THRESHOLD {
            self.dragging = Some(cell);
        }
        self.dragging.is_some()
    }

    pub fn release(&mut self, point: Point, ids: &[u32], grid: &Grid) -> Outcome {
        let pressed = self.press.take();
        let target = grid.cell_at(point);
        if let Some(from) = self.dragging.take() {
            let to = target.unwrap_or(from).min(ids.len() - 1);
            self.selected = to;
            return if to == from {
                Outcome::Open
            } else {
                Outcome::Apply(Action::MoveWorkspace { from, to })
            };
        }
        match (pressed, target) {
            (Some((cell, _)), Some(target)) if cell == target => Outcome::Run(open(ids, cell)),
            _ => Outcome::Open,
        }
    }

    /// The keys the picker takes, as key caps under the grid.
    fn hint(&self) -> &'static [(&'static str, &'static str)] {
        if self.confirming.is_some() {
            &[("enter", "close"), ("esc", "cancel")]
        } else {
            &[("tab", "move"), ("enter", "show"), ("esc", "close")]
        }
    }

    /// The cells' labels and close buttons and the hint under the grid,
    /// then the cells' frames, to be drawn over and under the thumbnails.
    pub fn draw(
        &self,
        grid: &Grid,
        workspaces: &[Entry],
        current: usize,
        pointer: Point,
        fonts: &Fonts,
    ) -> (Scene, Scene) {
        let (menu_width, menu_height) =
            (fonts.menu.cell_width as i32, fonts.menu.cell_height as i32);
        let mut over = Scene::new(&fonts.chrome);
        let mut under = Scene::new(&fonts.chrome);
        let line = over.line_width();
        let label_width = |text: &str| text.chars().count() as i32 * menu_width;
        let drop_target = self.dragging.and_then(|_| grid.cell_at(pointer));
        let radius = LARGE.px(FRAME_RADIUS, menu_height);
        for (i, cell) in grid.cells.iter().enumerate() {
            let workspace = workspaces.get(i);
            let confirming = workspace.is_some_and(|w| self.confirming == Some(w.id));
            let red = confirming || workspace.is_some_and(|w| w.closing);
            let thick = if self.selected == i { line * 2 } else { line };
            let (ring, stroke) = if drop_target == Some(i) {
                (palette::get().cyan, line)
            } else if red {
                (palette::get().red, thick)
            } else if self.selected == i {
                (palette::get().accent, thick)
            } else {
                (palette::get().muted, line)
            };
            let gap = LARGE.px(FRAME_GAP, menu_height) + stroke;
            let (x, y) = (cell.at.x - gap, cell.at.y - gap);
            let (w, h) = (cell.size.width + gap * 2, cell.size.height + gap * 2);
            let inside = palette::get().dark_background;
            under.rounded(x, y, (w, h), radius, ring, ring);
            let inner = (w - stroke * 2, h - stroke * 2);
            under.rounded(
                x + stroke,
                y + stroke,
                inner,
                radius - stroke,
                inside,
                inside,
            );

            let room = (cell.size.width / menu_width).max(1) as usize;
            let label_y = cell.at.y + cell.size.height + gap + (grid.label - gap - menu_height) / 2;
            let Some(workspace) = workspace else {
                let text = fit("+ New workspace", room);
                let x = cell.at.x + (cell.size.width - label_width(&text)) / 2;
                let y = cell.at.y + (cell.size.height - menu_height) / 2;
                over.text_px(x, y, &text, Face::Menu, palette::get().foreground);
                continue;
            };
            let (glyph, glyph_color) = mark(workspace.state);
            let (label, color, face) = if confirming {
                let label = format!("{} — enter to close", workspace.name);
                (label, palette::get().light_red, Face::Menu)
            } else if red {
                let label = format!("{} closing", workspace.name);
                (label, palette::get().light_red, Face::Menu)
            } else if i == current {
                (
                    workspace.name.to_string(),
                    palette::get().foreground,
                    Face::MenuBold,
                )
            } else {
                let label = workspace.name.to_string();
                (label, palette::get().faded_foreground, Face::Menu)
            };
            let glyph_color = if red {
                palette::get().light_red
            } else {
                glyph_color
            };
            let label = fit(&label, room.saturating_sub(2).max(1));
            let x = cell.at.x + (cell.size.width - label_width(&label) - 2 * menu_width) / 2;
            over.text_px(x, label_y, &glyph.to_string(), Face::Menu, glyph_color);
            over.text_px(x + 2 * menu_width, label_y, &label, face, color);
            let button = grid.close_button(i);
            let icon_color = if confirming {
                palette::get().light_red
            } else {
                palette::get().light_foreground
            };
            let icon_x = button.at.x + (button.size.width - menu_width) / 2;
            let icon_y = button.at.y + (button.size.height - menu_height) / 2;
            over.text_px(
                icon_x,
                icon_y,
                &CLOSE_ICON.to_string(),
                Face::Menu,
                icon_color,
            );
        }
        let size = grid.size;
        let cell = (menu_width, menu_height);
        let hint_y = size.height - (grid.label + menu_height) / 2;
        let hint_x = (size.width - hint_width(&LARGE, self.hint(), cell)) / 2;
        draw_hint(&mut over, &LARGE, (hint_x, hint_y), self.hint(), cell);
        (over, under)
    }
}

/// Show the workspace in `cell`, or start a new one from the cell past them.
fn open(ids: &[u32], cell: usize) -> Action {
    match ids.get(cell) {
        Some(&id) => Action::ShowWorkspace(id),
        None => Action::StartNewWorkspace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (i32, i32) = (1600, 900);
    const CELL: i32 = 18;

    fn grid(count: usize) -> Grid {
        Grid::new(count, size(), CELL)
    }

    fn size() -> Size {
        Size {
            width: SIZE.0,
            height: SIZE.1,
        }
    }

    fn center(rect: Rect) -> Point {
        Point {
            x: rect.at.x + rect.size.width / 2,
            y: rect.at.y + rect.size.height / 2,
        }
    }

    #[test]
    fn the_grid_is_as_wide_as_the_square_root_of_the_cells_and_fits_the_output() {
        let grid = grid(5);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.cells[3].at.y, grid.cells[4].at.y);
        assert!(grid.cells[3].at.y > grid.cells[0].at.y);
        for cell in &grid.cells {
            assert!(cell.at.x >= 0 && cell.at.x + cell.size.width <= SIZE.0);
            assert!(cell.at.y >= 0 && cell.at.y + cell.size.height + grid.label <= SIZE.1);
        }
        let ratio = grid.cells[0].size.width as f64 / grid.cells[0].size.height as f64;
        assert!((ratio - 16.0 / 9.0).abs() < 0.02);
    }

    #[test]
    fn tab_steps_through_every_cell_and_wraps() {
        let mut picker = Picker::new(0);
        let ids = [3, 5];
        let grid = grid(3);
        picker.key(MenuKey::Tab, &ids, &grid);
        picker.key(MenuKey::Tab, &ids, &grid);
        assert_eq!(picker.selected, 2);
        picker.key(MenuKey::Tab, &ids, &grid);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn enter_shows_the_selected_workspace_or_starts_a_new_one() {
        let ids = [3, 5];
        let grid = grid(3);
        let mut picker = Picker::new(1);
        let show = Outcome::Run(Action::ShowWorkspace(5));
        assert_eq!(picker.key(MenuKey::Enter, &ids, &grid), show);
        picker.key(MenuKey::Down, &ids, &grid);
        let new = Outcome::Run(Action::StartNewWorkspace);
        assert_eq!(picker.key(MenuKey::Enter, &ids, &grid), new);
    }

    #[test]
    fn a_workspace_closes_only_on_a_second_press_of_its_x() {
        let ids = [3, 5];
        let grid = grid(3);
        let button = grid.close_button(1);
        let on_x = center(button);
        let mut picker = Picker::new(0);
        assert_eq!(picker.press(on_x, &ids, &grid), Outcome::Open);
        assert_eq!(picker.confirming, Some(5));
        let close = Outcome::Apply(Action::CloseWorkspace(5));
        assert_eq!(picker.press(on_x, &ids, &grid), close);
    }

    #[test]
    fn the_close_button_is_at_least_the_least_button_size_with_a_small_font() {
        let button = Grid::new(3, size(), 8).close_button(0);
        assert!(button.size.width >= MIN_BUTTON && button.size.height >= MIN_BUTTON);
    }

    #[test]
    fn escape_cancels_a_pending_close_before_it_closes_the_picker() {
        let grid = grid(2);
        let mut picker = Picker::new(0);
        picker.confirming = Some(3);
        assert_eq!(picker.key(MenuKey::Escape, &[3], &grid), Outcome::Open);
        assert_eq!(picker.confirming, None);
        assert_eq!(picker.key(MenuKey::Escape, &[3], &grid), Outcome::Close);
    }

    #[test]
    fn a_click_shows_the_workspace_and_a_drag_moves_it() {
        let ids = [3, 5, 8];
        let grid = grid(4);
        let mut picker = Picker::new(0);
        picker.press(center(grid.cells[1]), &ids, &grid);
        assert_eq!(
            picker.release(center(grid.cells[1]), &ids, &grid),
            Outcome::Run(Action::ShowWorkspace(5))
        );
        picker.press(center(grid.cells[0]), &ids, &grid);
        assert!(picker.motion(center(grid.cells[2]), &ids));
        assert_eq!(
            picker.release(center(grid.cells[2]), &ids, &grid),
            Outcome::Apply(Action::MoveWorkspace { from: 0, to: 2 })
        );
    }

    #[test]
    fn a_workspace_dropped_on_the_new_workspace_cell_goes_last() {
        let ids = [3, 5];
        let grid = grid(3);
        let mut picker = Picker::new(0);
        picker.press(center(grid.cells[0]), &ids, &grid);
        picker.motion(center(grid.cells[2]), &ids);
        assert_eq!(
            picker.release(center(grid.cells[2]), &ids, &grid),
            Outcome::Apply(Action::MoveWorkspace { from: 0, to: 1 })
        );
    }
}
