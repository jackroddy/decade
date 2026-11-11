use crate::server::chrome::draw::{Face, Scene, scaled};
use crate::server::chrome::frame::{CARD_RADIUS, POPOVER_PADDING, SHADOW_OFFSET};
use crate::server::chrome::menu::{Action, MenuKey, Outcome};
use crate::server::chrome::palette;
use crate::server::chrome::rename::Named;

// the sizes below are in pixels at 18 px rows, for `scaled`

/// The space above and below an entry's text, inside its row.
const ROW_PADDING: i32 = 3;
const ROW_RADIUS: i32 = 4;

/// What a right click on decade's chrome landed on, with what its entries
/// depend on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// A pane's bar or outline.
    Pane {
        id: usize,
        pinned: bool,
        zoomed: bool,
        scratch: bool,
    },

    /// A workspace's card or tile in the sidebar.
    Workspace { id: u32, clearable: bool },

    /// A tab's pill on the top bar, by index.
    Tab(usize),

    /// The sidebar outside the cards and tiles.
    Sidebar,
}

/// What an entry does when it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Pin(usize),
    Zoom(usize),
    Scratch(usize),

    /// Return the workspace's agents that are done or need attention to idle.
    Clear(u32),
    Rename(Named),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub label: &'static str,
    pub command: Command,
}

/// The entries of the menu for `place`, in the order they are listed.
pub fn entries(place: Place) -> Vec<Entry> {
    let entry = |label, command| Entry { label, command };
    match place {
        Place::Pane {
            id,
            pinned,
            zoomed,
            scratch,
        } => vec![
            entry(if pinned { "Unpin" } else { "Pin" }, Command::Pin(id)),
            entry(if zoomed { "Unzoom" } else { "Zoom" }, Command::Zoom(id)),
            entry(
                if scratch {
                    "Unmark as scratch"
                } else {
                    "Mark as scratch"
                },
                Command::Scratch(id),
            ),
            entry("Rename", Command::Rename(Named::Pane(id))),
        ],
        Place::Workspace { id, clearable } => {
            let rename = entry("Rename", Command::Rename(Named::Workspace(id)));
            if clearable {
                vec![entry("Clear", Command::Clear(id)), rename]
            } else {
                vec![rename]
            }
        }
        Place::Tab(index) => vec![entry("Rename", Command::Rename(Named::Tab(index)))],
        Place::Sidebar => Vec::new(),
    }
}

/// The menu a right click opens, at the pointer and inside the window.
pub struct ContextMenu {
    entries: Vec<Entry>,

    /// The box's top left, in pixels.
    at: (i32, i32),
    size: (i32, i32),

    /// The chrome font's cell.
    cell: (i32, i32),
    selected: usize,
}

impl ContextMenu {
    /// The menu of `entries` with its corner at `pointer`, moved left and
    /// up as far as it takes to fit in `window`, or `None` for no entries.
    pub fn open(
        entries: Vec<Entry>,
        pointer: (i32, i32),
        window: (i32, i32),
        cell: (i32, i32),
    ) -> Option<ContextMenu> {
        if entries.is_empty() {
            return None;
        }
        let (cell_width, cell_height) = cell;
        let longest = (entries.iter())
            .map(|entry| entry.label.chars().count())
            .max()
            .unwrap_or(0) as i32;
        let padding = scaled(POPOVER_PADDING, cell_height);
        let row = row_height(cell_height);
        let size = (
            2 * padding + (longest + 2) * cell_width,
            2 * padding + entries.len() as i32 * row,
        );
        let at = (
            pointer.0.min(window.0 - size.0).max(0),
            pointer.1.min(window.1 - size.1).max(0),
        );
        Some(ContextMenu {
            entries,
            at,
            size,
            cell,
            selected: 0,
        })
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        let (left, top) = self.at;
        (left..left + self.size.0).contains(&x) && (top..top + self.size.1).contains(&y)
    }

