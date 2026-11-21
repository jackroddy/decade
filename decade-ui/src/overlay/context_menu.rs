use crate::Scene;
use crate::overlay::{Cursor, Layout, MenuKey, Outcome, Row, SMALL, Step};

/// The fewest cells of text a row has room for.
const MIN_COLUMNS: i32 = 16;

/// An entry of a context menu, with `A` what it runs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry<A> {
    pub label: String,
    pub action: A,
    pub destructive: bool,

    /// The label a first click turns the entry into, drawn destructive,
    /// with a second click running the action, or `None` to run on the
    /// first click.
    pub confirm: Option<String>,
}

fn rows<A>(entries: &[Entry<A>], armed: Option<usize>) -> Vec<Row<'_>> {
    (entries.iter().enumerate())
        .map(|(index, entry)| match &entry.confirm {
            Some(confirm) if armed == Some(index) => Row {
                destructive: true,
                ..Row::new(confirm)
            },
            _ => Row {
                destructive: entry.destructive,
                ..Row::new(&entry.label)
            },
        })
        .collect()
}

/// The menu a right click opens, at the pointer and inside the window.
pub struct ContextMenu<A> {
    entries: Vec<Entry<A>>,

    /// The box, in window pixels.
    layout: Layout,
    cursor: Cursor,

    /// The entry a first click turned into its confirm label.
    armed: Option<usize>,
}

impl<A: Clone> ContextMenu<A> {
    /// The menu of `entries` with its corner at `pointer`, moved left and
    /// up as far as it takes to fit in `window`, or `None` for no entries.
    /// `cell` is the small font's cell.
    pub fn open(
        entries: Vec<Entry<A>>,
        pointer: (i32, i32),
        window: (i32, i32),
        cell: (i32, i32),
    ) -> Option<ContextMenu<A>> {
        if entries.is_empty() {
            return None;
        }
        let longest = (entries.iter())
            .flat_map(|entry| [Some(&entry.label), entry.confirm.as_ref()])
            .flatten()
            .map(|label| label.chars().count())
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
            armed: None,
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
        self.cursor
            .hover(&list, &rows(&self.entries, self.armed), x, y)
    }

    /// What a left click at pixel `x`, `y` does.
    pub fn click(&mut self, x: i32, y: i32) -> Outcome<A> {
        let list = self.layout.list_area();
        let clicked = list.and_then(|list| {
            self.cursor
                .click(&list, &rows(&self.entries, self.armed), x, y)
        });
        match clicked {
            Some(index) => self.run(index),
            None if self.contains(x, y) => Outcome::Open,
            None => Outcome::Close,
        }
    }

    pub fn key(&mut self, key: MenuKey) -> Outcome<A> {
        match self.cursor.key(key, &rows(&self.entries, self.armed)) {
            Step::Run(index) => self.run(index),
            Step::Close => Outcome::Close,
            Step::Stay | Step::Pass(_) => Outcome::Open,
        }
    }

    fn run(&mut self, index: usize) -> Outcome<A> {
        let entry = &self.entries[index];
        if entry.confirm.is_some() && self.armed != Some(index) {
            self.armed = Some(index);
            return Outcome::Open;
        }
        Outcome::Run(entry.action.clone())
    }

