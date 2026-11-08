use std::cmp::Reverse;
use std::path::Path;
use std::time::Duration;

use crate::server::chrome::draw::{Face, Scene};
use crate::server::chrome::palette;
use crate::server::chrome::palette::Rgba;
use crate::server::chrome::text::{fit, tail};
use crate::server::panes::CellRect;
use decade_channel::AgentState;

/// The rows above the panes that the top bar takes.
const TOP: u32 = 1;
const OPEN_WIDTH: u32 = 32;
const CLOSED_WIDTH: u32 = 3;
const DIAMOND: char = '◆';
const MENU: char = '≡';

const BAR: Rgba = palette::LIGHTER_BACKGROUND;
const SIDEBAR: Rgba = palette::DARK_BACKGROUND;
const EDGE: Rgba = palette::SELECTION;
const CURRENT: Rgba = palette::SELECTION;
const TEXT: Rgba = palette::FOREGROUND;
const DIM: Rgba = palette::LIGHT_FOREGROUND;
const MUTED: Rgba = palette::MUTED;

/// The row height, in pixels, that the card sizes below are given at.
const MOCKUP_ROW: i32 = 18;
const CARD_INSET: i32 = 6;
const CARD_GAP: i32 = 6;
const CARD_PADDING: i32 = 4;
const CARD_RADIUS: i32 = 6;
const STRIPE: i32 = 3;

fn sidebar_width(open: bool) -> u32 {
    if open { OPEN_WIDTH } else { CLOSED_WIDTH }
}

/// The cells left for panes once the top bar and sidebar have theirs.
pub fn pane_area(columns: u32, rows: u32, open: bool) -> CellRect {
    let left = sidebar_width(open).min(columns);
    CellRect {
        column: left,
        row: TOP.min(rows),
        columns: columns - left,
        rows: rows.saturating_sub(TOP),
    }
}

pub struct Entry {
    pub id: u32,
    pub name: String,
    pub tabs: usize,

    /// The workspace's directory, shortened by `short_path`.
    pub path: String,
    pub branch: Option<String>,
    pub state: Option<AgentState>,

    /// How long the agent has held `state`.
    pub age: Duration,
    pub message: Option<String>,
    pub current: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Show(u32),
    Clear(u32),
    New,
    Palette,
    Sidebar,
    Tab(usize),
    NewTab,
}

pub struct Frame {
    pub columns: u32,
    pub rows: u32,
    pub open: bool,

    /// The workspaces, most urgent agent state first, otherwise in the picker's order.
    pub entries: Vec<Entry>,
    pub tabs: usize,
    pub active_tab: usize,
    pub pane: String,
    pub zoom: bool,

    /// The width and height of a chrome cell in pixels.
    pub cell: (i32, i32),
}

/// The mark for an agent state and its colour.
fn mark(state: Option<AgentState>) -> (char, Rgba) {
    match state {
        Some(AgentState::Attention) => ('!', palette::RED),
        Some(AgentState::Done) => ('✓', palette::GREEN),
        Some(AgentState::Working) => ('●', palette::YELLOW),
        Some(AgentState::Idle) => ('○', DIM),
        None => ('·', MUTED),
    }
}

/// The colour of a card's stripe.
fn stripe(state: Option<AgentState>) -> Option<Rgba> {
    match state? {
        AgentState::Attention => Some(palette::RED),
        AgentState::Done => Some(palette::GREEN),
        AgentState::Working => Some(palette::YELLOW),
        AgentState::Idle => Some(palette::SELECTION),
    }
}

/// The colour of a card's state line.
fn tint(state: Option<AgentState>) -> Rgba {
    match state {
        Some(AgentState::Attention) => palette::LIGHT_RED,
        Some(AgentState::Done) => palette::LIGHT_GREEN,
        Some(AgentState::Working) => palette::LIGHT_YELLOW,
        Some(AgentState::Idle) => DIM,
        None => MUTED,
    }
}

/// The state's name in a card.
fn label(state: AgentState) -> &'static str {
    match state {
        AgentState::Attention => "needs you",
        AgentState::Done => "done",
        AgentState::Working => "working",
        AgentState::Idle => "idle",
    }
}

