use crate::server::chrome::draw::{Face, Scene};
use crate::server::chrome::menu::MenuKey;
use crate::server::chrome::palette::{self, Rgba};

/// The text columns of a large box.
pub const LARGE_COLUMNS: i32 = 40;

/// One of the two sizes the overlays' parts come in. Lengths are in pixels
/// at a cell `mockup` pixels tall, and `px` scales them to the font's cell.
pub struct Size {
    pub face: Face,
    pub bold: Face,
    mockup: i32,
    row: i32,
    padding: i32,
    radius: i32,
    row_radius: i32,
    shadow: i32,

    /// How far a row's text sits inside its fill.
    inset: i32,

    /// The space above and below each row's fill, inside the row.
    fill_gap: i32,
    key_radius: i32,
    key_padding: i32,

    /// How far a key cap reaches above and below its text.
    key_rise: i32,

    /// How far the edge drawn under a key cap reaches below it.
    key_drop: i32,
    header_gap: i32,
    header_below: i32,

    /// Whether a rule spanning the box parts the field and the hint from
    /// the list.
    rules: bool,
    rule_above: i32,
    rule_below: i32,
    hint_above: i32,
}

/// The palette, the settings panel and the screenshot dialog.
pub const LARGE: Size = Size {
    face: Face::Menu,
    bold: Face::MenuBold,
    mockup: 25,
    row: 40,
    padding: 16,
    radius: 12,
    row_radius: 8,
    shadow: 6,
    inset: 10,
    fill_gap: 1,
    key_radius: 4,
    key_padding: 6,
    key_rise: 2,
    key_drop: 2,
    header_gap: 5,
    header_below: 10,
    rules: true,
    rule_above: 6,
    rule_below: 8,
    hint_above: 10,
};

impl Size {
    /// `px`, a length at this size's mockup cell, scaled to `cell_height`.
    pub fn px(&self, px: i32, cell_height: i32) -> i32 {
        (px * cell_height + self.mockup / 2) / self.mockup
    }

    /// The width of a box whose rows hold `columns` cells of text.
    pub fn width(&self, (cell_width, cell_height): (i32, i32), columns: i32) -> i32 {
        let side = self.px(self.padding, cell_height) + self.px(self.inset, cell_height);
        columns * cell_width + 2 * side
    }

    pub fn padding(&self, cell_height: i32) -> i32 {
        self.px(self.padding, cell_height)
    }

    /// The left edge of a part's text, from the box's left edge.
    pub fn text_left(&self, cell_height: i32) -> i32 {
        self.padding(cell_height) + self.px(self.inset, cell_height)
    }
}

/// Draw a box at `at` of `size` with its shadow.
pub fn draw_box(scene: &mut Scene, size: &Size, at: (i32, i32), (width, height): (i32, i32)) {
    let cell_height = scene.cell().1;
    let radius = size.px(size.radius, cell_height);
    let offset = size.px(size.shadow, cell_height);
    let shadow = palette::SHADOW;
    scene.rounded(at.0, at.1 + offset, (width, height), radius, shadow, shadow);
    let fill = palette::LIGHTER_BACKGROUND;
    scene.rounded(at.0, at.1, (width, height), radius, fill, palette::BORDER);
}

/// Where each part of a box goes, built top to bottom by adding parts in
/// the order they are drawn.
pub struct Layout {
    pub size: &'static Size,
    cell: (i32, i32),

    /// The box's top left, in the scene's pixels.
    pub at: (i32, i32),
    pub width: i32,
    pub height: i32,

    // the parts' tops, from the box's top
    header: Option<i32>,
    field: Option<i32>,
    field_rule: Option<i32>,
    list: Option<(i32, usize)>,
    hint_rule: Option<i32>,
    hint: Option<i32>,
}

impl Layout {
    pub fn new(size: &'static Size, cell: (i32, i32), width: i32) -> Layout {
        Layout {
            size,
            cell,
            at: (0, 0),
            width,
            height: size.padding(cell.1),
            header: None,
            field: None,
            field_rule: None,
            list: None,
            hint_rule: None,
            hint: None,
        }
    }

    fn px(&self, px: i32) -> i32 {
        self.size.px(px, self.cell.1)
    }

    fn line(&self) -> i32 {
        (self.cell.0 / 8).max(1)
    }

