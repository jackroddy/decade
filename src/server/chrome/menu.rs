use std::path::{Path, PathBuf};

use smithay::utils::{Physical, Size};

use crate::server::apps::App;
use crate::server::chrome::draw::{Face, Scene};
use crate::server::chrome::palette::{self, Rgba};
use crate::server::chrome::settings::Settings;
use crate::server::chrome::text::{fit, tail};

/// The width in cells of the palette, the New workspace browser and the settings panel.
const WIDTH: i32 = 40;
const APP_ICON: char = '\u{f08c6}';
const FOLDER_ICON: char = '\u{f024b}';
const OPEN_ICON: char = '\u{f0770}';
const CLOSE_ICON: char = '\u{f0156}';
const MAX_ROWS: usize = 10;

/// The cells a row's label may take, after its icon and before its chevron.
const LABEL_ROOM: usize = WIDTH as usize - 5;
const BOX: Rgba = palette::LIGHTER_BACKGROUND;
const BORDER: Rgba = palette::BORDER;
const TEXT: Rgba = palette::FOREGROUND;
const DIM: Rgba = palette::LIGHT_FOREGROUND;
const RULE: Rgba = palette::SELECTION;

/// The palette font's cell height, in pixels, that sizes given to
/// `scaled` are given at.
const MOCKUP_CELL: i32 = 25;

// the sizes below are in pixels at `MOCKUP_CELL`, for `scaled`
const ROW: i32 = 40;
const PADDING: i32 = 16;
const BOTTOM_PADDING: i32 = 14;
const RADIUS: i32 = 12;
const SHADOW_OFFSET: i32 = 6;
const RULE_ABOVE: i32 = 6;
const RULE_BELOW: i32 = 8;
const HINT_ABOVE: i32 = 4;
const ROW_RADIUS: i32 = 8;

/// How far a row's fill reaches past the box's padding.
const ROW_BLEED: i32 = 8;

/// How far a row's text sits inside its fill.
const ROW_INSET: i32 = 10;
const KEY_RADIUS: i32 = 4;
const KEY_PADDING: i32 = 6;

/// How far a key cap reaches above and below its text.
const KEY_RISE: i32 = 2;

/// The cells between one key's action and the next key's cap.
const HINT_GAP: i32 = 2;

/// `px`, a length given at `MOCKUP_CELL`, scaled to `cell_height`.
pub fn scaled(px: i32, cell_height: i32) -> i32 {
    (px * cell_height + MOCKUP_CELL / 2) / MOCKUP_CELL
}

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

    pub fn draw(&self, scene: &mut Scene) -> Size<i32, Physical> {
        let title: Vec<&str> = std::iter::once("decade")
            .chain(self.trail().map(|row| row.label.as_str()))
            .collect();
        let matching = self.matching();
        let shown = matching.len().clamp(1, MAX_ROWS);
        let (rows, size) = draw_box(scene, &title, Some(&self.query), shown, self.hint());
        if matching.is_empty() {
            let empty = if self.browsing() {
                "no folders"
            } else {
                "no matches"
            };
            scene.text_px(rows.left, rows.text_y(0), empty, Face::Menu, DIM);
        }
        let (cell_width, _) = scene.cell();
        let skip = self.selected.saturating_sub(MAX_ROWS - 1);
        for (i, (_, row)) in matching.iter().enumerate().skip(skip).take(MAX_ROWS) {
            let slot = i - skip;
            let style = rows.select(scene, slot, i == self.selected, row.destructive());
            let y = rows.text_y(slot);
            if row.icon != ' ' {
                scene.text_px(rows.left, y, &row.icon.to_string(), Face::Menu, style.icon);
            }
            let label = fit(&row.label, LABEL_ROOM);
            scene.text_px(
                rows.left + cell_width * 3,
                y,
                &label,
                style.face,
                style.text,
            );
            if matches!(row.target, Target::Submenu(_) | Target::Directory) {
                scene.text_px(rows.right - cell_width, y, "›", Face::Menu, style.icon);
            }
        }
        size
    }
}