/// `age` in its largest whole unit: "now" under a minute, then "4m", "2h", "3d".
fn age(age: Duration) -> String {
    let minutes = age.as_secs() / 60;
    match minutes {
        0 => "now".to_string(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// `cwd` with `home` as `~` and every directory but the last cut to its
/// first letter, or its first two when the first is a dot.
pub fn short_path(cwd: &Path, home: &Path) -> String {
    let (head, rest) = match cwd.strip_prefix(home) {
        Ok(rest) => ("~", rest),
        Err(_) => ("", cwd.strip_prefix("/").unwrap_or(cwd)),
    };
    let names: Vec<_> = rest
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let mut parts = vec![head.to_string()];
    for (i, name) in names.iter().enumerate() {
        if i + 1 == names.len() {
            parts.push(name.clone());
        } else {
            let letters = if name.starts_with('.') { 2 } else { 1 };
            parts.push(name.chars().take(letters).collect());
        }
    }
    let path = parts.join("/");
    if path.is_empty() {
        "/".to_string()
    } else {
        path
    }
}

/// The current workspace's fill across a sidebar `width` columns wide.
fn highlight(scene: &mut Scene, row: u32, width: u32) {
    let rect = CellRect {
        column: 0,
        row,
        columns: width.saturating_sub(1),
        rows: 1,
    };
    scene.fill(rect, CURRENT);
}

impl Frame {
    pub fn new(
        (columns, rows): (u32, u32),
        open: bool,
        mut entries: Vec<Entry>,
        (tabs, active_tab): (usize, usize),
        pane: String,
        zoom: bool,
        cell: (i32, i32),
    ) -> Frame {
        entries.sort_by_key(|entry| Reverse(entry.state));
        Frame {
            columns,
            rows,
            open,
            entries,
            tabs,
            active_tab,
            pane,
            zoom,
            cell,
        }
    }

    /// The workspace that most needs the person, other than the current one.
    pub fn next_needing(&self) -> Option<u32> {
        self.entries
            .iter()
            .find(|entry| !entry.current && entry.state >= Some(AgentState::Done))
            .map(|entry| entry.id)
    }

    /// The current workspace's name, cut to fit the top bar.
    fn name(&self) -> String {
        let current = self
            .entries
            .iter()
            .find(|entry| entry.current)
            .map(|entry| entry.name.as_str())
            .unwrap_or("");
        fit(current, self.room() / 2)
    }

    fn room(&self) -> usize {
        (self.columns as usize / 2).max(8)
    }

    /// The top bar's tab buttons and the `+` after them, each with its
    /// first column and label, up to the ones that would reach the counts.
    fn tab_buttons(&self) -> Vec<(u32, String, Hit)> {
        let mut column = CLOSED_WIDTH + self.name().chars().count() as u32 + 1;
        let end = self.counts_start();
        let mut buttons = Vec::new();
        let tabs = (0..self.tabs).map(|tab| (format!(" {} ", tab + 1), Hit::Tab(tab)));
        for (label, hit) in tabs.chain([(" + ".to_string(), Hit::NewTab)]) {
            let width = label.chars().count() as u32;
            if column + width >= end {
                break;
            }
            buttons.push((column, label, hit));
            column += width;
        }
        buttons
    }

    /// The agent counts at the right of the top bar, each with its first
    /// column, text and colour.
    fn counts(&self) -> Vec<(u32, String, Rgba)> {
        let mut right = self.columns.saturating_sub(5);
        let mut counts = Vec::new();
        for state in [AgentState::Working, AgentState::Done, AgentState::Attention] {
            let count = self
                .entries
                .iter()
                .filter(|entry| entry.state == Some(state))
                .count();
            if count == 0 {
                continue;
            }
            let (glyph, color) = mark(Some(state));
            let text = format!("{glyph} {count}");
            right = right.saturating_sub(text.chars().count() as u32);
            counts.push((right, text, color));
            right = right.saturating_sub(2);
        }
        counts
    }

    /// The first column of the counts, or of the menu button when there
    /// are none.
    fn counts_start(&self) -> u32 {
        self.counts()
            .last()
            .map_or(self.columns.saturating_sub(2), |&(column, _, _)| column)
    }

    /// The workspaces with a glyph in the closed sidebar, each with its row.
    fn marks(&self) -> impl Iterator<Item = (u32, usize)> {
        (0..self.entries.len())
            .map(|i| (TOP + 1 + i as u32, i))
            .filter(|&(row, _)| row < self.rows)
    }

    /// `px`, a length given at `MOCKUP_ROW` rows, scaled to the chrome's
    /// cell height.
    fn scaled(&self, px: i32) -> i32 {
        (px * self.cell.1 + MOCKUP_ROW / 2) / MOCKUP_ROW
    }

    /// The left edge and width of every card in the open sidebar.
    fn card_span(&self) -> (i32, i32) {
        let inset = self.scaled(CARD_INSET);
        let sidebar = OPEN_WIDTH.min(self.columns) as i32 * self.cell.0;
        (inset, sidebar - 2 * inset)
    }

    /// The left edge of the text in a card.
    fn card_text(&self) -> i32 {
        self.card_span().0 + self.scaled(STRIPE) + self.cell.0
    }

    /// The open sidebar's cards that fit, each with its workspace and its
    /// top and height in pixels, and the top of the `+ new workspace` row
    /// when it fits.
    fn cards(&self) -> (Vec<(usize, i32, i32)>, Option<i32>) {
        let (gap, padding) = (self.scaled(CARD_GAP), self.scaled(CARD_PADDING));
        let row = self.cell.1;
        let bottom = self.rows as i32 * row;
        let mut top = (TOP + 3) as i32 * row;
        let mut cards = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            let lines = if entry.message.is_some() { 4 } else { 3 };
            let card = 2 * padding + lines * row;
            if top + card > bottom {
                return (cards, None);
            }
            cards.push((i, top, card));
            top += card + gap;
        }
        let new = top + gap;
        (cards, (new + row <= bottom).then_some(new))
    }

    /// What a click at pixel `x`, `y` does.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (column, row) = ((x / self.cell.0) as u32, (y / self.cell.1) as u32);
        if row < TOP {
            return if column < CLOSED_WIDTH {
                Some(Hit::Sidebar)
            } else if column + 3 >= self.columns {
                Some(Hit::Palette)
            } else {
                self.tab_buttons()
                    .into_iter()
                    .find(|(start, label, _)| {
                        (*start..start + label.chars().count() as u32).contains(&column)
                    })
                    .map(|(_, _, hit)| hit)
            };
        }
        if column >= sidebar_width(self.open) {
            return None;
        }
        if !self.open {
            let (_, i) = self.marks().find(|&(at, _)| at == row)?;
            return Some(Hit::Show(self.entries[i].id));
        }
        let (cards, new) = self.cards();
        if let Some(&(i, top, _)) = cards
            .iter()
            .find(|&&(_, top, height)| (top..top + height).contains(&y))
        {
            let entry = &self.entries[i];
            let state = top + self.scaled(CARD_PADDING) + 2 * self.cell.1;
            let on_glyph =
                (state..state + self.cell.1).contains(&y) && x < self.card_text() + 2 * self.cell.0;
            return Some(if on_glyph && entry.state >= Some(AgentState::Done) {
                Hit::Clear(entry.id)
            } else {
                Hit::Show(entry.id)
            });
        }
        let new = new?;
        (new..new + self.cell.1).contains(&y).then_some(Hit::New)
    }

    pub fn draw(&self, scene: &mut Scene) {
        if self.rows == 0 || self.columns == 0 {
            return;
        }
        self.draw_top(scene);
        self.draw_sidebar(scene);
    }

    fn draw_top(&self, scene: &mut Scene) {
        let bar = CellRect {
            column: 0,
            row: 0,
            columns: self.columns,
            rows: TOP,
        };
        scene.fill(bar, BAR);
        scene.text(1, 0, &DIAMOND.to_string(), palette::ACCENT);
        scene.text(CLOSED_WIDTH, 0, &self.name(), TEXT);
        let mut column = CLOSED_WIDTH;
        for (start, label, hit) in self.tab_buttons() {
            let width = label.chars().count() as u32;
            let color = if hit == Hit::Tab(self.active_tab) {
                let button = CellRect {
                    column: start,
                    row: 0,
                    columns: width,
                    rows: TOP,
                };
                scene.fill(button, CURRENT);
                TEXT
            } else {
                DIM
            };
            scene.text(start, 0, &label, color);
            column = start + width + 1;
        }
        let end = self.counts_start().saturating_sub(1);
        let room = (end.saturating_sub(column) as usize).min(self.room() / 2);
        let pane = fit(&self.pane, room);
        scene.text(column, 0, &pane, DIM);
        column += pane.chars().count() as u32 + 2;
        if self.zoom && column + 4 <= end {
            scene.text(column, 0, "zoom", palette::ACCENT);
        }

        scene.text(self.columns.saturating_sub(2), 0, &MENU.to_string(), DIM);
        for (column, text, color) in self.counts() {
            scene.text(column, 0, &text, color);
        }
    }

    fn draw_sidebar(&self, scene: &mut Scene) {
        let width = sidebar_width(self.open).min(self.columns);
        let side = CellRect {
            column: 0,
            row: TOP,
            columns: width,
            rows: self.rows.saturating_sub(TOP),
        };
        scene.fill(side, SIDEBAR);
        scene.right_edge(side, EDGE);
        if !self.open {
            for (row, i) in self.marks() {
                let entry = &self.entries[i];
                if entry.current {
                    highlight(scene, row, width);
                }
                let (glyph, color) = mark(entry.state);
                scene.text(1, row, &glyph.to_string(), color);
            }
            return;
        }
        let count = self.entries.len().to_string();
        scene.text(1, TOP + 1, "WORKSPACES", DIM);
        scene.text(
            width.saturating_sub(1 + count.len() as u32),
            TOP + 1,
            &count,
            DIM,
        );
        let (cards, new) = self.cards();
        for (i, top, height) in cards {
            self.draw_card(scene, &self.entries[i], top, height);
        }
        if let Some(new) = new {
            scene.text_px(self.cell.0, new, "+ new workspace", Face::Chrome, DIM);
        }
    }

    /// A workspace's card in the open sidebar.
    fn draw_card(&self, scene: &mut Scene, entry: &Entry, top: i32, height: i32) {
        let (cell_width, cell_height) = self.cell;
        let (left, width) = self.card_span();
        let radius = self.scaled(CARD_RADIUS);
        let background = if entry.current { CURRENT } else { SIDEBAR };
        if entry.current {
            scene.rounded(left, top, (width, height), radius, CURRENT, CURRENT);
        }
        if let Some(color) = stripe(entry.state) {
            // a rounded rect in the stripe's colour, covered but for its
            // left edge by one in the card's colour moved right by the
            // stripe's width, so the stripe bends round the corners
            let size = (2 * radius, height);
            scene.rounded(left, top, size, radius, color, color);
            let x = left + self.scaled(STRIPE);
            scene.rounded(x, top, size, radius, background, background);
        }

        let x = self.card_text();
        let right = left + width - cell_width;
        let room = ((right - x) / cell_width).max(0) as usize;
        let line = |n: i32| top + self.scaled(CARD_PADDING) + n * cell_height;

        let tabs = match entry.tabs {
            1 => "1 tab".to_string(),
            tabs => format!("{tabs} tabs"),
        };
        let tabs_width = tabs.chars().count();
        let name = fit(&entry.name, room.saturating_sub(tabs_width + 1));
        let name_color = if entry.current {
            TEXT
        } else {
            palette::FADED_FOREGROUND
        };
        scene.text_px(x, line(0), &name, Face::ChromeBold, name_color);
        let tabs_x = right - tabs_width as i32 * cell_width;
        scene.text_px(tabs_x, line(0), &tabs, Face::Chrome, DIM);

        let branch = entry.branch.as_deref().map(|branch| fit(branch, room / 2));
        let branch_width = branch.as_ref().map_or(0, |b| b.chars().count() + 2);
        let path = tail(&entry.path, room.saturating_sub(branch_width));
        scene.text_px(x, line(1), &path, Face::Chrome, DIM);
        if let Some(branch) = branch {
            let branch_x = x + (path.chars().count() + 2) as i32 * cell_width;
            scene.text_px(branch_x, line(1), &branch, Face::Chrome, palette::CYAN);
        }

        let (glyph, _) = mark(entry.state);
        let state = match entry.state {
            Some(state) => format!("{glyph} {} · {}", label(state), age(entry.age)),
            None => format!("{glyph} no agent"),
        };
        let state = fit(&state, room);
        scene.text_px(x, line(2), &state, Face::Chrome, tint(entry.state));

        if let Some(message) = &entry.message {
            let message = fit(message, room.saturating_sub(2));
            scene.text_px(x + 2 * cell_width, line(3), &message, Face::Chrome, DIM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32, name: &str, state: Option<AgentState>) -> Entry {
        Entry {
            id,
            name: name.to_string(),
            tabs: 1,
            path: format!("~/p/{name}"),
            branch: None,
            state,
            age: Duration::from_secs(240),
            message: None,
            current: id == 0,
        }
    }

    fn frame(open: bool) -> Frame {
        let mut entries = vec![
            entry(0, "jack", None),
            entry(1, "nail", Some(AgentState::Working)),
            entry(2, "foam", Some(AgentState::Attention)),
            entry(3, "sail", Some(AgentState::Done)),
            entry(4, "sufr", Some(AgentState::Done)),
        ];
        entries[2].message = Some("which branch?".to_string());
        Frame::new(
            (120, 40),
            open,
            entries,
            (2, 1),
            "terminal".to_string(),
            false,
            (8, 18),
        )
    }

    /// What a click on the cell at `column`, `row` does.
    fn hit_cell(frame: &Frame, column: i32, row: u32) -> Option<Hit> {
        frame.hit(column * 8, row as i32 * 18)
    }

    #[test]
    fn workspaces_are_ordered_by_urgency_and_keep_their_order_within_a_state() {
        let names: Vec<_> = frame(true).entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["foam", "sail", "sufr", "nail", "jack"]);
    }

    #[test]
    fn the_pane_area_starts_below_the_top_bar_and_right_of_the_sidebar() {
        let area = pane_area(120, 40, true);
        assert_eq!((area.column, area.row), (OPEN_WIDTH, TOP));
        assert_eq!((area.columns, area.rows), (120 - OPEN_WIDTH, 40 - TOP));
        assert_eq!(pane_area(120, 40, false).column, CLOSED_WIDTH);
    }

    #[test]
    fn cards_follow_the_heading_and_a_blank_row_with_a_gap_between_them() {
        // at 18 px rows the mockup's sizes are unchanged: a card is
        // 4 px of padding above and below its lines, with 6 px after it
        let (cards, new) = frame(true).cards();
        let tops: Vec<_> = cards
            .iter()
            .map(|&(_, top, height)| (top, height))
            .collect();
        assert_eq!(tops, [(72, 80), (158, 62), (226, 62), (294, 62), (362, 62)]);
        assert_eq!(new, Some(436));
    }

    #[test]
    fn card_sizes_scale_with_the_cell_height() {
        let mut frame = frame(true);
        frame.cell = (12, 27);
        let (cards, _) = frame.cards();
        assert_eq!(cards[0], (0, 4 * 27, 2 * 6 + 4 * 27));
        assert_eq!(cards[1].1, 4 * 27 + 2 * 6 + 4 * 27 + 9);
        assert_eq!(frame.card_span(), (9, 32 * 12 - 18));
    }

    #[test]
    fn cards_that_reach_past_the_bottom_are_left_out() {
        let mut frame = frame(true);
        frame.rows = 13;
        let (cards, new) = frame.cards();
        assert_eq!(cards.len(), 2);
        assert_eq!(new, None);
    }

    #[test]
    fn a_click_anywhere_on_a_card_shows_its_workspace_and_a_gap_does_nothing() {
        let frame = frame(true);
        assert_eq!(frame.hit(100, 72), Some(Hit::Show(2)));
        assert_eq!(frame.hit(250, 151), Some(Hit::Show(2)));
        assert_eq!(frame.hit(100, 152), None);
        assert_eq!(frame.hit(100, 158), Some(Hit::Show(3)));
        assert_eq!(frame.hit(100, 400), Some(Hit::Show(0)));
        assert_eq!(frame.hit(100, 436), Some(Hit::New));
        assert_eq!(frame.hit(100, 60), None);
        assert_eq!(frame.hit(32 * 8, 72), None);
    }

    #[test]
    fn a_click_on_the_state_glyph_clears_done_or_attention_and_nothing_else() {
        let frame = frame(true);

        // the state line is the third, and its glyph sits one cell in
        // from the 3 px stripe of a card inset 6 px
        let glyph = 6 + 3 + 8;
        let state = 72 + 4 + 2 * 18;
        assert_eq!(frame.hit(glyph, state), Some(Hit::Clear(2)));
        assert_eq!(frame.hit(glyph, state - 18), Some(Hit::Show(2)));
        assert_eq!(frame.hit(glyph + 3 * 8, state), Some(Hit::Show(2)));
        assert_eq!(frame.hit(glyph, 158 + 4 + 36), Some(Hit::Clear(3)));

        // nail is working and jack has no agent
        assert_eq!(frame.hit(glyph, 294 + 4 + 36), Some(Hit::Show(1)));
        assert_eq!(frame.hit(glyph, 362 + 4 + 36), Some(Hit::Show(0)));
    }

    #[test]
    fn the_closed_sidebar_has_a_row_per_workspace() {
        let frame = frame(false);
        assert_eq!(hit_cell(&frame, 1, TOP + 1), Some(Hit::Show(2)));
        assert_eq!(hit_cell(&frame, 1, TOP + 2), Some(Hit::Show(3)));
        assert_eq!(hit_cell(&frame, 1, TOP + 5), Some(Hit::Show(0)));
        assert_eq!(hit_cell(&frame, 1, TOP + 6), None);
        assert_eq!(hit_cell(&frame, 5, TOP + 1), None);
    }

    #[test]
    fn an_age_is_given_in_its_largest_whole_unit() {
        let ages =
            [59, 60, 3599, 3600, 86399, 86400 * 3].map(|secs| age(Duration::from_secs(secs)));
        assert_eq!(ages, ["now", "1m", "59m", "1h", "23h", "3d"]);
    }

    #[test]
    fn a_short_path_puts_home_as_a_tilde_and_cuts_the_middle_directories() {
        let home = Path::new("/home/jack");
        let short = |path: &str| short_path(Path::new(path), home);
        assert_eq!(short("/home/jack/projects/decade"), "~/p/decade");
        assert_eq!(short("/home/jack"), "~");
        assert_eq!(short("/home/jack/.config/hypr/scripts"), "~/.c/h/scripts");
        assert_eq!(short("/etc/nginx"), "/e/nginx");
        assert_eq!(short("/"), "/");
    }

    #[test]
    fn the_top_bar_toggles_the_sidebar_on_the_left_and_opens_the_palette_on_the_right() {
        let frame = frame(true);
        assert_eq!(hit_cell(&frame, 1, 0), Some(Hit::Sidebar));
        assert_eq!(hit_cell(&frame, 118, 0), Some(Hit::Palette));
        assert_eq!(hit_cell(&frame, 60, 0), None);
    }

    #[test]
    fn the_top_bar_has_a_button_per_tab_after_the_name_and_a_plus_after_them() {
        let frame = frame(true);

        // "jack" takes columns 3 to 6, so " 1 " starts at 8
        assert_eq!(hit_cell(&frame, 7, 0), None);
        assert_eq!(hit_cell(&frame, 8, 0), Some(Hit::Tab(0)));
        assert_eq!(hit_cell(&frame, 10, 0), Some(Hit::Tab(0)));
        assert_eq!(hit_cell(&frame, 11, 0), Some(Hit::Tab(1)));
        assert_eq!(hit_cell(&frame, 14, 0), Some(Hit::NewTab));
        assert_eq!(hit_cell(&frame, 17, 0), None);
    }

    #[test]
    fn a_long_workspace_name_is_cut_so_the_tabs_stay_on_the_bar() {
        let mut frame = frame(true);
        frame
            .entries
            .iter_mut()
            .find(|entry| entry.current)
            .unwrap()
            .name = "x".repeat(200);
        let buttons = frame.tab_buttons();
        let (start, _, hit) = buttons.last().unwrap();
        assert_eq!(*hit, Hit::NewTab);
        assert!(*start < 60);
    }

    #[test]
    fn tab_buttons_stop_before_they_reach_the_agent_counts() {
        let mut frame = frame(true);
        frame.columns = 40;
        frame.tabs = 30;
        let start = frame.counts_start();
        let buttons = frame.tab_buttons();
        assert!(buttons.len() < 31);
        let (column, label, _) = buttons.last().unwrap();
        assert!(column + label.chars().count() as u32 <= start);
    }

    #[test]
    fn the_next_workspace_needing_the_person_skips_the_current_one() {
        assert_eq!(frame(true).next_needing(), Some(2));
    }
}
