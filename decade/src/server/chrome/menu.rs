use std::path::{Path, PathBuf};

use crate::server::apps::App;
use crate::server::chrome::context_menu::Command;
use crate::server::chrome::draw::Scene;
use crate::server::chrome::overlay::{self, Cursor, LARGE, LARGE_COLUMNS, Layout, Step};
use crate::server::chrome::palette;
use crate::server::chrome::settings::Settings;
use crate::server::chrome::text::{fit, tail};

const APP_ICON: char = '\u{f08c6}';
const FOLDER_ICON: char = '\u{f024b}';
const OPEN_ICON: char = '\u{f0770}';
const CLOSE_ICON: char = '\u{f0156}';
const MAX_ROWS: usize = 10;

/// The cells a row's label may take, after its icon and before its chevron.
const LABEL_ROOM: usize = LARGE_COLUMNS as usize - 5;

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

/// What the palette, the settings panel, the workspace picker and the
/// context menu ask the compositor to do.
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
    Command(Command),

    /// Rename what the open name field names to the text typed in it.
    Rename,
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
    cursor: Cursor,
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

    /// Whether the row closes something, or opens rows that do.
    fn destructive(&self) -> bool {
        match &self.target {
            Target::Run(Action::CloseTab { .. } | Action::CloseWorkspace(_)) => true,
            Target::Submenu(rows) => rows.iter().any(Row::destructive),
            _ => false,
        }
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
            cursor: Cursor::default(),
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
        self.cursor = Cursor::default();
        if self.browsing() {
            self.found = browse(&self.query, &self.home);
        }
    }

    /// The matching rows as the list takes them.
    fn list_rows(&self) -> Vec<overlay::Row<'_>> {
        (self.matching().into_iter())
            .map(|(_, row)| overlay::Row {
                destructive: row.destructive(),
                ..overlay::Row::new(&row.label)
            })
            .collect()
    }

    pub fn key(&mut self, key: MenuKey) -> Outcome {
        let mut cursor = self.cursor;
        let step = cursor.key(key, &self.list_rows());
        self.cursor = cursor;
        match step {
            Step::Stay | Step::Pass(MenuKey::Left | MenuKey::Right) => {}
            Step::Run(_) => return self.enter(),
            Step::Close => return self.up(),
            Step::Pass(MenuKey::Char(ch)) => {
                self.query.push(ch);
                self.query_changed();
            }
            Step::Pass(MenuKey::Backspace) if self.query.is_empty() => return self.up(),
            Step::Pass(MenuKey::Backspace) => {
                self.query.pop();
                self.query_changed();
            }
            Step::Pass(MenuKey::Tab) => self.complete(),
            Step::Pass(_) => {}
        }
        self.cursor.scroll(MAX_ROWS);
        Outcome::Open
    }

    fn enter(&mut self) -> Outcome {
        let Some(&(index, row)) = self.matching().get(self.cursor.selected) else {
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
        if let Some((_, row)) = self.matching().get(self.cursor.selected)
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
        self.cursor = Cursor::default();
        Outcome::Open
    }

    fn hint(&self) -> &'static [(&'static str, &'static str)] {
        if self.browsing() {
            &[("tab", "complete"), ("enter", "open"), ("esc", "back")]
        } else if self.path.is_empty() {
            &[("enter", "open"), ("esc", "close")]
        } else if self.opened().is_some_and(Row::destructive) {
            &[("enter", "close"), ("esc", "back")]
        } else {
            &[("enter", "open"), ("esc", "back")]
        }
    }

    /// The palette's box in `window`, for the menu font's `cell`: centred
    /// across it, a seventh of the way down.
    fn layout(&self, cell: (i32, i32), (width, height): (i32, i32)) -> Layout {
        let shown = self.matching().len().clamp(1, MAX_ROWS);
        let box_width = LARGE.width(cell, LARGE_COLUMNS).min(width);
        let layout = (Layout::new(&LARGE, cell, box_width))
            .field()
            .list(shown)
            .hint()
            .end();
        let top = (height / 7).min((height - layout.height).max(0));
        layout.at(((width - box_width).max(0) / 2, top))
    }

    /// Select the row under pixel `x`, `y`, and return whether the
    /// selection moved.
    pub fn hover(&mut self, (x, y): (i32, i32), cell: (i32, i32), window: (i32, i32)) -> bool {
        let Some(list) = self.layout(cell, window).list_area() else {
            return false;
        };
        let mut cursor = self.cursor;
        let moved = cursor.hover(&list, &self.list_rows(), x, y);
        self.cursor = cursor;
        moved
    }

    /// What a left click at pixel `x`, `y` does, or `None` outside the box.
    pub fn click(
        &mut self,
        (x, y): (i32, i32),
        cell: (i32, i32),
        window: (i32, i32),
    ) -> Option<Outcome> {
        let layout = self.layout(cell, window);
        if !layout.contains(x, y) {
            return None;
        }
        let list = layout.list_area()?;
        let mut cursor = self.cursor;
        let clicked = cursor.click(&list, &self.list_rows(), x, y);
        self.cursor = cursor;
        Some(match clicked {
            Some(_) => self.enter(),
            None => Outcome::Open,
        })
    }

    /// Draw the palette in `window`, in the scene's pixels.
    pub fn draw(&self, scene: &mut Scene, window: (i32, i32)) {
        let layout = self.layout(scene.cell(), window);
        layout.draw_box(scene);
        let trail: Vec<&str> = self.trail().map(|row| row.label.as_str()).collect();
        let placeholder = if trail.is_empty() {
            "type to search".to_string()
        } else {
            format!("{} › type to search", trail.join(" › "))
        };
        let placeholder = fit(&placeholder, LARGE_COLUMNS as usize - 3);
        let query = tail(&self.query, LARGE_COLUMNS as usize - 3);
        layout.draw_field(scene, &query, &placeholder, false);
        layout.draw_hint(scene, self.hint());
        let Some(list) = layout.list_area() else {
            return;
        };
        let matching = self.matching();
        if matching.is_empty() {
            let empty = if self.browsing() {
                "no folders"
            } else {
                "no matches"
            };
            let (x, y) = (list.left(), list.text_y(0));
            scene.text_px(x, y, empty, LARGE.face, palette::MUTED);
        }
        let (cell_width, _) = scene.cell();
        let top = self.cursor.top;
        let shown = matching.iter().enumerate().skip(top).take(MAX_ROWS);
        for (i, (_, row)) in shown {
            let label = fit(&row.label, LABEL_ROOM);
            let item = overlay::Row {
                icon: (row.icon != ' ').then_some(row.icon),
                destructive: row.destructive(),
                ..overlay::Row::new(&label)
            };
            let style = list.draw_row(scene, i - top, &item, i == self.cursor.selected);
            if matches!(row.target, Target::Submenu(_) | Target::Directory) {
                let x = list.right() - cell_width;
                scene.text_px(x, list.text_y(i - top), "›", LARGE.face, style.icon);
            }
        }
    }
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
        assert_eq!(menu.cursor.selected, 0);
        for _ in 0..5 {
            menu.key(MenuKey::Down);
        }
        assert_eq!(menu.cursor.selected, 4);
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
    fn the_close_rows_and_their_confirm_rows_are_destructive_and_no_other_row_is() {
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 0, "decade", 0);
        let destructive: Vec<&str> = menu
            .root
            .iter()
            .filter(|row| row.destructive())
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(destructive, ["Close tab", "Close workspace"]);
        for ch in "close t".chars() {
            menu.key(MenuKey::Char(ch));
        }
        menu.key(MenuKey::Enter);
        assert!(menu.rows().iter().all(Row::destructive));
        assert_eq!(menu.hint(), [("enter", "close"), ("esc", "back")]);
    }

    #[test]
    fn hovering_a_row_selects_it_and_a_click_runs_it_like_enter() {
        let (cell, window) = ((10, 25), (1200, 800));
        let mut menu = Menu::new(Vec::new(), PathBuf::from("/"), 0, "decade", 0);
        let list = menu.layout(cell, window).list_area().unwrap();
        let settings = (list.left(), list.text_y(4));
        assert!(menu.hover(settings, cell, window));
        assert_eq!(menu.cursor.selected, 4);
        let launch = (list.left(), list.text_y(0));
        assert_eq!(menu.click(launch, cell, window), Some(Outcome::Open));
        assert_eq!(menu.path, [0]);
        assert_eq!(menu.click((0, 0), cell, window), None);
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