/// The fill, colours and face of a list row.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RowStyle {
    pub fill: Option<Rgba>,
    pub text: Rgba,
    pub icon: Rgba,
    pub face: Face,
}

impl RowStyle {
    fn new(selected: bool, destructive: bool) -> RowStyle {
        match (selected, destructive) {
            (false, _) => RowStyle {
                fill: None,
                text: TEXT,
                icon: DIM,
                face: Face::Menu,
            },
            (true, false) => RowStyle {
                fill: Some(palette::SELECTION),
                text: TEXT,
                icon: TEXT,
                face: Face::MenuBold,
            },
            (true, true) => RowStyle {
                fill: Some(palette::DARK_RED),
                text: palette::LIGHT_RED,
                icon: palette::LIGHT_RED,
                face: Face::MenuBold,
            },
        }
    }
}

/// The list rows of a box drawn by `draw_box`.
pub struct Rows {
    /// The x where row text starts.
    pub left: i32,

    /// The x where row text must end.
    pub right: i32,
    pub height: i32,
    top: i32,
    text: i32,
    fill_x: i32,
    fill_width: i32,
    radius: i32,
}

impl Rows {
    pub fn text_y(&self, slot: usize) -> i32 {
        self.top + slot as i32 * self.height + self.text
    }

    /// The y that centres something `height` tall in row `slot`.
    pub fn centred(&self, slot: usize, height: i32) -> i32 {
        self.top + slot as i32 * self.height + (self.height - height) / 2
    }

    /// Draw the fill behind row `slot` when it is selected, and return how
    /// to draw the row.
    pub fn select(
        &self,
        scene: &mut Scene,
        slot: usize,
        selected: bool,
        destructive: bool,
    ) -> RowStyle {
        let style = RowStyle::new(selected, destructive);
        if let Some(fill) = style.fill {
            let top = self.top + slot as i32 * self.height;
            let size = (self.fill_width, self.height);
            scene.rounded(self.fill_x, top, size, self.radius, fill, fill);
        }
        style
    }
}

/// Draw the box of the palette or the settings panel: the `title`
/// breadcrumb, a prompt holding `query` when there is one, room for `rows`
/// rows under the rule and `hint` last.
pub fn draw_box(
    scene: &mut Scene,
    title: &[&str],
    query: Option<&str>,
    rows: usize,
    hint: &[(&str, &str)],
) -> (Rows, Size<i32, Physical>) {
    let (cell_width, cell_height) = scene.cell();
    let px = |px: i32| scaled(px, cell_height);
    let pad = px(PADDING);
    let row_height = px(ROW);
    let text = (row_height - cell_height) / 2;
    let width = WIDTH * cell_width + pad * 2;
    let header = 1 + query.is_some() as i32;
    let line = scene.line_width();
    let rule = pad + row_height * header + px(RULE_ABOVE);
    let fill_x = pad - px(ROW_BLEED);
    let list = Rows {
        left: fill_x + px(ROW_INSET),
        right: width - fill_x - px(ROW_INSET),
        height: row_height,
        top: rule + line + px(RULE_BELOW),
        text,
        fill_x,
        fill_width: width - fill_x * 2,
        radius: px(ROW_RADIUS),
    };
    let hint_top = list.top + row_height * rows as i32 + px(HINT_ABOVE);
    let height = hint_top + row_height + px(BOTTOM_PADDING);
    let (radius, offset) = (px(RADIUS), px(SHADOW_OFFSET));
    let shadow = palette::SHADOW;
    scene.rounded(offset, offset, (width, height), radius, shadow, shadow);
    scene.rounded(0, 0, (width, height), radius, BOX, BORDER);

    let mut x = pad;
    for (i, part) in title.iter().enumerate() {
        if i > 0 {
            scene.text_px(x, pad + text, " › ", Face::Menu, palette::MUTED);
            x += 3 * cell_width;
        }
        scene.text_px(x, pad + text, part, Face::MenuBold, TEXT);
        x += part.chars().count() as i32 * cell_width;
    }
    if let Some(query) = query {
        let y = pad + row_height + text;
        let query = tail(query, WIDTH as usize - 3);
        let caret = pad + (2 + query.chars().count() as i32) * cell_width;
        scene.text_px(pad, y, "›", Face::Menu, palette::ACCENT);
        scene.text_px(pad + 2 * cell_width, y, &query, Face::Menu, TEXT);
        scene.text_px(caret, y, "▏", Face::Menu, palette::ACCENT);
    }
    scene.rect(pad, rule, width - pad * 2, line, RULE);
    draw_hint(
        scene,
        (pad, hint_top + text),
        hint,
        (cell_width, cell_height),
    );
    (list, (width, height).into())
}

