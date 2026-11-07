use std::path::{Path, PathBuf};

use smithay::utils::{Physical, Size};

use crate::apps::App;
use crate::draw::{Face, Scene};
use crate::palette;
use crate::text::Font;

const WIDTH: i32 = 22;
const BROWSE_WIDTH: i32 = 48;
const APP_ICON: char = '\u{f08c6}';
const FOLDER_ICON: char = '\u{f024b}';
const OPEN_ICON: char = '\u{f0770}';
const CLOSE_ICON: char = '\u{f0156}';
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
    Tab,
    Char(char),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Launch(Vec<String>),
    Settings,
    NewWorkspace(PathBuf),
    CloseWorkspace(u32),
    CloseTab { workspace: u32, tab: usize },
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
    target: Target,
}

enum Target {
    Submenu(Vec<Row>),
    Run(Action),

    /// A row that turns the query into a path and lists the directories under it.
    Directory,

    /// A row that replaces the query with this text.
    Complete(String),
}

pub struct Menu {
    root: Vec<Row>,
    path: Vec<usize>,
    query: String,
    selected: usize,
    home: PathBuf,

    /// The rows listed for the query while a `Directory` row is open.
    found: Vec<Row>,
}

impl Row {
    fn new(icon: char, label: &str, target: Target) -> Row {
        Row {
            icon,
            label: label.to_string(),
            target,
        }
    }

    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
    }
}

impl Menu {
    pub fn new(apps: Vec<App>, home: PathBuf, workspace: u32, name: &str, tab: usize) -> Menu {
        let launch = apps
            .into_iter()
            .map(|app| Row::new(APP_ICON, &app.name, Target::Run(Action::Launch(app.argv))))
            .collect();
        let confirm = format!("Close {name} and its panes");
        let close = vec![Row::new(
            CLOSE_ICON,
            &confirm,
            Target::Run(Action::CloseWorkspace(workspace)),
        )];
        let confirm = format!("Close tab {}", tab + 1);
        let close_tab = vec![Row::new(
            CLOSE_ICON,
            &confirm,
            Target::Run(Action::CloseTab { workspace, tab }),
        )];
        let rows = vec![
            Row::new('\u{f003b}', "Launch", Target::Submenu(launch)),
            Row::new(FOLDER_ICON, "New workspace", Target::Directory),
            Row::new(CLOSE_ICON, "Close tab", Target::Submenu(close_tab)),
            Row::new(CLOSE_ICON, "Close workspace", Target::Submenu(close)),
            Row::new('\u{f0493}', "Settings", Target::Run(Action::Settings)),
        ];
        Menu {
            root: rows,
            path: Vec::new(),
            query: String::new(),
            selected: 0,
            home,
            found: Vec::new(),
        }
    }

    fn opened(&self) -> Option<&Row> {
        let mut rows = self.root.as_slice();
        let mut row = None;
        for &index in &self.path {
            row = Some(&rows[index]);
            rows = match &rows[index].target {
                Target::Submenu(rows) => rows,
                _ => &[],
            };
        }
        row
    }

    fn browsing(&self) -> bool {
        self.opened()
            .is_some_and(|row| matches!(row.target, Target::Directory))
    }

    fn rows(&self) -> &[Row] {
        match self.opened().map(|row| &row.target) {
            None => &self.root,
            Some(Target::Submenu(rows)) => rows,
            Some(Target::Directory) => &self.found,
            Some(_) => &[],
        }
    }