    /// The index of the entry under pixel `x`, `y`.
    fn entry_at(&self, x: i32, y: i32) -> Option<usize> {
        let padding = scaled(POPOVER_PADDING, self.cell.1);
        let (left, top) = (self.at.0 + padding, self.at.1 + padding);
        if x < left || x >= self.at.0 + self.size.0 - padding || y < top {
            return None;
        }
        let index = ((y - top) / row_height(self.cell.1)) as usize;
        (index < self.entries.len()).then_some(index)
    }

    /// Select the entry under pixel `x`, `y`, and return whether the
    /// selection moved.
    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        match self.entry_at(x, y) {
            Some(index) if index != self.selected => {
                self.selected = index;
                true
            }
            _ => false,
        }
    }

    /// What a left click at pixel `x`, `y` does.
    pub fn click(&self, x: i32, y: i32) -> Outcome {
        match self.entry_at(x, y) {
            Some(index) => self.run(index),
            None if self.contains(x, y) => Outcome::Open,
            None => Outcome::Close,
        }
    }

    pub fn key(&mut self, key: MenuKey) -> Outcome {
        match key {
            MenuKey::Up => self.selected = self.selected.saturating_sub(1),
            MenuKey::Down => self.selected = (self.selected + 1).min(self.entries.len() - 1),
            MenuKey::Enter => return self.run(self.selected),
            MenuKey::Escape => return Outcome::Close,
            MenuKey::Left
            | MenuKey::Right
            | MenuKey::Backspace
            | MenuKey::Tab
            | MenuKey::Char(_) => {}
        }
        Outcome::Open
    }

    fn run(&self, index: usize) -> Outcome {
        Outcome::Run(Action::Command(self.entries[index].command))
    }

    /// Draw the menu in window pixels, with the chrome font.
    pub fn draw(&self, scene: &mut Scene) {
        let (left, top) = self.at;
        let (width, height) = self.size;
        let (cell_width, cell_height) = self.cell;
        let padding = scaled(POPOVER_PADDING, cell_height);
        let radius = scaled(CARD_RADIUS, cell_height);
        let offset = scaled(SHADOW_OFFSET, cell_height);
        let shadow = palette::SHADOW;
        scene.rounded(
            left + offset,
            top + offset,
            self.size,
            radius,
            shadow,
            shadow,
        );
        let fill = palette::LIGHTER_BACKGROUND;
        scene.rounded(left, top, (width, height), radius, fill, palette::BORDER);

        let row = row_height(cell_height);
        let row_radius = scaled(ROW_RADIUS, cell_height);
        for (index, entry) in self.entries.iter().enumerate() {
            let y = top + padding + index as i32 * row;
            let face = if index == self.selected {
                let fill = palette::SELECTION;
                let size = (width - 2 * padding, row);
                scene.rounded(left + padding, y, size, row_radius, fill, fill);
                Face::ChromeBold
            } else {
                Face::Chrome
            };
            let text_y = y + scaled(ROW_PADDING, cell_height);
            let text_x = left + padding + cell_width;
            scene.text_px(text_x, text_y, entry.label, face, palette::FOREGROUND);
        }
    }
}