    /// Add a title, with a dim second line under it when `second` is true.
    pub fn header(mut self, second: bool) -> Layout {
        self.header = Some(self.height);
        self.height += self.cell.1 + self.px(self.size.header_below);
        if second {
            self.height += self.px(self.size.header_gap) + self.cell.1;
        }
        self
    }

    pub fn field(mut self) -> Layout {
        self.field = Some(self.height);
        self.height += self.px(self.size.row);
        if self.size.rules {
            let rule = self.height + self.px(self.size.rule_above);
            self.field_rule = Some(rule);
            self.height = rule + self.line() + self.px(self.size.rule_below);
        }
        self
    }

    /// Add room for `rows` list rows.
    pub fn list(mut self, rows: usize) -> Layout {
        self.list = Some((self.height, rows));
        self.height += rows as i32 * self.px(self.size.row);
        self
    }

    pub fn hint(mut self) -> Layout {
        if self.size.rules {
            self.height += self.px(self.size.rule_below);
            self.hint_rule = Some(self.height);
            self.height += self.line();
        }
        self.height += self.px(self.size.hint_above);
        self.hint = Some(self.height);
        let rise = self.px(self.size.key_rise);
        self.height += self.cell.1 + 2 * rise + self.px(self.size.key_drop);
        self
    }

    /// Close the box with its bottom padding.
    pub fn end(mut self) -> Layout {
        self.height += self.size.padding(self.cell.1);
        self
    }

    pub fn at(mut self, at: (i32, i32)) -> Layout {
        self.at = at;
        self
    }

    /// Put the box in the middle of `window`, no further left or up than
    /// its top left corner.
    pub fn centred(self, (width, height): (i32, i32)) -> Layout {
        let at = (
            (width - self.width).max(0) / 2,
            (height - self.height).max(0) / 2,
        );
        self.at(at)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        let (left, top) = self.at;
        (left..left + self.width).contains(&x) && (top..top + self.height).contains(&y)
    }

    /// The x where the parts' text starts.
    pub fn text_left(&self) -> i32 {
        self.at.0 + self.size.text_left(self.cell.1)
    }

    pub fn list_area(&self) -> Option<List> {
        let (top, rows) = self.list?;
        let padding = self.size.padding(self.cell.1);
        Some(List {
            size: self.size,
            cell: self.cell,
            left: self.at.0 + padding,
            top: self.at.1 + top,
            width: self.width - 2 * padding,
            rows,
        })
    }

    pub fn draw_box(&self, scene: &mut Scene) {
        draw_box(scene, self.size, self.at, (self.width, self.height));
    }

    /// Draw the header's `title`, and `second` dim under it.
    pub fn draw_header(&self, scene: &mut Scene, title: &str, second: Option<&str>) {
        let Some(top) = self.header else {
            return;
        };
        let (x, y) = (self.text_left(), self.at.1 + top);
        scene.text_px(x, y, title, self.size.bold, palette::FOREGROUND);
        if let Some(second) = second {
            let y = y + self.cell.1 + self.px(self.size.header_gap);
            scene.text_px(x, y, second, self.size.face, palette::LIGHT_FOREGROUND);
        }
    }

    /// Draw the text field holding `text`, or `placeholder` when it is
    /// empty, with the text on a selection fill when `marked` is true.
    pub fn draw_field(&self, scene: &mut Scene, text: &str, placeholder: &str, marked: bool) {
        let Some(top) = self.field else {
            return;
        };
        let (cell_width, cell_height) = self.cell;
        let face = self.size.face;
        let x = self.text_left();
        let y = self.at.1 + top + (self.px(self.size.row) - cell_height) / 2;
        let text_x = x + 2 * cell_width;
        let shown = if text.is_empty() { placeholder } else { text };
        let width = shown.chars().count() as i32 * cell_width;
        scene.text_px(x, y, "›", face, palette::ACCENT);
        if text.is_empty() {
            scene.text_px(text_x, y, placeholder, face, palette::MUTED);
        } else if marked {
            scene.rect(text_x, y, width, cell_height, palette::SELECTION);
        }
        scene.text_px(text_x, y, text, face, palette::FOREGROUND);
        scene.text_px(text_x + width, y, "▏", face, palette::ACCENT);
        if let Some(rule) = self.field_rule {
            self.draw_rule(scene, rule);
        }
    }