    fn matching(&self) -> Vec<(usize, &Row)> {
        let browsing = self.browsing();
        self.rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| browsing || row.matches(&self.query))
            .collect()
    }

    fn query_changed(&mut self) {
        self.selected = 0;
        if self.browsing() {
            self.found = browse(&self.query, &self.home);
        }
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
                self.query_changed();
            }
            MenuKey::Backspace if self.query.is_empty() => return self.up(),
            MenuKey::Backspace => {
                self.query.pop();
                self.query_changed();
            }
            MenuKey::Escape => return self.up(),
            MenuKey::Left | MenuKey::Right => {}
            MenuKey::Tab => self.complete(),
            MenuKey::Enter => return self.enter(),
        }
        Outcome::Open
    }

    fn enter(&mut self) -> Outcome {
        let Some(&(index, row)) = self.matching().get(self.selected) else {
            return Outcome::Open;
        };
        match &row.target {
            Target::Run(action) => return Outcome::Run(action.clone()),
            Target::Submenu(_) => {
                self.path.push(index);
                self.query.clear();
                self.query_changed();
            }
            Target::Directory => self.open_directory(index),
            Target::Complete(_) => self.complete(),
        }
        Outcome::Open
    }

    fn open_directory(&mut self, index: usize) {
        self.path.push(index);
        self.query = "~/".to_string();
        self.query_changed();
    }

    pub fn start_new_workspace(&mut self) {
        let directory = self
            .root
            .iter()
            .position(|row| matches!(row.target, Target::Directory));
        if let Some(index) = directory {
            self.path.clear();
            self.open_directory(index);
        }
    }

    fn complete(&mut self) {
        if let Some((_, row)) = self.matching().get(self.selected)
            && let Target::Complete(query) = &row.target
        {
            self.query = query.clone();
            self.query_changed();
        }
    }

    fn up(&mut self) -> Outcome {
        if self.path.pop().is_none() {
            return Outcome::Close;
        }
        self.query.clear();
        self.found.clear();
        self.selected = 0;
        Outcome::Open
    }

    pub fn draw(&self, scene: &mut Scene, font: &Font) -> Size<i32, Physical> {
        let (cell_width, cell_height) = (font.cell_width as i32, font.cell_height as i32);
        let pad = cell_height * 2 / 3;
        let row_height = cell_height * 8 / 5;
        let columns = if self.browsing() { BROWSE_WIDTH } else { WIDTH };
        let width = columns * cell_width + pad * 2;
        let list = pad + row_height * 2;
        let height = list + row_height * MAX_ROWS as i32 + pad;
        scene.rounded(0, 0, (width, height), pad, BOX, BORDER);

        let text_y = |top: i32| top + (row_height - cell_height) / 2;
        let mut title = String::from("decade");
        let mut rows = self.root.as_slice();
        for &index in &self.path {
            title.push_str(" › ");
            title.push_str(&rows[index].label);
            rows = match &rows[index].target {
                Target::Submenu(rows) => rows,
                _ => &[],
            };
        }
        scene.text_px(pad, text_y(pad), &title, Face::Menu, BORDER);
        let query = format!("› {}▏", tail(&self.query, columns as usize - 3));
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
                &tail(&row.label, columns as usize - 5),
                Face::Menu,
                fg,
            );
            if matches!(row.target, Target::Submenu(_) | Target::Directory) {
                let x = width - pad - cell_width;
                scene.text_px(x, text_y(top), "›", Face::Menu, fg);
            }
        }
        (width, height).into()
    }
}

fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count + 1 - max).collect();
    format!("…{kept}")
}

fn expand(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(path),
    }
}

