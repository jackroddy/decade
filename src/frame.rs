use std::cmp::Reverse;

use crate::channel::AgentState;
use crate::draw::Scene;
use crate::palette;
use crate::panes::CellRect;
use crate::text::Rgba;

/// The rows above the panes that the top bar takes.
pub const TOP: u32 = 1;
const OPEN_WIDTH: u32 = 24;
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

pub fn sidebar_width(open: bool) -> u32 {
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
    pub state: Option<AgentState>,
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
    Workspace(usize),
    Message(usize),
    New,
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

/// `text` cut to `max` characters, with an ellipsis where it was cut.
pub fn fit(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
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
    /// first column and label.
    fn tab_buttons(&self) -> Vec<(u32, String, Hit)> {
        let mut column = CLOSED_WIDTH + self.name().chars().count() as u32 + 1;
        let mut buttons = Vec::new();
        let tabs = (0..self.tabs).map(|tab| (format!(" {} ", tab + 1), Hit::Tab(tab)));
        for (label, hit) in tabs.chain([(" + ".to_string(), Hit::NewTab)]) {
            let width = label.chars().count() as u32;
            buttons.push((column, label, hit));
            column += width;
        }
        buttons
    }

    fn lines(&self) -> Vec<(u32, Line)> {
        let mut lines = Vec::new();
        let mut row = TOP + if self.open { 2 } else { 1 };
        for (i, entry) in self.entries.iter().enumerate() {
            lines.push((row, Line::Workspace(i)));
            row += 1;
            let needs = entry.state >= Some(AgentState::Done);
            if self.open && needs && entry.message.is_some() {
                lines.push((row, Line::Message(i)));
                row += 1;
            }
        }
        if self.open {
            lines.push((row + 1, Line::New));
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
            Line::Workspace(i) => {
                let entry = &self.entries[i];
                let needs = entry.state >= Some(AgentState::Done);
                if self.open && needs && column < 3 {
                    Hit::Clear(entry.id)
                } else {
                    Hit::Show(entry.id)
                }
            }
            Line::Message(i) => Hit::Show(self.entries[i].id),
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
        let pane = fit(&self.pane, self.room() / 2);
        scene.text(column, 0, &pane, DIM);
        column += pane.chars().count() as u32 + 2;
        if self.zoom {
            scene.text(column, 0, "zoom", palette::ACCENT);
        }

        let mut right = self.columns.saturating_sub(2);
        scene.text(right, 0, &MENU.to_string(), DIM);
        right = right.saturating_sub(3);
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
            scene.text(right, 0, &text, color);
            right = right.saturating_sub(2);
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
        let room = width.saturating_sub(4) as usize;
        for (row, line) in self.lines() {
            match line {
                Line::Workspace(i) => {
                    let entry = &self.entries[i];
                    if entry.current {
                        let highlight = CellRect {
                            column: 0,
                            row,
                            columns: width.saturating_sub(1),
                            rows: 1,
                        };
                        scene.fill(highlight, CURRENT);
                    }
                    let (glyph, color) = mark(entry.state);
                    scene.text(1, row, &glyph.to_string(), color);
                    if self.open {
                        let text = if entry.current { TEXT } else { DIM };
                        scene.text(3, row, &fit(&entry.name, room), text);
                    }
                }
                Line::Message(i) => {
                    let message = self.entries[i].message.as_deref().unwrap_or("");
                    scene.text(3, row, &fit(message, room), MUTED);
                }
                Line::New => scene.text(1, row, "+ new workspace", MUTED),
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
            state,
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
    fn a_click_on_a_name_shows_it_and_on_an_urgent_mark_clears_it() {
        let frame = frame(true);
        let first = TOP + 2;
        assert_eq!(frame.hit(8, first), Some(Hit::Show(2)));
        assert_eq!(frame.hit(1, first), Some(Hit::Clear(2)));

        // foam's message takes the row under it, so sail is two rows down
        assert_eq!(frame.hit(8, first + 1), Some(Hit::Show(2)));
        assert_eq!(frame.hit(8, first + 2), Some(Hit::Show(3)));
        assert_eq!(frame.hit(1, first + 5), Some(Hit::Show(0)));
        assert_eq!(frame.hit(40, first), None);
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
    fn the_next_workspace_needing_the_person_skips_the_current_one() {
        assert_eq!(frame(true).next_needing(), Some(2));
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(fit("website-redesign", 8), "website…");
        assert_eq!(fit("nail", 8), "nail");
    }
}
