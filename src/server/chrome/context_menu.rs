use crate::server::chrome::draw::{Face, Scene, scaled};
use crate::server::chrome::frame::{CARD_RADIUS, POPOVER_PADDING, SHADOW_OFFSET};
use crate::server::chrome::menu::{Action, MenuKey, Outcome};
use crate::server::chrome::palette;
use crate::server::chrome::rename::Named;
use crate::server::workspace::Group;

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

    /// A workspace's card or tile in the sidebar, with the id of its group.
    Workspace {
        id: u32,
        clearable: bool,
        group: Option<u32>,
    },

    /// A group's header in the open sidebar, by the group's id.
    Group(u32),

    /// A tab's pill on the top bar, by index.
    Tab(usize),

    /// The open sidebar outside the cards and headers.
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

    /// Ask for a name and add a group by that name below the others.
    NewGroup,

    /// Remove the group, returning its workspaces to ungrouped.
    DeleteGroup(u32),

    /// Put the workspace in the group, or take it out of its group for
    /// `None`.
    MoveToGroup {
        workspace: u32,
        group: Option<u32>,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub label: String,
    pub command: Command,
}

/// The entries of the menu for `place`, in the order they are listed, with
/// `groups` the sidebar's groups in order.
pub fn entries(place: Place, groups: &[Group]) -> Vec<Entry> {
    let entry = |label: &str, command| Entry {
        label: label.to_string(),
        command,
    };
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
        Place::Workspace {
            id,
            clearable,
            group,
        } => {
            let mut entries = Vec::new();
            if clearable {
                entries.push(entry("Clear", Command::Clear(id)));
            }
            entries.push(entry("Rename", Command::Rename(Named::Workspace(id))));
            for other in groups.iter().filter(|other| Some(other.id) != group) {
                let command = Command::MoveToGroup {
                    workspace: id,
                    group: Some(other.id),
                };
                entries.push(entry(&format!("Move to {}", other.name), command));
            }
            if let Some(own) = groups.iter().find(|own| Some(own.id) == group) {
                let command = Command::MoveToGroup {
                    workspace: id,
                    group: None,
                };
                entries.push(entry(&format!("Move out of {}", own.name), command));
            }
            entries
        }
        Place::Group(id) => vec![
            entry("Rename", Command::Rename(Named::Group(id))),
            entry("Delete", Command::DeleteGroup(id)),
        ],
        Place::Tab(index) => vec![entry("Rename", Command::Rename(Named::Tab(index)))],
        Place::Sidebar => vec![entry("New group", Command::NewGroup)],
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
            scene.text_px(text_x, text_y, &entry.label, face, palette::FOREGROUND);
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

    fn labels(place: Place) -> Vec<String> {
        (entries(place, &groups()).into_iter())
            .map(|entry| entry.label)
            .collect()
    }

    fn groups() -> Vec<Group> {
        let group = |id, name: &str| Group {
            id,
            name: name.to_string(),
            folded: false,
        };
        vec![group(7, "work"), group(9, "home")]
    }

    fn entry(label: &str, command: Command) -> Entry {
        Entry {
            label: label.to_string(),
            command,
        }
    }

    fn pane_menu() -> ContextMenu {
        let place = Place::Pane {
            id: 4,
            pinned: false,
            zoomed: false,
            scratch: false,
        };
        ContextMenu::open(entries(place, &[]), (100, 100), WINDOW, CELL).unwrap()
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
        let commands: Vec<Command> = entries(plain, &[]).iter().map(|e| e.command).collect();
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
            group: None,
        };
        let rename = entry("Rename", Command::Rename(Named::Workspace(2)));
        let clear = entry("Clear", Command::Clear(2));
        assert_eq!(entries(done, &[]), [clear, rename.clone()]);
        let working = Place::Workspace {
            id: 2,
            clearable: false,
            group: None,
        };
        assert_eq!(entries(working, &[]), [rename]);
    }

    #[test]
    fn a_workspace_offers_a_move_to_every_other_group_and_out_of_its_own() {
        let ungrouped = Place::Workspace {
            id: 2,
            clearable: false,
            group: None,
        };
        assert_eq!(
            labels(ungrouped),
            ["Rename", "Move to work", "Move to home"]
        );
        let at_work = Place::Workspace {
            id: 2,
            clearable: false,
            group: Some(7),
        };
        assert_eq!(
            labels(at_work),
            ["Rename", "Move to home", "Move out of work"]
        );
        let commands: Vec<Command> = (entries(at_work, &groups()).into_iter())
            .map(|entry| entry.command)
            .collect();
        assert_eq!(
            commands[1..],
            [
                Command::MoveToGroup {
                    workspace: 2,
                    group: Some(9)
                },
                Command::MoveToGroup {
                    workspace: 2,
                    group: None
                }
            ]
        );
    }

    #[test]
    fn a_group_header_offers_rename_and_delete_and_empty_sidebar_space_a_new_group() {
        assert_eq!(
            entries(Place::Group(7), &groups()),
            [
                entry("Rename", Command::Rename(Named::Group(7))),
                entry("Delete", Command::DeleteGroup(7))
            ]
        );
        assert_eq!(
            entries(Place::Sidebar, &groups()),
            [entry("New group", Command::NewGroup)]
        );
    }

    #[test]
    fn a_tab_pill_offers_rename() {
        assert_eq!(
            entries(Place::Tab(1), &[]),
            [entry("Rename", Command::Rename(Named::Tab(1)))]
        );
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
