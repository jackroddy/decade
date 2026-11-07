use smithay::utils::{Physical, Size};

use crate::draw::{Face, Scene};
use crate::palette;
use crate::text::Font;

pub const MENU_PX: f32 = 20.0;
const WIDTH: i32 = 22;
const MAX_ROWS: usize = 10;
const BOX: [f32; 4] = palette::DARK_BACKGROUND;
const BORDER: [f32; 4] = palette::ACCENT;
const TEXT: [f32; 4] = palette::FOREGROUND;
const DIM: [f32; 4] = palette::LIGHT_FOREGROUND;
const SELECTED: [f32; 4] = palette::ACCENT;
const SELECTED_TEXT: [f32; 4] = palette::DARK_BACKGROUND;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKey {
    Up,
    Down,
    Enter,
    Escape,
    Backspace,
    Char(char),
}

pub struct Row {
    icon: char,
    label: String,
    submenu: Option<Vec<Row>>,
}

pub struct Menu {
    root: Vec<Row>,
    path: Vec<usize>,
    query: String,
    selected: usize,
}

impl Row {
    fn submenu(icon: char, label: &str, rows: Vec<Row>) -> Row {
        Row {
            icon,
            label: label.to_string(),
            submenu: Some(rows),
        }
    }

    fn note(label: &str) -> Row {
        Row {
            icon: ' ',
            label: label.to_string(),
            submenu: None,
        }
    }

    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
    }
}

impl Menu {
    pub fn new() -> Menu {
        let soon = || vec![Row::note("coming soon")];
        let rows = vec![
            Row::submenu('\u{f003b}', "Launch", soon()),
            Row::submenu('\u{f0570}', "Layout", soon()),
            Row::submenu('\u{f0493}', "Settings", soon()),
        ];
        Menu {
            root: rows,
            path: Vec::new(),
            query: String::new(),
            selected: 0,
        }
    }

    fn rows(&self) -> &[Row] {
        let mut rows = self.root.as_slice();
        for &index in &self.path {
            rows = rows[index].submenu.as_deref().unwrap_or_default();
        }
        rows
    }

    fn matching(&self) -> Vec<(usize, &Row)> {
        self.rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.matches(&self.query))
            .collect()
    }

    pub fn key(&mut self, key: MenuKey) -> bool {
        match key {
            MenuKey::Up => self.selected = self.selected.saturating_sub(1),
            MenuKey::Down => {
                let last = self.matching().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
            }
            MenuKey::Char(ch) => {
                self.query.push(ch);
                self.selected = 0;
            }
            MenuKey::Backspace if self.query.is_empty() => return self.up(),
            MenuKey::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            MenuKey::Escape => return self.up(),
            MenuKey::Enter => self.enter(),
        }
        true
    }

    fn enter(&mut self) {
        let Some(&(index, row)) = self.matching().get(self.selected) else {
            return;
        };
        if row.submenu.is_none() {
            return;
        }
        self.path.push(index);
        self.query.clear();
        self.selected = 0;
    }

    fn up(&mut self) -> bool {
        if self.path.pop().is_none() {
            return false;
        }
        self.query.clear();
        self.selected = 0;
        true
    }

    pub fn draw(&self, scene: &mut Scene, font: &Font) -> Size<i32, Physical> {
        let (cell_width, cell_height) = (font.cell_width as i32, font.cell_height as i32);
        let pad = cell_height * 2 / 3;
        let row_height = cell_height * 8 / 5;
        let width = WIDTH * cell_width + pad * 2;
        let list = pad + row_height * 2;
        let height = list + row_height * MAX_ROWS as i32 + pad;
        scene.rounded(0, 0, (width, height), pad, BOX, BORDER);

        let text_y = |top: i32| top + (row_height - cell_height) / 2;
        let mut title = String::from("decade");
        let mut rows = self.root.as_slice();
        for &index in &self.path {
            title.push_str(" › ");
            title.push_str(&rows[index].label);
            rows = rows[index].submenu.as_deref().unwrap_or_default();
        }
        scene.text_px(pad, text_y(pad), &title, Face::Menu, BORDER);
        let query = format!("› {}▏", self.query);
        scene.text_px(pad, text_y(pad + row_height), &query, Face::Menu, TEXT);
        scene.rect(pad, list - 1, width - pad * 2, scene.line_width(), DIM);

        let matching = self.matching();
        if matching.is_empty() {
            scene.text_px(pad, text_y(list), "no matches", Face::Menu, DIM);
        }
        let skip = self.selected.saturating_sub(MAX_ROWS - 1);
        for (i, (_, row)) in matching.iter().enumerate().skip(skip).take(MAX_ROWS) {
            let top = list + (i - skip) as i32 * row_height;
            let fg = if i == self.selected {
                let size = (width - pad, row_height);
                scene.rounded(pad / 2, top, size, pad / 2, SELECTED, SELECTED);
                SELECTED_TEXT
            } else {
                TEXT
            };
            if row.icon != ' ' {
                scene.text_px(pad, text_y(top), &row.icon.to_string(), Face::Menu, fg);
            }
            scene.text_px(
                pad + cell_width * 3,
                text_y(top),
                &row.label,
                Face::Menu,
                fg,
            );
            if row.submenu.is_some() {
                let x = width - pad - cell_width;
                scene.text_px(x, text_y(top), "›", Face::Menu, fg);
            }
        }
        (width, height).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(menu: &Menu) -> Vec<&str> {
        menu.matching()
            .into_iter()
            .map(|(_, row)| row.label.as_str())
            .collect()
    }

    #[test]
    fn typing_filters_the_rows_by_label() {
        let mut menu = Menu::new();
        menu.key(MenuKey::Char('l'));
        menu.key(MenuKey::Char('a'));
        assert_eq!(labels(&menu), ["Launch", "Layout"]);
        menu.key(MenuKey::Char('y'));
        assert_eq!(labels(&menu), ["Layout"]);
        menu.key(MenuKey::Backspace);
        assert_eq!(labels(&menu), ["Launch", "Layout"]);
    }

    #[test]
    fn the_selection_stays_inside_the_list() {
        let mut menu = Menu::new();
        menu.key(MenuKey::Up);
        assert_eq!(menu.selected, 0);
        for _ in 0..5 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.selected, 2);
    }

    #[test]
    fn enter_opens_a_submenu_and_escape_goes_up_then_closes() {
        let mut menu = Menu::new();
        menu.key(MenuKey::Down);
        menu.key(MenuKey::Enter);
        assert_eq!(menu.path, [1]);
        assert!(menu.key(MenuKey::Escape));
        assert!(menu.path.is_empty());
        assert!(!menu.key(MenuKey::Escape));
    }
}