    fn draw_rule(&self, scene: &mut Scene, top: i32) {
        let line = self.line();
        let width = self.width - 2 * line;
        scene.rect(
            self.at.0 + line,
            self.at.1 + top,
            width,
            line,
            palette::SELECTION,
        );
    }

    pub fn draw_hint(&self, scene: &mut Scene, hint: &[(&str, &str)]) {
        let Some(top) = self.hint else {
            return;
        };
        if let Some(rule) = self.hint_rule {
            self.draw_rule(scene, rule);
        }
        let y = self.at.1 + top + self.px(self.size.key_rise);
        draw_hint(scene, self.size, (self.text_left(), y), hint, self.cell);
    }
}

/// A row of a list, as the list draws it and its keys and clicks reach it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Row<'a> {
    pub icon: Option<char>,
    pub label: &'a str,

    /// The key that runs the row, shown in a key cap at its right.
    pub key: Option<char>,
    pub destructive: bool,
    pub available: bool,
}

impl Row<'_> {
    pub fn new(label: &str) -> Row<'_> {
        Row {
            icon: None,
            label,
            key: None,
            destructive: false,
            available: true,
        }
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
    pub fn new(size: &Size, row: &Row, selected: bool) -> RowStyle {
        let red = row.destructive.then_some(palette::DARK_RED);
        if !row.available {
            return RowStyle {
                fill: red,
                text: palette::MUTED,
                icon: palette::MUTED,
                face: size.face,
            };
        }
        let face = if selected { size.bold } else { size.face };
        match (selected, row.destructive) {
            (_, true) => RowStyle {
                fill: Some(if selected {
                    palette::MID_RED
                } else {
                    palette::DARK_RED
                }),
                text: palette::LIGHT_RED,
                icon: palette::LIGHT_RED,
                face,
            },
            (true, false) => RowStyle {
                fill: Some(palette::SELECTION),
                text: palette::FOREGROUND,
                icon: palette::ACCENT,
                face,
            },
            (false, false) => RowStyle {
                fill: None,
                text: palette::FOREGROUND,
                icon: palette::LIGHT_FOREGROUND,
                face,
            },
        }
    }
}

/// The rows of a box's list, in the scene's pixels.
pub struct List {
    size: &'static Size,
    cell: (i32, i32),

    /// The left edge and width of a row's fill.
    left: i32,
    width: i32,
    top: i32,

    /// The number of rows there is room for.
    pub rows: usize,
}

impl List {
    pub fn height(&self) -> i32 {
        self.size.px(self.size.row, self.cell.1)
    }

    fn row_top(&self, slot: usize) -> i32 {
        self.top + slot as i32 * self.height()
    }

    pub fn text_y(&self, slot: usize) -> i32 {
        self.row_top(slot) + (self.height() - self.cell.1) / 2
    }

    /// The y that centres something `height` tall in row `slot`.
    pub fn centred(&self, slot: usize, height: i32) -> i32 {
        self.row_top(slot) + (self.height() - height) / 2
    }

    /// The x where row text starts.
    pub fn left(&self) -> i32 {
        self.left + self.size.px(self.size.inset, self.cell.1)
    }

    /// The x where row text must end.
    pub fn right(&self) -> i32 {
        self.left + self.width - self.size.px(self.size.inset, self.cell.1)
    }

    /// The row slot under pixel `x`, `y`.
    pub fn slot_at(&self, x: i32, y: i32) -> Option<usize> {
        if !(self.left..self.left + self.width).contains(&x) || y < self.top {
            return None;
        }
        let slot = ((y - self.top) / self.height()) as usize;
        (slot < self.rows).then_some(slot)
    }