/// Where each key cap of `hint` starts and where the action after it
/// starts, from the line's left edge, and the line's width.
fn hint_layout(
    hint: &[(&str, &str)],
    (cell_width, cell_height): (i32, i32),
) -> (Vec<(i32, i32)>, i32) {
    let padding = scaled(KEY_PADDING, cell_height);
    let mut x = 0;
    let mut spots = Vec::new();
    for (i, (key, action)) in hint.iter().enumerate() {
        if i > 0 {
            x += HINT_GAP * cell_width;
        }
        let cap = x;
        let action_x = cap + key.chars().count() as i32 * cell_width + padding * 2 + cell_width;
        spots.push((cap, action_x));
        x = action_x + action.chars().count() as i32 * cell_width;
    }
    (spots, x)
}

/// The width of `hint` drawn by `draw_hint` with the palette font's cell.
pub fn hint_width(hint: &[(&str, &str)], cell: (i32, i32)) -> i32 {
    hint_layout(hint, cell).1
}

/// Draw `hint` with its text's top left at `(x, y)`, each key's name in a
/// key cap followed by what it does. `cell` is the palette font's cell.
pub fn draw_hint(scene: &mut Scene, (x, y): (i32, i32), hint: &[(&str, &str)], cell: (i32, i32)) {
    let (cell_width, cell_height) = cell;
    let padding = scaled(KEY_PADDING, cell_height);
    let rise = scaled(KEY_RISE, cell_height);
    let radius = scaled(KEY_RADIUS, cell_height);
    let (spots, _) = hint_layout(hint, cell);
    for ((key, action), (cap, action_x)) in hint.iter().zip(spots) {
        let width = key.chars().count() as i32 * cell_width + padding * 2;
        let size = (width, cell_height + rise * 2);
        let fill = palette::SELECTION;
        scene.rounded(x + cap, y - rise, size, radius, fill, fill);
        scene.text_px(x + cap + padding, y, key, Face::Menu, TEXT);
        scene.text_px(x + action_x, y, action, Face::Menu, DIM);
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
    fn a_selected_destructive_row_is_red_and_any_other_selected_row_is_not() {
        let red = RowStyle::new(true, true);
        assert_eq!(red.fill, Some(palette::DARK_RED));
        assert_eq!(
            (red.text, red.icon),
            (palette::LIGHT_RED, palette::LIGHT_RED)
        );
        let selected = RowStyle::new(true, false);
        assert_eq!(selected.fill, Some(palette::SELECTION));
        assert_eq!(selected.face, Face::MenuBold);
        assert_eq!(RowStyle::new(false, true), RowStyle::new(false, false));
        assert_eq!(RowStyle::new(false, true).fill, None);
    }

    #[test]
    fn a_hint_puts_each_action_a_cell_after_its_key_cap_and_the_pairs_two_cells_apart() {
        let hint = [("enter", "open"), ("esc", "back")];
        let (spots, width) = hint_layout(&hint, (10, MOCKUP_CELL));
        let padding = KEY_PADDING;
        let first_action = 50 + padding * 2 + 10;
        let second_cap = first_action + 40 + 20;
        let second_action = second_cap + 30 + padding * 2 + 10;
        assert_eq!(spots, [(0, first_action), (second_cap, second_action)]);
        assert_eq!(width, second_action + 40);
        assert_eq!(hint_width(&hint, (10, MOCKUP_CELL)), width);
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
