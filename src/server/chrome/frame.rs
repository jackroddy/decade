use std::cmp::Reverse;
use std::path::Path;
use std::time::Duration;

use crate::server::chrome::draw::Scene;
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

enum Line {
    /// A workspace's glyph in the closed sidebar.
    Mark(usize),
    Card(usize, Part),
    New,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Name,
    Path,
    State,
    Message,
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
        columns: u32,
        rows: u32,
        open: bool,
        mut entries: Vec<Entry>,
        (tabs, active_tab): (usize, usize),
        pane: String,
        zoom: bool,
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

    fn lines(&self) -> Vec<(u32, Line)> {
        let mut lines = Vec::new();
        if !self.open {
            for i in 0..self.entries.len() {
                lines.push((TOP + 1 + i as u32, Line::Mark(i)));
            }
        } else {
            let mut row = TOP + 2;
            for (i, entry) in self.entries.iter().enumerate() {
                let message = entry.message.is_some().then_some(Part::Message);
                for part in [Part::Name, Part::Path, Part::State]
                    .into_iter()
                    .chain(message)
                {
                    lines.push((row, Line::Card(i, part)));
                    row += 1;
                }
                row += 1;
            }
            lines.push((row, Line::New));
        }
        lines.retain(|&(row, _)| row < self.rows);
        lines
    }

    pub fn hit(&self, column: u32, row: u32) -> Option<Hit> {
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
        let (_, line) = self.lines().into_iter().find(|&(at, _)| at == row)?;
        Some(match line {
            Line::Mark(i) => Hit::Show(self.entries[i].id),
            Line::Card(i, part) => {
                let entry = &self.entries[i];
                let needs = entry.state >= Some(AgentState::Done);
                if part == Part::State && needs && (1..4).contains(&column) {
                    Hit::Clear(entry.id)
                } else {
                    Hit::Show(entry.id)
                }
            }
            Line::New => Hit::New,
        })
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
        if self.open {
            scene.text(1, TOP, "workspaces", MUTED);
        }
        for (row, line) in self.lines() {
            match line {
                Line::Mark(i) => {
                    let entry = &self.entries[i];
                    if entry.current {
                        highlight(scene, row, width);
                    }
                    let (glyph, color) = mark(entry.state);
                    scene.text(1, row, &glyph.to_string(), color);
                }
                Line::Card(i, part) => self.draw_card_line(scene, row, &self.entries[i], part),
                Line::New => scene.text(1, row, "+ new workspace", MUTED),
            }
        }
    }