    /// Draw `row` in slot `slot`: its fill, icon, label and key cap.
    /// Return its style, for what the caller draws after the label.
    pub fn draw_row(&self, scene: &mut Scene, slot: usize, row: &Row, selected: bool) -> RowStyle {
        let style = RowStyle::new(self.size, row, selected);
        let gap = self.size.px(self.size.fill_gap, self.cell.1);
        if let Some(fill) = style.fill {
            let top = self.row_top(slot) + gap;
            let size = (self.width, self.height() - 2 * gap);
            let radius = self.size.px(self.size.row_radius, self.cell.1);
            scene.rounded(self.left, top, size, radius, fill, fill);
        }
        let (cell_width, cell_height) = self.cell;
        let y = self.text_y(slot);
        let mut x = self.left();
        if let Some(icon) = row.icon {
            scene.text_px(x, y, &icon.to_string(), self.size.face, style.icon);
            x += 3 * cell_width;
        }
        scene.text_px(x, y, row.label, style.face, style.text);
        if let Some(key) = row.key {
            let key = key.to_string();
            let width = key_width(self.size, &key, self.cell);
            let at = (self.right() - width, y);
            draw_key(
                scene,
                self.size,
                at,
                &key,
                (cell_width, cell_height),
                row.available,
            );
        }
        style
    }
}

/// Which row of a list is selected, and which row is shown first.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cursor {
    pub selected: usize,
    pub top: usize,
}

/// What a key does to a list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The key moved the selection or did nothing.
    Stay,

    /// Run the row at this index.
    Run(usize),
    Close,

    /// A key the list has no use for, for its overlay to handle.
    Pass(MenuKey),
}

impl Cursor {
    /// Up and Down move the selection and stop at the ends, Enter runs the
    /// selected row, Escape closes, and a row's key runs that row.
    pub fn key(&mut self, key: MenuKey, rows: &[Row]) -> Step {
        match key {
            MenuKey::Up => self.selected = self.selected.saturating_sub(1),
            MenuKey::Down => {
                self.selected = (self.selected + 1).min(rows.len().saturating_sub(1));
            }
            MenuKey::Enter => {
                return match rows.get(self.selected) {
                    Some(row) if row.available => Step::Run(self.selected),
                    _ => Step::Stay,
                };
            }
            MenuKey::Escape => return Step::Close,
            MenuKey::Char(ch) => {
                let ch = ch.to_ascii_lowercase();
                let found = (rows.iter()).position(|row| row.available && row.key == Some(ch));
                let Some(index) = found else {
                    return Step::Pass(key);
                };
                self.selected = index;
                return Step::Run(index);
            }
            MenuKey::Left | MenuKey::Right | MenuKey::Backspace | MenuKey::Tab => {
                return Step::Pass(key);
            }
        }
        Step::Stay
    }

    /// Move `top` as little as it takes to show the selected row in a
    /// list with room for `shown` rows.
    pub fn scroll(&mut self, shown: usize) {
        if self.selected < self.top {
            self.top = self.selected;
        } else if shown > 0 && self.selected >= self.top + shown {
            self.top = self.selected + 1 - shown;
        }
    }

    /// The index of the available row under pixel `x`, `y`.
    fn row_at(&self, list: &List, rows: &[Row], x: i32, y: i32) -> Option<usize> {
        let index = self.top + list.slot_at(x, y)?;
        rows.get(index).filter(|row| row.available).map(|_| index)
    }

    /// Select the row under pixel `x`, `y`, and return whether the
    /// selection moved.
    pub fn hover(&mut self, list: &List, rows: &[Row], x: i32, y: i32) -> bool {
        match self.row_at(list, rows, x, y) {
            Some(index) if index != self.selected => {
                self.selected = index;
                true
            }
            _ => false,
        }
    }

    /// Select and return the row a left click at pixel `x`, `y` runs.
    pub fn click(&mut self, list: &List, rows: &[Row], x: i32, y: i32) -> Option<usize> {
        let index = self.row_at(list, rows, x, y)?;
        self.selected = index;
        Some(index)
    }
}

fn key_width(size: &Size, key: &str, (cell_width, cell_height): (i32, i32)) -> i32 {
    key.chars().count() as i32 * cell_width + 2 * size.px(size.key_padding, cell_height)
}

/// Draw `key` in a key cap with its text's top left at `(x, y)`, dimmed
/// when `available` is false.
fn draw_key(
    scene: &mut Scene,
    size: &Size,
    (x, y): (i32, i32),
    key: &str,
    cell: (i32, i32),
    available: bool,
) {
    let cell_height = cell.1;
    let rise = size.px(size.key_rise, cell_height);
    let drop = size.px(size.key_drop, cell_height);
    let radius = size.px(size.key_radius, cell_height);
    let padding = size.px(size.key_padding, cell_height);
    let cap = (key_width(size, key, cell), cell_height + 2 * rise);
    let (edge, text) = if available {
        (palette::BORDER, palette::LIGHT_FOREGROUND)
    } else {
        let edge = palette::mix(palette::BORDER, palette::LIGHTER_BACKGROUND, 0.5);
        (edge, palette::MUTED)
    };
    scene.rounded(x, y - rise + drop, cap, radius, edge, edge);
    let fill = palette::DARK_BACKGROUND;
    scene.rounded(x, y - rise, cap, radius, fill, edge);
    scene.text_px(x + padding, y, key, size.face, text);
}