fn row_height(cell_height: i32) -> i32 {
    cell_height + 2 * scaled(ROW_PADDING, cell_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (i32, i32) = (8, 18);
    const WINDOW: (i32, i32) = (800, 600);

    fn labels(place: Place) -> Vec<&'static str> {
        entries(place).iter().map(|entry| entry.label).collect()
    }

    fn pane_menu() -> ContextMenu {
        let place = Place::Pane {
            id: 4,
            pinned: false,
            zoomed: false,
            scratch: false,
        };
        ContextMenu::open(entries(place), (100, 100), WINDOW, CELL).unwrap()
    }

    #[test]
    fn a_pane_offers_pin_zoom_scratch_and_rename_labelled_for_what_a_click_does() {
        let plain = Place::Pane {
            id: 4,
            pinned: false,
            zoomed: false,
            scratch: false,
        };
        assert_eq!(labels(plain), ["Pin", "Zoom", "Mark as scratch", "Rename"]);
        let commands: Vec<Command> = entries(plain).iter().map(|e| e.command).collect();
        assert_eq!(
            commands,
            [
                Command::Pin(4),
                Command::Zoom(4),
                Command::Scratch(4),
                Command::Rename(Named::Pane(4))
            ]
        );
        let set = Place::Pane {
            id: 4,
            pinned: true,
            zoomed: true,
            scratch: true,
        };
        assert_eq!(
            labels(set),
            ["Unpin", "Unzoom", "Unmark as scratch", "Rename"]
        );
    }

    #[test]
    fn a_workspace_offers_rename_and_clear_only_when_its_agent_is_done_or_needs_attention() {
        let done = Place::Workspace {
            id: 2,
            clearable: true,
        };
        let rename = Entry {
            label: "Rename",
            command: Command::Rename(Named::Workspace(2)),
        };
        let clear = Entry {
            label: "Clear",
            command: Command::Clear(2),
        };
        assert_eq!(entries(done), [clear, rename]);
        let working = Place::Workspace {
            id: 2,
            clearable: false,
        };
        assert_eq!(entries(working), [rename]);
    }

    #[test]
    fn a_tab_pill_offers_rename_and_empty_sidebar_space_opens_no_menu() {
        assert_eq!(
            entries(Place::Tab(1)),
            [Entry {
                label: "Rename",
                command: Command::Rename(Named::Tab(1))
            }]
        );
        assert!(ContextMenu::open(entries(Place::Sidebar), (0, 0), WINDOW, CELL).is_none());
    }

    #[test]
    fn the_menu_opens_at_the_pointer_and_moves_inside_the_window_near_an_edge() {
        let menu = pane_menu();
        assert_eq!(menu.at, (100, 100));

        // "Mark as scratch" is 15 cells, with a cell each side, and
        // each row is 18 px with 3 above and below
        let size = (2 * 6 + 17 * 8, 2 * 6 + 4 * 24);
        assert_eq!(menu.size, size);

        let corner = ContextMenu::open(menu.entries.clone(), (795, 590), WINDOW, CELL).unwrap();
        assert_eq!(corner.at, (WINDOW.0 - size.0, WINDOW.1 - size.1));
        let tiny = ContextMenu::open(menu.entries.clone(), (50, 50), (40, 40), CELL).unwrap();
        assert_eq!(tiny.at, (0, 0));
    }

    #[test]
    fn up_and_down_move_the_selection_and_stop_at_the_first_and_last_entry() {
        let mut menu = pane_menu();
        assert_eq!(menu.selected, 0);
        assert_eq!(menu.key(MenuKey::Up), Outcome::Open);
        assert_eq!(menu.selected, 0);
        for _ in 0..4 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.selected, 3);
        assert_eq!(
            menu.key(MenuKey::Enter),
            Outcome::Run(Action::Command(Command::Rename(Named::Pane(4))))
        );
        menu.key(MenuKey::Up);
        menu.key(MenuKey::Up);
        assert_eq!(
            menu.key(MenuKey::Enter),
            Outcome::Run(Action::Command(Command::Zoom(4)))
        );
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Close);
    }

    #[test]
    fn hovering_selects_an_entry_and_a_click_runs_it_or_dismisses_outside() {
        let mut menu = pane_menu();
        let (x, y) = (100 + 6 + 8, 100 + 6 + 24 + 10);
        assert!(menu.hover(x, y));
        assert_eq!(menu.selected, 1);
        assert!(!menu.hover(x, y));
        assert_eq!(
            menu.click(x, y),
            Outcome::Run(Action::Command(Command::Zoom(4)))
        );
        assert_eq!(menu.click(102, 102), Outcome::Open);
        assert_eq!(menu.click(99, 102), Outcome::Close);
    }
}