    /// Draw the menu in window pixels, with the small font.
    pub fn draw(&self, scene: &mut Scene) {
        self.layout.draw_box(scene);
        let Some(list) = self.layout.list_area() else {
            return;
        };
        for (index, row) in rows(&self.entries, self.armed).iter().enumerate() {
            list.draw_row(scene, index, row, index == self.cursor.selected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (i32, i32) = (8, 18);
    const WINDOW: (i32, i32) = (800, 600);

    fn entries() -> Vec<Entry<char>> {
        let entry = |label: &str, action| Entry {
            label: label.to_string(),
            action,
            destructive: false,
            confirm: None,
        };
        vec![
            entry("Cut", 'x'),
            entry("Copy", 'c'),
            entry("Paste", 'v'),
            entry("Paste without style", 's'),
            entry("Select all", 'a'),
        ]
    }

    fn menu() -> ContextMenu<char> {
        ContextMenu::open(entries(), (100, 100), WINDOW, CELL).unwrap()
    }

    #[test]
    fn no_entries_open_no_menu() {
        assert!(ContextMenu::<char>::open(Vec::new(), (100, 100), WINDOW, CELL).is_none());
    }

    #[test]
    fn the_menu_opens_at_the_pointer_and_moves_inside_the_window_near_an_edge() {
        let menu = menu();
        assert_eq!(menu.layout.at, (100, 100));

        // "Paste without style" is 19 cells, with the padding and the
        // row inset each side, and each row is 22 px
        let size = (19 * 8 + 2 * (6 + 6), 2 * 6 + 5 * 22);
        assert_eq!((menu.layout.width, menu.layout.height), size);

        let corner = ContextMenu::open(entries(), (795, 590), WINDOW, CELL).unwrap();
        assert_eq!(corner.layout.at, (WINDOW.0 - size.0, WINDOW.1 - size.1));
        let tiny = ContextMenu::open(entries(), (50, 50), (40, 40), CELL).unwrap();
        assert_eq!(tiny.layout.at, (0, 0));
    }

    #[test]
    fn up_and_down_move_the_selection_and_stop_at_the_first_and_last_entry() {
        let mut menu = menu();
        assert_eq!(menu.cursor.selected, 0);
        assert_eq!(menu.key(MenuKey::Up), Outcome::Open);
        assert_eq!(menu.cursor.selected, 0);
        for _ in 0..5 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.cursor.selected, 4);
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run('a'));
        for _ in 0..3 {
            menu.key(MenuKey::Up);
        }
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run('c'));
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Close);
    }

    #[test]
    fn hovering_selects_an_entry_and_a_click_runs_it_or_dismisses_outside() {
        let mut menu = menu();
        let (x, y) = (100 + 6 + 8, 100 + 6 + 22 + 10);
        assert!(menu.hover(x, y));
        assert_eq!(menu.cursor.selected, 1);
        assert!(!menu.hover(x, y));
        assert_eq!(menu.click(x, y), Outcome::Run('c'));
        assert_eq!(menu.click(102, 102), Outcome::Open);
        assert_eq!(menu.click(99, 102), Outcome::Close);
    }

    #[test]
    fn an_entry_with_a_confirm_label_turns_into_it_on_a_first_click_and_runs_on_a_second() {
        let mut entries = entries();
        entries[1].confirm = Some("Copy everything".to_string());
        let mut menu = ContextMenu::open(entries, (100, 100), WINDOW, CELL).unwrap();
        let (x, y) = (100 + 6 + 8, 100 + 6 + 22 + 10);
        assert_eq!(menu.click(x, y), Outcome::Open);
        let shown = rows(&menu.entries, menu.armed);
        assert_eq!(shown[1].label, "Copy everything");
        assert!(shown[1].destructive);
        assert_eq!(shown[0].label, "Cut");
        assert_eq!(menu.click(x, y), Outcome::Run('c'));
    }

    #[test]
    fn enter_on_an_entry_with_a_confirm_label_arms_it_first_and_runs_it_second() {
        let mut entries = entries();
        entries[0].confirm = Some("Cut everything".to_string());
        let mut menu = ContextMenu::open(entries, (100, 100), WINDOW, CELL).unwrap();
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Open);
        assert_eq!(rows(&menu.entries, menu.armed)[0].label, "Cut everything");
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run('x'));
    }

    #[test]
    fn the_menu_is_wide_enough_for_a_confirm_label_longer_than_every_label() {
        let mut entries = entries();
        entries[0].confirm = Some("Cut every one of the lines".to_string());
        let menu = ContextMenu::open(entries, (100, 100), WINDOW, CELL).unwrap();
        assert_eq!(menu.layout.width, 26 * 8 + 2 * (6 + 6));
    }
}
