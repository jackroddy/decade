use std::path::{Path, PathBuf};

use smithay::utils::{Physical, Size};

use crate::apps::App;
use crate::draw::{Face, Scene};
use crate::palette::{self, Rgba};
use crate::settings::Settings;
use crate::text::{fit, tail};

/// The width in cells of the palette, the New workspace browser and the settings panel.
const WIDTH: i32 = 40;
const APP_ICON: char = '\u{f08c6}';
const FOLDER_ICON: char = '\u{f024b}';
const OPEN_ICON: char = '\u{f0770}';
const CLOSE_ICON: char = '\u{f0156}';
const MAX_ROWS: usize = 10;

/// The cells a row's label may take, after its icon and before its chevron.
const LABEL_ROOM: usize = WIDTH as usize - 5;
const BOX: Rgba = palette::DARK_BACKGROUND;
const BORDER: Rgba = palette::ACCENT;
const TEXT: Rgba = palette::FOREGROUND;
const DIM: Rgba = palette::LIGHT_FOREGROUND;
const SELECTED: Rgba = palette::ACCENT;
const SELECTED_TEXT: Rgba = palette::DARK_BACKGROUND;

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

/// What the palette, the settings panel and the workspace picker ask the
/// compositor to do.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    Launch(Vec<String>),
    Settings,
    NewWorkspace(PathBuf),
    CloseWorkspace(u32),
    CloseTab {
        workspace: u32,
        tab: usize,
    },
    ShowWorkspace(u32),

    /// Open the palette at New workspace.
    StartNewWorkspace,
    MoveWorkspace {
        from: usize,
        to: usize,
    },
    ChangeSettings(Settings),
}

/// The result of a key or a click in an overlay.
#[derive(Clone, PartialEq, Debug)]
pub enum Outcome {
    Open,
    Close,

    /// Close the overlay, then run the action.
    Run(Action),

    /// Run the action and leave the overlay open.
    Apply(Action),
}

struct Row {
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

    /// The rows opened from the root, one for each step of `path`.
    fn trail(&self) -> impl Iterator<Item = &Row> {
        let mut rows = self.root.as_slice();
        self.path.iter().map(move |&index| {
            let row = &rows[index];
            rows = match &row.target {
                Target::Submenu(rows) => rows,
                _ => &[],
            };
            row
        })
    }

    fn opened(&self) -> Option<&Row> {
        self.trail().last()
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

    pub fn draw(&self, scene: &mut Scene) -> Size<i32, Physical> {
        let title: Vec<&str> = std::iter::once("decade")
            .chain(self.trail().map(|row| row.label.as_str()))
            .collect();
        let query = format!("› {}▏", tail(&self.query, WIDTH as usize - 3));
        let matching = self.matching();
        let shown = matching.len().clamp(1, MAX_ROWS);
        let browsing = self.browsing();
        let hint = if browsing {
            "tab complete · enter open · esc back"
        } else {
            "enter open · esc back"
        };
        let (rows, size) = draw_box(scene, &[&title.join(" › "), &query], shown, hint);
        if matching.is_empty() {
            let empty = if browsing { "no folders" } else { "no matches" };
            scene.text_px(rows.left, rows.text_y(0), empty, Face::Menu, DIM);
        }
        let (cell_width, _) = scene.cell();
        let skip = self.selected.saturating_sub(MAX_ROWS - 1);
        for (i, (_, row)) in matching.iter().enumerate().skip(skip).take(MAX_ROWS) {
            let slot = i - skip;
            let fg = rows.select(scene, slot, i == self.selected);
            let y = rows.text_y(slot);
            if row.icon != ' ' {
                scene.text_px(rows.left, y, &row.icon.to_string(), Face::Menu, fg);
            }
            let label = fit(&row.label, LABEL_ROOM);
            scene.text_px(rows.left + cell_width * 3, y, &label, Face::Menu, fg);
            if matches!(row.target, Target::Submenu(_) | Target::Directory) {
                scene.text_px(rows.right - cell_width, y, "›", Face::Menu, fg);
            }
        }
        size
    }
}

/// The list rows of a box drawn by `draw_box`.
pub struct Rows {
    /// The x where row text starts.
    pub left: i32,

    /// The x where row text must end.
    pub right: i32,
    top: i32,
    height: i32,
    text: i32,
}

impl Rows {
    pub fn text_y(&self, slot: usize) -> i32 {
        self.top + slot as i32 * self.height + self.text
    }

    /// Draw the selection behind row `slot` when it is selected, and return
    /// the colour of the row's text.
    pub fn select(&self, scene: &mut Scene, slot: usize, selected: bool) -> Rgba {
        if !selected {
            return TEXT;
        }
        let pad = self.left;
        let top = self.top + slot as i32 * self.height;
        let size = (self.right, self.height);
        scene.rounded(pad / 2, top, size, pad / 2, SELECTED, SELECTED);
        SELECTED_TEXT
    }
}

/// Draw the box of the palette or the settings panel, with `header` above
/// its rule, title first, room for `rows` rows under it and `hint` last.
pub fn draw_box(
    scene: &mut Scene,
    header: &[&str],
    rows: usize,
    hint: &str,
) -> (Rows, Size<i32, Physical>) {
    let (cell_width, cell_height) = scene.cell();
    let pad = cell_height * 2 / 3;
    let row_height = cell_height * 8 / 5;
    let width = WIDTH * cell_width + pad * 2;
    let below_header = pad + row_height * header.len() as i32;
    let list = Rows {
        left: pad,
        right: width - pad,
        top: below_header + pad / 2,
        height: row_height,
        text: (row_height - cell_height) / 2,
    };
    let height = list.top + row_height * (rows as i32 + 1) + pad;
    scene.rounded(0, 0, (width, height), pad, BOX, BORDER);
    for (i, line) in header.iter().enumerate() {
        let color = if i == 0 { BORDER } else { TEXT };
        let y = pad + i as i32 * row_height + list.text;
        scene.text_px(pad, y, line, Face::Menu, color);
    }
    let line = scene.line_width();
    let rule = below_header + pad / 4 - line / 2;
    scene.rect(pad, rule, width - pad * 2, line, DIM);
    scene.text_px(pad, list.text_y(rows), hint, Face::Menu, DIM);
    (list, (width, height).into())
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
        let label = tail(&format!("Open {query}"), LABEL_ROOM);
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
