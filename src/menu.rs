use smithay::utils::{Physical, Size};

use crate::apps::App;
use crate::draw::{Face, Scene};
use crate::palette;
use crate::text::Font;

const WIDTH: i32 = 22;
const APP_ICON: char = '\u{f08c6}';
const MAX_ROWS: usize = 10;
pub const BOX: [f32; 4] = palette::DARK_BACKGROUND;
pub const BORDER: [f32; 4] = palette::ACCENT;
pub const TEXT: [f32; 4] = palette::FOREGROUND;
pub const DIM: [f32; 4] = palette::LIGHT_FOREGROUND;
pub const SELECTED: [f32; 4] = palette::ACCENT;
pub const SELECTED_TEXT: [f32; 4] = palette::DARK_BACKGROUND;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Backspace,
    Char(char),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Launch(Vec<String>),
    Settings,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Open,
    Close,
    Run(Action),
}

pub struct Row {
    icon: char,
    label: String,
    submenu: Option<Vec<Row>>,
    action: Option<Action>,
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
            action: None,
        }
    }

    fn action(icon: char, label: &str, action: Action) -> Row {
        Row {
            icon,
            label: label.to_string(),
            submenu: None,
            action: Some(action),
        }
    }

    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
    }
}

impl Menu {
    pub fn new(apps: Vec<App>) -> Menu {
        let launch = apps
            .into_iter()
            .map(|app| Row::action(APP_ICON, &app.name, Action::Launch(app.argv)))
            .collect();
        let rows = vec![
            Row::submenu('\u{f003b}', "Launch", launch),
            Row::action('\u{f0493}', "Settings", Action::Settings),
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

    pub fn key(&mut self, key: MenuKey) -> Outcome {
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
            MenuKey::Left | MenuKey::Right => {}
            MenuKey::Enter => return self.enter(),
        }
        Outcome::Open
    }

    fn enter(&mut self) -> Outcome {
        let Some(&(index, row)) = self.matching().get(self.selected) else {
            return Outcome::Open;
        };
        if let Some(action) = &row.action {
            return Outcome::Run(action.clone());
        }
        if row.submenu.is_some() {
            self.path.push(index);
            self.query.clear();
            self.selected = 0;
        }
        Outcome::Open
    }

    fn up(&mut self) -> Outcome {
        if self.path.pop().is_none() {
            return Outcome::Close;
        }
        self.query.clear();
        self.selected = 0;
        Outcome::Open
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
        let mut menu = Menu::new(Vec::new());
        menu.key(MenuKey::Char('l'));
        menu.key(MenuKey::Char('a'));
        assert_eq!(labels(&menu), ["Launch"]);
        menu.key(MenuKey::Char('y'));
        assert_eq!(labels(&menu), Vec::<&str>::new());
        menu.key(MenuKey::Backspace);
        assert_eq!(labels(&menu), ["Launch"]);
    }

    #[test]
    fn the_selection_stays_inside_the_list() {
        let mut menu = Menu::new(Vec::new());
        menu.key(MenuKey::Up);
        assert_eq!(menu.selected, 0);
        for _ in 0..5 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.selected, 1);
    }

    #[test]
    fn enter_on_an_app_runs_its_command() {
        let app = App {
            name: "Files".to_string(),
            argv: vec!["nautilus".to_string()],
        };
        let mut menu = Menu::new(vec![app]);
        menu.key(MenuKey::Enter);
        for ch in "fil".chars() {
            menu.key(MenuKey::Char(ch));
        }
        let launch = Action::Launch(vec!["nautilus".to_string()]);
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run(launch));
    }

    #[test]
    fn enter_opens_a_submenu_and_escape_goes_up_then_closes() {
        let mut menu = Menu::new(Vec::new());
        menu.key(MenuKey::Enter);
        assert_eq!(menu.path, [0]);
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Open);
        assert!(menu.path.is_empty());
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Close);
    }
}