/// The rows for a path typed into New workspace: an Open row when the path is
/// a directory, then the directories that complete its last component.
fn browse(query: &str, home: &Path) -> Vec<Row> {
    let Some(slash) = query.rfind('/') else {
        return Vec::new();
    };
    let (parent, partial) = query.split_at(slash + 1);
    let mut rows = Vec::new();
    let target = expand(query, home);
    if target.is_dir() && partial != "." && partial != ".." {
        let label = format!("Open {query}");
        rows.push(Row::new(
            OPEN_ICON,
            &label,
            Target::Run(Action::NewWorkspace(target)),
        ));
    }
    let Ok(entries) = std::fs::read_dir(expand(parent, home)) else {
        return rows;
    };
    let partial_lower = partial.to_lowercase();
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| partial.starts_with('.') || !name.starts_with('.'))
        .filter(|name| name.to_lowercase().starts_with(&partial_lower))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    rows.extend(names.into_iter().map(|name| {
        let query = format!("{parent}{name}/");
        Row::new(FOLDER_ICON, &name, Target::Complete(query))
    }));
    rows
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
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 0, "decade", 0);
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
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 0, "decade", 0);
        menu.key(MenuKey::Up);
        assert_eq!(menu.selected, 0);
        for _ in 0..5 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.selected, 4);
    }

    #[test]
    fn enter_on_an_app_runs_its_command() {
        let app = App {
            name: "Files".to_string(),
            argv: vec!["nautilus".to_string()],
        };
        let mut menu = Menu::new(vec![app], PathBuf::from("/"), 0, "decade", 0);
        menu.key(MenuKey::Enter);
        for ch in "fil".chars() {
            menu.key(MenuKey::Char(ch));
        }
        let launch = Action::Launch(vec!["nautilus".to_string()]);
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run(launch));
    }

    #[test]
    fn closing_a_workspace_takes_a_second_enter_on_a_row_naming_it() {
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 4, "decade", 0);
        for ch in "close w".chars() {
            menu.key(MenuKey::Char(ch));
        }
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Open);
        assert_eq!(labels(&menu), ["Close decade and its panes"]);
        let close = Action::CloseWorkspace(4);
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run(close));
    }

    #[test]
    fn closing_a_tab_takes_a_second_enter_on_a_row_naming_it() {
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 4, "decade", 1);
        for ch in "close t".chars() {
            menu.key(MenuKey::Char(ch));
        }
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Open);
        assert_eq!(labels(&menu), ["Close tab 2"]);
        let close = Action::CloseTab {
            workspace: 4,
            tab: 1,
        };
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run(close));
    }

    #[test]
    fn enter_opens_a_submenu_and_escape_goes_up_then_closes() {
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 0, "decade", 0);
        menu.key(MenuKey::Enter);
        assert_eq!(menu.path, [0]);
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Open);
        assert!(menu.path.is_empty());
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Close);
    }

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("decade-menu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["projects/decade", "projects/foam", "Pictures", ".config"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("projects/notes.txt"), "").unwrap();
        root
    }

    fn browsing(home: &Path, typed: &str) -> Menu {
        let mut menu = Menu::new(Vec::new(), home.to_path_buf(), 0, "decade", 0);
        for ch in "new".chars() {
            menu.key(MenuKey::Char(ch));
        }
        menu.key(MenuKey::Enter);
        for ch in typed.chars() {
            menu.key(MenuKey::Char(ch));
        }
        menu
    }

    #[test]
    fn new_workspace_lists_the_directories_under_the_home_directory() {
        let home = tree("home");
        let menu = browsing(&home, "");
        assert_eq!(menu.query, "~/");
        assert_eq!(labels(&menu), ["Open ~/", "Pictures", "projects"]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_last_part_of_the_path_narrows_the_directories_and_skips_files() {
        let home = tree("narrow");
        let menu = browsing(&home, "projects/");
        assert_eq!(labels(&menu), ["Open ~/projects/", "decade", "foam"]);
        let menu = browsing(&home, "projects/f");
        assert_eq!(labels(&menu), ["foam"]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn hidden_directories_are_listed_once_the_part_starts_with_a_dot() {
        let home = tree("hidden");
        let menu = browsing(&home, ".");
        assert_eq!(labels(&menu), [".config"]);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn tab_completes_the_selected_directory_and_enter_on_open_runs_it() {
        let home = tree("complete");
        let mut menu = browsing(&home, "pro");
        menu.key(MenuKey::Tab);
        assert_eq!(menu.query, "~/projects/");
        menu.key(MenuKey::Down);
        menu.key(MenuKey::Enter);
        assert_eq!(menu.query, "~/projects/decade/");
        let open = Action::NewWorkspace(home.join("projects/decade/"));
        assert_eq!(menu.key(MenuKey::Enter), Outcome::Run(open));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn escape_leaves_new_workspace_with_an_empty_query() {
        let home = tree("escape");
        let mut menu = browsing(&home, "projects/");
        assert_eq!(menu.key(MenuKey::Escape), Outcome::Open);
        assert_eq!(menu.query, "");
        assert_eq!(
            labels(&menu),
            [
                "Launch",
                "New workspace",
                "Close tab",
                "Close workspace",
                "Settings"
            ]
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