    /// One row of a workspace's card in the open sidebar.
    fn draw_card_line(&self, scene: &mut Scene, row: u32, entry: &Entry, part: Part) {
        let width = OPEN_WIDTH.min(self.columns);
        if entry.current {
            highlight(scene, row, width);
        }
        let (glyph, color) = mark(entry.state);
        if entry.state.is_some() {
            let (x, y) = scene.px(0, row);
            let (_, height) = scene.cell();
            scene.rect(x, y, scene.line_width(), height, color);
        }
        let room = width.saturating_sub(3) as usize;
        match part {
            Part::Name => {
                let tabs = match entry.tabs {
                    1 => "1 tab".to_string(),
                    tabs => format!("{tabs} tabs"),
                };
                let tabs_width = tabs.chars().count();
                let name_room = room.saturating_sub(tabs_width + 1);
                scene.text(2, row, &fit(&entry.name, name_room), TEXT);
                let start = width.saturating_sub(1 + tabs_width as u32);
                scene.text(start, row, &tabs, MUTED);
            }
            Part::Path => {
                let branch = entry.branch.as_deref().map(|branch| fit(branch, room / 2));
                let branch_width = branch.as_ref().map_or(0, |b| b.chars().count() + 2);
                let path = tail(&entry.path, room.saturating_sub(branch_width));
                scene.text(2, row, &path, DIM);
                if let Some(branch) = branch {
                    let column = 2 + path.chars().count() as u32 + 2;
                    scene.text(column, row, &branch, palette::CYAN);
                }
            }
            Part::State => {
                scene.text(2, row, &glyph.to_string(), color);
                let Some(state) = entry.state else {
                    scene.text(4, row, &fit("no agent", room.saturating_sub(2)), MUTED);
                    return;
                };
                let text = format!("{} · {}", label(state), age(entry.age));
                scene.text(4, row, &fit(&text, room.saturating_sub(2)), color);
            }
            Part::Message => {
                let message = entry.message.as_deref().unwrap_or("");
                scene.text(4, row, &fit(message, room.saturating_sub(2)), DIM);
            }
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
            120,
            40,
            open,
            entries,
            (2, 1),
            "terminal".to_string(),
            false,
        )
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
    fn a_card_takes_a_row_per_line_and_a_blank_row_after_it() {
        let frame = frame(true);
        let first = TOP + 2;

        // foam has a message, so its card has four rows and sail's
        // starts after the blank row under them
        for row in first..first + 4 {
            assert_eq!(frame.hit(8, row), Some(Hit::Show(2)));
        }
        assert_eq!(frame.hit(8, first + 4), None);
        assert_eq!(frame.hit(8, first + 5), Some(Hit::Show(3)));
        assert_eq!(frame.hit(8, first + 7), Some(Hit::Show(3)));
        assert_eq!(frame.hit(8, first + 8), None);
        assert_eq!(frame.hit(8, first + 9), Some(Hit::Show(4)));

        // four cards of three rows after foam's, each with its blank row
        assert_eq!(frame.hit(1, first + 5 + 4 * 4), Some(Hit::New));
        assert_eq!(frame.hit(40, first), None);
    }

    #[test]
    fn a_click_on_the_state_glyph_clears_done_or_attention_and_nothing_else() {
        let frame = frame(true);
        let state = TOP + 2 + 2;
        assert_eq!(frame.hit(2, state), Some(Hit::Clear(2)));
        assert_eq!(frame.hit(2, state - 2), Some(Hit::Show(2)));
        assert_eq!(frame.hit(8, state), Some(Hit::Show(2)));
        assert_eq!(frame.hit(2, state + 5), Some(Hit::Clear(3)));

        // nail is working and jack has no agent
        assert_eq!(frame.hit(2, state + 13), Some(Hit::Show(1)));
        assert_eq!(frame.hit(2, state + 17), Some(Hit::Show(0)));
    }

    #[test]
    fn the_closed_sidebar_has_a_row_per_workspace() {
        let frame = frame(false);
        assert_eq!(frame.hit(1, TOP + 1), Some(Hit::Show(2)));
        assert_eq!(frame.hit(1, TOP + 2), Some(Hit::Show(3)));
        assert_eq!(frame.hit(1, TOP + 5), Some(Hit::Show(0)));
        assert_eq!(frame.hit(1, TOP + 6), None);
        assert_eq!(frame.hit(5, TOP + 1), None);
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
        assert_eq!(frame.hit(1, 0), Some(Hit::Sidebar));
        assert_eq!(frame.hit(118, 0), Some(Hit::Palette));
        assert_eq!(frame.hit(60, 0), None);
    }

    #[test]
    fn the_top_bar_has_a_button_per_tab_after_the_name_and_a_plus_after_them() {
        let frame = frame(true);

        // "jack" takes columns 3 to 6, so " 1 " starts at 8
        assert_eq!(frame.hit(7, 0), None);
        assert_eq!(frame.hit(8, 0), Some(Hit::Tab(0)));
        assert_eq!(frame.hit(10, 0), Some(Hit::Tab(0)));
        assert_eq!(frame.hit(11, 0), Some(Hit::Tab(1)));
        assert_eq!(frame.hit(14, 0), Some(Hit::NewTab));
        assert_eq!(frame.hit(17, 0), None);
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
