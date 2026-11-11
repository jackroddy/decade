use crate::server::chrome::draw::Scene;
use crate::server::chrome::menu::{Action, MenuKey, Outcome};
use crate::server::chrome::overlay::{Cursor, Layout, Row, SMALL, Step};
use crate::server::chrome::rename::Named;
use crate::server::workspace::Group;

/// The fewest cells of text a row has room for.
const MIN_COLUMNS: i32 = 16;

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
        screenshots: bool,
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

    /// Turn the pane's accept screenshots mode on or off.
    Screenshots(usize),

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

impl Entry {
    fn destructive(&self) -> bool {
        matches!(self.command, Command::DeleteGroup(_))
    }
}

fn rows(entries: &[Entry]) -> Vec<Row<'_>> {
    (entries.iter())
        .map(|entry| Row {
            destructive: entry.destructive(),
            ..Row::new(&entry.label)
        })
        .collect()
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
            screenshots,
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
            entry(
                if screenshots {
                    "Stop accepting screenshots"
                } else {
                    "Accept screenshots"
                },
                Command::Screenshots(id),
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

    /// The box, in window pixels.
    layout: Layout,
    cursor: Cursor,
}

impl ContextMenu {
    /// The menu of `entries` with its corner at `pointer`, moved left and
    /// up as far as it takes to fit in `window`, or `None` for no entries.
    /// `cell` is the chrome font's cell.
    pub fn open(
        entries: Vec<Entry>,
        pointer: (i32, i32),
        window: (i32, i32),
        cell: (i32, i32),
    ) -> Option<ContextMenu> {
        if entries.is_empty() {
            return None;
        }
        let longest = (entries.iter())
            .map(|entry| entry.label.chars().count())
            .max()
            .unwrap_or(0) as i32;
        let width = SMALL.width(cell, longest.max(MIN_COLUMNS));
        let layout = (Layout::new(&SMALL, cell, width))
            .list(entries.len())
            .end()
            .inside(pointer, window);
        Some(ContextMenu {
            entries,
            layout,
            cursor: Cursor::default(),
        })
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.layout.contains(x, y)
    }

    /// Select the entry under pixel `x`, `y`, and return whether the
    /// selection moved.
    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        let Some(list) = self.layout.list_area() else {
            return false;
        };
        self.cursor.hover(&list, &rows(&self.entries), x, y)
    }

    /// What a left click at pixel `x`, `y` does.
    pub fn click(&mut self, x: i32, y: i32) -> Outcome {
        let list = self.layout.list_area();
        let clicked = list.and_then(|list| self.cursor.click(&list, &rows(&self.entries), x, y));
        match clicked {
            Some(index) => self.run(index),
            None if self.contains(x, y) => Outcome::Open,
            None => Outcome::Close,
        }
    }

    pub fn key(&mut self, key: MenuKey) -> Outcome {
        match self.cursor.key(key, &rows(&self.entries)) {
            Step::Run(index) => self.run(index),
            Step::Close => Outcome::Close,
            Step::Stay | Step::Pass(_) => Outcome::Open,
        }
    }

    fn run(&self, index: usize) -> Outcome {
        Outcome::Run(Action::Command(self.entries[index].command))
    }

    /// Draw the menu in window pixels, with the chrome font.
    pub fn draw(&self, scene: &mut Scene) {
        self.layout.draw_box(scene);
        let Some(list) = self.layout.list_area() else {
            return;
        };
        for (index, row) in rows(&self.entries).iter().enumerate() {
            list.draw_row(scene, index, row, index == self.cursor.selected);
        }
    }
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
            screenshots: false,
        };
        ContextMenu::open(entries(place, &[]), (100, 100), WINDOW, CELL).unwrap()
    }

    #[test]
    fn a_pane_offers_pin_zoom_scratch_screenshots_and_rename_labelled_for_what_a_click_does() {
        let plain = Place::Pane {
            id: 4,
            pinned: false,
            zoomed: false,
            scratch: false,
            screenshots: false,
        };
        assert_eq!(
            labels(plain),
            [
                "Pin",
                "Zoom",
                "Mark as scratch",
                "Accept screenshots",
                "Rename"
            ]
        );
        let commands: Vec<Command> = entries(plain, &[]).iter().map(|e| e.command).collect();
        assert_eq!(
            commands,
            [
                Command::Pin(4),
                Command::Zoom(4),
                Command::Scratch(4),
                Command::Screenshots(4),
                Command::Rename(Named::Pane(4))
            ]
        );
        let set = Place::Pane {
            id: 4,
            pinned: true,
            zoomed: true,
            scratch: true,
            screenshots: true,
        };
        assert_eq!(
            labels(set),
            [
                "Unpin",
                "Unzoom",
                "Unmark as scratch",
                "Stop accepting screenshots",
                "Rename"
            ]
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
    fn delete_is_the_only_destructive_entry() {
        let destructive: Vec<String> = (entries(Place::Group(7), &groups()).into_iter())
            .filter(Entry::destructive)
            .map(|entry| entry.label)
            .collect();
        assert_eq!(destructive, ["Delete"]);
        let pane = pane_menu();
        assert!(!pane.entries.iter().any(Entry::destructive));
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
        assert_eq!(menu.layout.at, (100, 100));

        // "Accept screenshots" is 18 cells, with the padding and the
        // row inset each side, and each row is 22 px
        let size = (18 * 8 + 2 * (6 + 6), 2 * 6 + 5 * 22);
        assert_eq!((menu.layout.width, menu.layout.height), size);

        let corner = ContextMenu::open(menu.entries.clone(), (795, 590), WINDOW, CELL).unwrap();
        assert_eq!(corner.layout.at, (WINDOW.0 - size.0, WINDOW.1 - size.1));
        let tiny = ContextMenu::open(menu.entries.clone(), (50, 50), (40, 40), CELL).unwrap();
        assert_eq!(tiny.layout.at, (0, 0));
    }

    #[test]
    fn up_and_down_move_the_selection_and_stop_at_the_first_and_last_entry() {
        let mut menu = pane_menu();
        assert_eq!(menu.cursor.selected, 0);
        assert_eq!(menu.key(MenuKey::Up), Outcome::Open);
        assert_eq!(menu.cursor.selected, 0);
        for _ in 0..5 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.cursor.selected, 4);
        assert_eq!(
            menu.key(MenuKey::Enter),
            Outcome::Run(Action::Command(Command::Rename(Named::Pane(4))))
        );
        for _ in 0..3 {
            menu.key(MenuKey::Up);
        }
        assert_eq!(
            menu.key(MenuKey::Enter),
            Outcome::Run(Action::Command(Command::Zoom(4)))
        );
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Close);
    }

    #[test]
    fn hovering_selects_an_entry_and_a_click_runs_it_or_dismisses_outside() {
        let mut menu = pane_menu();
        let (x, y) = (100 + 6 + 8, 100 + 6 + 22 + 10);
        assert!(menu.hover(x, y));
        assert_eq!(menu.cursor.selected, 1);
        assert!(!menu.hover(x, y));
        assert_eq!(
            menu.click(x, y),
            Outcome::Run(Action::Command(Command::Zoom(4)))
        );
        assert_eq!(menu.click(102, 102), Outcome::Open);
        assert_eq!(menu.click(99, 102), Outcome::Close);
    }
}