/// Where each key cap of `hint` starts and where the action after it
/// starts, from the line's left edge, and the line's width.
fn hint_layout(
    size: &Size,
    hint: &[(&str, &str)],
    (cell_width, cell_height): (i32, i32),
) -> (Vec<(i32, i32)>, i32) {
    let mut x = 0;
    let mut spots = Vec::new();
    for (i, (key, action)) in hint.iter().enumerate() {
        if i > 0 {
            x += 2 * cell_width;
        }
        let cap = x;
        let action_x = cap + key_width(size, key, (cell_width, cell_height)) + cell_width;
        spots.push((cap, action_x));
        x = action_x + action.chars().count() as i32 * cell_width;
    }
    (spots, x)
}

/// The width of `hint` drawn by `draw_hint`.
pub fn hint_width(size: &Size, hint: &[(&str, &str)], cell: (i32, i32)) -> i32 {
    hint_layout(size, hint, cell).1
}

/// Draw `hint` with its text's top left at `(x, y)`, each key's name in a
/// key cap followed by what it does.
pub fn draw_hint(
    scene: &mut Scene,
    size: &Size,
    (x, y): (i32, i32),
    hint: &[(&str, &str)],
    cell: (i32, i32),
) {
    let (spots, _) = hint_layout(size, hint, cell);
    for ((key, action), (cap, action_x)) in hint.iter().zip(spots) {
        draw_key(scene, size, (x + cap, y), key, cell, true);
        scene.text_px(x + action_x, y, action, size.face, palette::MUTED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (i32, i32) = (10, 25);

    fn rows() -> [Row<'static>; 4] {
        [
            Row {
                key: Some('y'),
                ..Row::new("Allow")
            },
            Row {
                available: false,
                key: Some('s'),
                ..Row::new("Skip")
            },
            Row::new("Later"),
            Row {
                key: Some('n'),
                destructive: true,
                ..Row::new("Deny")
            },
        ]
    }

    fn list() -> List {
        Layout::new(&LARGE, CELL, 200)
            .list(4)
            .end()
            .at((100, 50))
            .list_area()
            .unwrap()
    }

    #[test]
    fn up_and_down_move_the_selection_and_stop_at_the_ends() {
        let rows = rows();
        let mut cursor = Cursor::default();
        assert_eq!(cursor.key(MenuKey::Up, &rows), Step::Stay);
        assert_eq!(cursor.selected, 0);
        for _ in 0..6 {
            cursor.key(MenuKey::Down, &rows);
        }
        assert_eq!(cursor.selected, 3);
        assert_eq!(cursor.key(MenuKey::Down, &[]), Step::Stay);
    }

    #[test]
    fn enter_runs_the_selected_row_unless_it_is_unavailable_and_escape_closes() {
        let rows = rows();
        let mut cursor = Cursor::default();
        assert_eq!(cursor.key(MenuKey::Enter, &rows), Step::Run(0));
        cursor.key(MenuKey::Down, &rows);
        assert_eq!(cursor.key(MenuKey::Enter, &rows), Step::Stay);
        assert_eq!(cursor.key(MenuKey::Escape, &rows), Step::Close);
        assert_eq!(cursor.key(MenuKey::Enter, &[]), Step::Stay);
    }

    #[test]
    fn a_rows_key_runs_it_in_either_case_and_other_keys_pass_to_the_overlay() {
        let rows = rows();
        let mut cursor = Cursor::default();
        assert_eq!(cursor.key(MenuKey::Char('N'), &rows), Step::Run(3));
        assert_eq!(cursor.selected, 3);
        let skip = MenuKey::Char('s');
        assert_eq!(cursor.key(skip, &rows), Step::Pass(skip));
        let x = MenuKey::Char('x');
        assert_eq!(cursor.key(x, &rows), Step::Pass(x));
        assert_eq!(cursor.key(MenuKey::Left, &rows), Step::Pass(MenuKey::Left));
    }

    #[test]
    fn hovering_selects_the_available_row_under_the_pointer_and_a_click_runs_it() {
        let (list, rows) = (list(), rows());
        let mut cursor = Cursor::default();
        let x = 100 + 20;
        let y = |slot: i32| list.top + slot * list.height() + list.height() / 2;
        assert!(cursor.hover(&list, &rows, x, y(2)));
        assert_eq!(cursor.selected, 2);
        assert!(!cursor.hover(&list, &rows, x, y(2)));
        assert!(!cursor.hover(&list, &rows, x, y(1)));
        assert_eq!(cursor.selected, 2);
        assert_eq!(cursor.click(&list, &rows, x, y(3)), Some(3));
        assert_eq!(cursor.selected, 3);
        assert_eq!(cursor.click(&list, &rows, x, y(1)), None);
        assert_eq!(cursor.click(&list, &rows, x, y(4)), None);
        assert_eq!(cursor.click(&list, &rows, 99, y(0)), None);
    }

    #[test]
    fn a_click_counts_rows_from_the_first_one_shown() {
        let (list, rows) = (list(), rows());
        let mut cursor = Cursor {
            selected: 3,
            top: 2,
        };
        let y = list.top + list.height() / 2;
        assert_eq!(cursor.click(&list, &rows, 120, y), Some(2));
    }

    #[test]
    fn scrolling_keeps_the_selected_row_in_view() {
        let mut cursor = Cursor {
            selected: 12,
            top: 0,
        };
        cursor.scroll(10);
        assert_eq!(cursor.top, 3);
        cursor.selected = 1;
        cursor.scroll(10);
        assert_eq!(cursor.top, 1);
    }

    #[test]
    fn a_destructive_row_is_red_selected_or_not_and_an_unavailable_row_is_grey() {
        let deny = rows()[3];
        let red = RowStyle::new(&LARGE, &deny, false);
        assert_eq!(red.fill, Some(palette::DARK_RED));
        assert_eq!((red.text, red.face), (palette::LIGHT_RED, Face::Menu));
        let selected = RowStyle::new(&LARGE, &deny, true);
        assert_eq!(selected.fill, Some(palette::MID_RED));
        assert_eq!(selected.face, Face::MenuBold);
        let grey = RowStyle::new(&LARGE, &rows()[1], true);
        assert_eq!((grey.fill, grey.text), (None, palette::MUTED));
        assert_eq!(grey.face, Face::Menu);
        let plain = RowStyle::new(&LARGE, &rows()[2], true);
        assert_eq!(plain.fill, Some(palette::SELECTION));
        assert_eq!(plain.face, Face::MenuBold);
        assert_eq!(RowStyle::new(&LARGE, &rows()[2], false).fill, None);
    }

    #[test]
    fn the_parts_stack_in_the_order_they_are_added() {
        let cell = (10, 25);
        let layout = Layout::new(&LARGE, cell, 400)
            .header(true)
            .list(3)
            .hint()
            .end();
        let header = 16 + 25 + 10 + 5 + 25;
        let list = layout.list_area().unwrap();
        assert_eq!(list.top, header);
        assert_eq!((list.left, list.width), (16, 400 - 32));
        let hint = header + 3 * 40 + 8 + 1 + 10;
        assert_eq!(layout.hint, Some(hint));
        assert_eq!(layout.height, hint + 25 + 2 * 2 + 2 + 16);
    }

    #[test]
    fn a_hint_puts_each_action_a_cell_after_its_key_cap_and_the_pairs_two_cells_apart() {
        let hint = [("enter", "open"), ("esc", "back")];
        let cell = (10, 25);
        let (spots, width) = hint_layout(&LARGE, &hint, cell);
        let first_action = 50 + 6 * 2 + 10;
        let second_cap = first_action + 40 + 20;
        let second_action = second_cap + 30 + 6 * 2 + 10;
        assert_eq!(spots, [(0, first_action), (second_cap, second_action)]);
        assert_eq!(width, second_action + 40);
        assert_eq!(hint_width(&LARGE, &hint, cell), width);
    }
}
