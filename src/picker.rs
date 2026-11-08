use smithay::utils::{Physical, Point, Rectangle, Size};

use crate::menu::MenuKey;

const DRAG_THRESHOLD: i32 = 8;

/// The least width and height in physical pixels of a cell's close button.
const MIN_BUTTON: i32 = 24;

pub struct Picker {
    /// The selected cell: a workspace, or past the last one the new-workspace cell.
    pub selected: usize,

    /// The workspace whose close waits for a second press on its x.
    pub confirming: Option<u32>,

    /// The cell a press landed on, and where.
    press: Option<(usize, Point<i32, Physical>)>,

    /// The workspace cell being dragged to a new place.
    pub dragging: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Open,
    Close,
    Show(u32),
    New,
    CloseWorkspace(u32),
    Move { from: usize, to: usize },
}

pub struct Grid {
    /// The thumbnail of each cell, workspaces first, then the new-workspace cell.
    pub cells: Vec<Rectangle<i32, Physical>>,

    pub columns: usize,

    /// The height of the label row under each thumbnail.
    pub label: i32,
}

impl Grid {
    pub fn new(count: usize, size: Size<i32, Physical>, cell_height: i32) -> Grid {
        let pad = cell_height * 2;
        let label = cell_height * 2;
        let columns = (count as f64).sqrt().ceil().max(1.0) as usize;
        let rows = count.div_ceil(columns).max(1);
        let free_w = size.w - pad * (columns as i32 + 1);
        let free_h = size.h - pad * (rows as i32 + 1) - label * rows as i32;
        let scale = (free_w as f64 / columns as f64 / size.w as f64)
            .min(free_h as f64 / rows as f64 / size.h as f64)
            .max(0.01);
        let width = (size.w as f64 * scale) as i32;
        let height = (size.h as f64 * scale) as i32;
        let grid_w = columns as i32 * width + (columns as i32 - 1) * pad;
        let grid_h = rows as i32 * (height + label) + (rows as i32 - 1) * pad;
        let left = (size.w - grid_w) / 2;
        let top = (size.h - grid_h) / 2;
        let cells = (0..count)
            .map(|i| {
                let (column, row) = ((i % columns) as i32, (i / columns) as i32);
                let x = left + column * (width + pad);
                let y = top + row * (height + label + pad);
                Rectangle::new((x, y).into(), (width, height).into())
            })
            .collect();
        Grid {
            cells,
            columns,
            label,
        }
    }

    pub fn cell_at(&self, point: Point<i32, Physical>) -> Option<usize> {
        self.cells.iter().position(|cell| cell.contains(point))
    }

    pub fn close_button(&self, cell: usize, cell_height: i32) -> Rectangle<i32, Physical> {
        let thumbnail = self.cells[cell];
        let side = (cell_height * 3 / 2).max(MIN_BUTTON);
        let at = thumbnail.loc + Point::from((thumbnail.size.w - side, 0));
        Rectangle::new(at, (side, side).into())
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

    pub fn key(&mut self, key: MenuKey, ids: &[u32], columns: usize) -> Outcome {
        let count = ids.len() + 1;
        let selected = self.selected;
        match key {
            MenuKey::Escape if self.confirming.is_some() => self.confirming = None,
            MenuKey::Escape => return Outcome::Close,
            MenuKey::Enter => {
                if let Some(id) = self.confirming.take() {
                    return Outcome::CloseWorkspace(id);
                }
                return match ids.get(self.selected) {
                    Some(&id) => Outcome::Show(id),
                    None => Outcome::New,
                };
            }
            MenuKey::Tab => self.selected = (self.selected + 1) % count,
            MenuKey::Left => self.selected = self.selected.saturating_sub(1),
            MenuKey::Right => self.selected = (self.selected + 1).min(count - 1),
            MenuKey::Up if self.selected >= columns => self.selected -= columns,
            MenuKey::Down => self.selected = (self.selected + columns).min(count - 1),
            MenuKey::Up | MenuKey::Backspace | MenuKey::Char(_) => {}
        }
        if self.selected != selected {
            self.confirming = None;
        }
        Outcome::Open
    }

    pub fn press(
        &mut self,
        point: Point<i32, Physical>,
        ids: &[u32],
        grid: &Grid,
        cell_height: i32,
    ) -> Outcome {
        let Some(cell) = grid.cell_at(point) else {
            self.confirming = None;
            return Outcome::Open;
        };
        self.selected = cell;
        if let Some(&id) = ids.get(cell)
            && grid.close_button(cell, cell_height).contains(point)
        {
            if self.confirming == Some(id) {
                self.confirming = None;
                return Outcome::CloseWorkspace(id);
            }
            self.confirming = Some(id);
            return Outcome::Open;
        }
        self.confirming = None;
        self.press = Some((cell, point));
        Outcome::Open
    }

    /// Whether the motion started or continued a drag.
    pub fn motion(&mut self, point: Point<i32, Physical>, ids: &[u32]) -> bool {
        let Some((cell, at)) = self.press else {
            return false;
        };
        let moved = (point.x - at.x).abs().max((point.y - at.y).abs());
        if self.dragging.is_none() && cell < ids.len() && moved > DRAG_THRESHOLD {
            self.dragging = Some(cell);
        }
        self.dragging.is_some()
    }

    pub fn release(&mut self, point: Point<i32, Physical>, ids: &[u32], grid: &Grid) -> Outcome {
        let pressed = self.press.take();
        let target = grid.cell_at(point);
        if let Some(from) = self.dragging.take() {
            let to = target.unwrap_or(from).min(ids.len() - 1);
            self.selected = to;
            return if to == from {
                Outcome::Open
            } else {
                Outcome::Move { from, to }
            };
        }
        match (pressed, target) {
            (Some((cell, _)), Some(target)) if cell == target => match ids.get(cell) {
                Some(&id) => Outcome::Show(id),
                None => Outcome::New,
            },
            _ => Outcome::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (i32, i32) = (1600, 900);
    const CELL: i32 = 18;

    fn grid(count: usize) -> Grid {
        Grid::new(count, SIZE.into(), CELL)
    }

    fn center(grid: &Grid, cell: usize) -> Point<i32, Physical> {
        let rect = grid.cells[cell];
        rect.loc + Point::from((rect.size.w / 2, rect.size.h / 2))
    }

    #[test]
    fn the_grid_is_as_wide_as_the_square_root_of_the_cells_and_fits_the_output() {
        let grid = grid(5);
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.cells[3].loc.y, grid.cells[4].loc.y);
        assert!(grid.cells[3].loc.y > grid.cells[0].loc.y);
        for cell in &grid.cells {
            assert!(cell.loc.x >= 0 && cell.loc.x + cell.size.w <= SIZE.0);
            assert!(cell.loc.y >= 0 && cell.loc.y + cell.size.h + grid.label <= SIZE.1);
        }
        let ratio = grid.cells[0].size.w as f64 / grid.cells[0].size.h as f64;
        assert!((ratio - 16.0 / 9.0).abs() < 0.02);
    }

    #[test]
    fn tab_steps_through_every_cell_and_wraps() {
        let mut picker = Picker::new(0);
        let ids = [3, 5];
        picker.key(MenuKey::Tab, &ids, 2);
        picker.key(MenuKey::Tab, &ids, 2);
        assert_eq!(picker.selected, 2);
        picker.key(MenuKey::Tab, &ids, 2);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn enter_shows_the_selected_workspace_or_starts_a_new_one() {
        let ids = [3, 5];
        let mut picker = Picker::new(1);
        assert_eq!(picker.key(MenuKey::Enter, &ids, 2), Outcome::Show(5));
        picker.key(MenuKey::Down, &ids, 2);
        assert_eq!(picker.key(MenuKey::Enter, &ids, 2), Outcome::New);
    }

    #[test]
    fn a_workspace_closes_only_on_a_second_press_of_its_x() {
        let ids = [3, 5];
        let grid = grid(3);
        let button = grid.close_button(1, CELL);
        let on_x = button.loc + Point::from((button.size.w / 2, button.size.h / 2));
        let mut picker = Picker::new(0);
        assert_eq!(picker.press(on_x, &ids, &grid, CELL), Outcome::Open);
        assert_eq!(picker.confirming, Some(5));
        assert_eq!(
            picker.press(on_x, &ids, &grid, CELL),
            Outcome::CloseWorkspace(5)
        );
    }

    #[test]
    fn the_close_button_is_at_least_the_least_button_size_with_a_small_font() {
        let button = grid(3).close_button(0, 8);
        assert!(button.size.w >= MIN_BUTTON && button.size.h >= MIN_BUTTON);
    }

    #[test]
    fn escape_cancels_a_pending_close_before_it_closes_the_picker() {
        let mut picker = Picker::new(0);
        picker.confirming = Some(3);
        assert_eq!(picker.key(MenuKey::Escape, &[3], 1), Outcome::Open);
        assert_eq!(picker.confirming, None);
        assert_eq!(picker.key(MenuKey::Escape, &[3], 1), Outcome::Close);
    }

    #[test]
    fn a_click_shows_the_workspace_and_a_drag_moves_it() {
        let ids = [3, 5, 8];
        let grid = grid(4);
        let mut picker = Picker::new(0);
        picker.press(center(&grid, 1), &ids, &grid, CELL);
        assert_eq!(
            picker.release(center(&grid, 1), &ids, &grid),
            Outcome::Show(5)
        );
        picker.press(center(&grid, 0), &ids, &grid, CELL);
        assert!(picker.motion(center(&grid, 2), &ids));
        assert_eq!(
            picker.release(center(&grid, 2), &ids, &grid),
            Outcome::Move { from: 0, to: 2 }
        );
    }

    #[test]
    fn a_workspace_dropped_on_the_new_workspace_cell_goes_last() {
        let ids = [3, 5];
        let grid = grid(3);
        let mut picker = Picker::new(0);
        picker.press(center(&grid, 0), &ids, &grid, CELL);
        picker.motion(center(&grid, 2), &ids);
        assert_eq!(
            picker.release(center(&grid, 2), &ids, &grid),
            Outcome::Move { from: 0, to: 1 }
        );
    }
}
