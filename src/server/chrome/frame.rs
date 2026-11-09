use std::cmp::Reverse;
use std::path::Path;
use std::time::Duration;

use crate::server::chrome::draw::{Face, Scene, scaled};
use crate::server::chrome::palette;
use crate::server::chrome::palette::Rgba;
use crate::server::chrome::text::{fit, tail};
use crate::server::panes::CellRect;
use decade_channel::{AgentState, Context, Tokens};

/// The rows above the panes that the top bar takes.
const TOP: u32 = 2;
const OPEN_WIDTH: u32 = 32;
const CLOSED_WIDTH: u32 = 5;

/// The column of the workspace's name on the top bar.
const NAME_COLUMN: u32 = 3;
const DIAMOND: char = '◆';
const MENU: char = '≡';
const DOT: char = '●';

/// The mark of a dim agent.
const DIM_MARK: char = '○';

/// The columns at the right of the top bar that open the palette.
const MENU_WIDTH: u32 = 5;

/// The most columns of what runs in a tab, on its pill.
const TAB_LABEL: usize = 16;

const BAR: Rgba = palette::LIGHTER_BACKGROUND;
const SIDEBAR: Rgba = palette::DARK_BACKGROUND;
const EDGE: Rgba = palette::SELECTION;
const CURRENT: Rgba = palette::SELECTION;
const TEXT: Rgba = palette::FOREGROUND;
const DIM: Rgba = palette::LIGHT_FOREGROUND;
const MUTED: Rgba = palette::MUTED;
const SHADOW: Rgba = palette::SHADOW;
const POINTED: Rgba = palette::MAGENTA;

/// How far toward `POINTED` a pulsing pill, card or tile is filled at
/// the pulse's peak; its ring goes all the way.
const POINTED_FILL: f32 = 0.3;

// the sizes below are in pixels at 18 px rows, for `scaled`
const CARD_INSET: i32 = 6;
const CARD_GAP: i32 = 6;
const CARD_PADDING: i32 = 4;
const CARD_RADIUS: i32 = 6;
const STRIPE: i32 = 3;
const PILL_RADIUS: i32 = 4;
const TILE: i32 = 28;
const TILE_GAP: i32 = 9;
const TILE_RADIUS: i32 = 6;
const STATE_DOT: i32 = 8;
const STATE_DOT_RING: i32 = 2;
const POPOVER_GAP: i32 = 6;

/// How far the popover's top sits above its tile's.
const POPOVER_RISE: i32 = 5;
const POPOVER_PADDING: i32 = 6;
const SHADOW_OFFSET: i32 = 4;

/// The popover's width in columns.
const POPOVER_WIDTH: u32 = 34;

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

/// A tab of the current workspace, as its pill shows it.
pub struct Tab {
    /// What runs in the tab's focused pane.
    pub label: String,

    /// The most urgent state an agent in the tab has reported.
    pub state: Option<AgentState>,

    /// How far the pulse of an agent's point at a pane in the tab is
    /// toward its peak, when the tab is not the one shown.
    pub pulse: Option<f32>,
}

/// An agent of a workspace, as its card and popover show it.
pub struct Agent {
    pub name: String,
    pub state: Option<AgentState>,

    /// How long a live agent has held `state`, or how long ago a dim one
    /// was last seen.
    pub age: Duration,

    /// The question, or else the activity.
    pub message: Option<String>,
    pub model: Option<String>,
    pub context: Option<Context>,
    pub tokens: Option<Tokens>,
    pub cost: Option<f64>,

    /// Why the agent is drawn dim, or `None` for a live agent.
    pub dim: Option<Dim>,
}

/// Why an agent is drawn dim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dim {
    Disconnected,
    NoPane,
}

impl Agent {
    /// The agent's state when it is live, which is all that counts toward
    /// urgency.
    fn live_state(&self) -> Option<AgentState> {
        self.state.filter(|_| self.dim.is_none())
    }
}

pub struct Entry {
    pub id: u32,
    pub name: String,
    pub tabs: usize,

    /// The workspace's directory, shortened by `short_path`.
    pub path: String,
    pub branch: Option<String>,

    /// The workspace's agents, the one its card shows first once
    /// `Frame::new` has ordered them.
    pub agents: Vec<Agent>,
    pub current: bool,

    /// How far the pulse of the workspace's point is toward its peak, when
    /// the workspace is not the one shown.
    pub pulse: Option<f32>,
}

impl Entry {
    /// The state of the agent the card shows, when that agent is live.
    fn state(&self) -> Option<AgentState> {
        self.agents.first().and_then(Agent::live_state)
    }

    /// The lines of the card's text.
    fn card_lines(&self) -> i32 {
        let agent = self.agents.first();
        let message = agent.is_some_and(|agent| agent.message.is_some());
        let usage = agent.and_then(|agent| usage(agent, usize::MAX)).is_some();
        3 + message as i32 + usage as i32
    }

    /// The lines of the popover's text: two for the workspace, each
    /// agent's state and message, and the hint.
    fn popover_lines(&self) -> i32 {
        let agents: i32 = (self.agents.iter())
            .map(|agent| 1 + agent.message.is_some() as i32)
            .sum();
        3 + agents.max(1)
    }
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

    /// Show the next workspace whose agent is in this state.
    Next(AgentState),
}

/// A rounded button on the top bar.
struct Pill {
    column: u32,
    text: String,

    /// The colour of a state dot after the text.
    dot: Option<Rgba>,
    hit: Hit,
}

impl Pill {
    fn width(&self) -> u32 {
        let dot = if self.dot.is_some() { 2 } else { 0 };
        self.text.chars().count() as u32 + dot
    }

    fn contains(&self, column: u32) -> bool {
        (self.column..self.column + self.width()).contains(&column)
    }
}

/// A workspace's card shown beside the closed sidebar, in pixels.
struct Popover {
    entry: usize,
    at: (i32, i32),
    size: (i32, i32),
}

pub struct Frame {
    pub columns: u32,
    pub rows: u32,
    pub open: bool,

    /// The workspaces, most urgent agent state first, otherwise in the picker's order.
    pub entries: Vec<Entry>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,

    /// The focused window's title.
    pub title: String,
    pub zoom: bool,

    /// The width and height of a chrome cell in pixels.
    pub cell: (i32, i32),

    /// What is under the pointer, as `Frame::hover` gives it.
    pub hover: Option<Hit>,
}

/// The mark for an agent state and its colour.
pub(super) fn mark(state: Option<AgentState>) -> (char, Rgba) {
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

/// The background of a state's pill on the top bar.
fn pill_background(state: AgentState) -> Rgba {
    match state {
        AgentState::Attention => palette::DARK_RED,
        AgentState::Done => palette::DARK_GREEN,
        AgentState::Working => palette::DARK_YELLOW,
        AgentState::Idle => CURRENT,
    }
}

/// The background of a state's pill on the top bar under the pointer.
fn pill_hover(state: AgentState) -> Rgba {
    match state {
        AgentState::Attention => palette::MID_RED,
        AgentState::Done => palette::MID_GREEN,
        AgentState::Working => palette::MID_YELLOW,
        AgentState::Idle => palette::BORDER,
    }
}

/// "1 tab" or "N tabs".
fn tab_count(tabs: usize) -> String {
    match tabs {
        1 => "1 tab".to_string(),
        tabs => format!("{tabs} tabs"),
    }
}

/// An agent's state line: "! coax needs you · 4m", with " +N" after it
/// for `others` agents not shown, or "· no agent" for `None`.
fn state_line(agent: Option<&Agent>, others: usize) -> String {
    let Some(agent) = agent else {
        return format!("{} no agent", mark(None).0);
    };
    let name = &agent.name;
    let line = match (agent.dim, agent.state) {
        (Some(Dim::NoPane), _) => format!("{DIM_MARK} {name} no pane"),
        (Some(Dim::Disconnected), _) => {
            format!("{DIM_MARK} {name} disconnected · {}", age(agent.age))
        }
        (None, Some(state)) => {
            let (glyph, _) = mark(Some(state));
            format!("{glyph} {name} {} · {}", label(state), age(agent.age))
        }
        (None, None) => format!("{} {name}", mark(None).0),
    };
    match others {
        0 => line,
        others => format!("{line} +{others}"),
    }
}

/// The colour of an agent's state line: its state's, or `MUTED` when it
/// is dim.
fn agent_tint(agent: Option<&Agent>) -> Rgba {
    tint(agent.and_then(Agent::live_state))
}

/// What the agent has reported of its usage: "claude-opus · 62% ctx ·
/// 18.4k tok · $0.42", leaving out the parts not reported, and the model
/// too when the line is wider than `room`.
fn usage(agent: &Agent, room: usize) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(model) = &agent.model {
        parts.push(model.clone());
    }
    if let Some(Context { used, limit }) = agent.context.filter(|context| context.limit > 0) {
        let percent = (used as f64 / limit as f64 * 100.0).round();
        parts.push(format!("{percent}% ctx"));
    }
    if let Some(Tokens { input, output }) = agent.tokens {
        parts.push(format!("{} tok", count(input.saturating_add(output))));
    }
    if let Some(cost) = agent.cost {
        parts.push(format!("${cost:.2}"));
    }
    if parts.is_empty() {
        return None;
    }
    let line = parts.join(" · ");
    if line.chars().count() > room && agent.model.is_some() && parts.len() > 1 {
        parts.remove(0);
    }
    Some(fit(&parts.join(" · "), room))
}

/// `n` with a k or M suffix past a thousand: "512", "18.4k", "1.2M".
fn count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..999_950 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
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

impl Frame {
    pub fn new(
        (columns, rows): (u32, u32),
        open: bool,
        mut entries: Vec<Entry>,
        (tabs, active_tab): (Vec<Tab>, usize),
        title: String,
        zoom: bool,
        cell: (i32, i32),
    ) -> Frame {
        for entry in &mut entries {
            entry.agents.sort_by_key(|agent| {
                Reverse((agent.dim.is_none(), agent.live_state(), Reverse(agent.age)))
            });
        }
        entries.sort_by_key(|entry| Reverse(entry.state()));
        Frame {
            columns,
            rows,
            open,
            entries,
            tabs,
            active_tab,
            title,
            zoom,
            cell,
            hover: None,
        }
    }

    /// The workspace that most needs the person, other than the current one.
    pub fn next_needing(&self) -> Option<u32> {
        self.entries
            .iter()
            .find(|entry| !entry.current && entry.state() >= Some(AgentState::Done))
            .map(|entry| entry.id)
    }

    /// The workspace in `state` after the current one, wrapping round.
    pub fn next_in(&self, state: AgentState) -> Option<u32> {
        let count = self.entries.len();
        let current = self.entries.iter().position(|entry| entry.current)?;
        (1..count)
            .map(|step| &self.entries[(current + step) % count])
            .find(|entry| entry.state() == Some(state))
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

    /// The top bar's tab pills and the `+` after them, up to the ones
    /// that would reach the state pills.
    fn tab_pills(&self) -> Vec<Pill> {
        let mut column = NAME_COLUMN + self.name().chars().count() as u32 + 2;
        let end = self.states_start();
        let tabs = self.tabs.iter().enumerate().map(|(i, tab)| Pill {
            column: 0,
            text: format!(" {} {} ", i + 1, fit(&tab.label, TAB_LABEL)),
            dot: tab.state.map(|state| mark(Some(state)).1),
            hit: Hit::Tab(i),
        });
        let new = Pill {
            column: 0,
            text: " + ".to_string(),
            dot: None,
            hit: Hit::NewTab,
        };
        let mut pills = Vec::new();
        for mut pill in tabs.chain([new]) {
            if column + pill.width() >= end {
                break;
            }
            pill.column = column;
            column += pill.width() + 1;
            pills.push(pill);
        }
        pills
    }

    /// The pills at the right of the top bar that count the workspaces
    /// in each state, right to left.
    fn state_pills(&self) -> Vec<Pill> {
        let mut right = self.columns.saturating_sub(MENU_WIDTH);
        let mut pills = Vec::new();
        for state in [AgentState::Working, AgentState::Done, AgentState::Attention] {
            let count = self
                .entries
                .iter()
                .filter(|entry| entry.state() == Some(state))
                .count();
            if count == 0 {
                continue;
            }
            let (glyph, _) = mark(Some(state));
            let text = format!(" {glyph} {count} {} ", label(state));
            right = right.saturating_sub(text.chars().count() as u32);
            pills.push(Pill {
                column: right,
                text,
                dot: None,
                hit: Hit::Next(state),
            });
            right = right.saturating_sub(1);
        }
        pills
    }

    /// The first column of the state pills, or of the menu button when
    /// there are none.
    fn states_start(&self) -> u32 {
        self.state_pills()
            .last()
            .map_or(self.columns.saturating_sub(MENU_WIDTH), |pill| pill.column)
    }

    /// The closed sidebar's tiles that fit, each with its workspace, or
    /// `None` for the `+` tile, and its top in pixels.
    fn tiles(&self) -> Vec<(Option<usize>, i32)> {
        let (size, gap) = (self.scaled(TILE), self.scaled(TILE_GAP));
        let bottom = self.rows as i32 * self.cell.1;
        let mut top = TOP as i32 * self.cell.1 + gap;
        let mut tiles = Vec::new();
        for i in (0..self.entries.len()).map(Some).chain([None]) {
            if top + size > bottom {
                break;
            }
            tiles.push((i, top));
            top += size + gap;
        }
        tiles
    }

    /// The left edge of every tile, centred in the closed sidebar.
    fn tile_left(&self) -> i32 {
        let rail = CLOSED_WIDTH.min(self.columns) as i32 * self.cell.0;
        (rail - self.scaled(TILE)) / 2
    }

    /// The popover for the tile under the pointer, kept inside the window.
    fn popover(&self) -> Option<Popover> {
        let Some(Hit::Show(id)) = self.hover.filter(|_| !self.open) else {
            return None;
        };
        let (i, tile) = self
            .tiles()
            .into_iter()
            .find_map(|(i, top)| Some((i?, top)).filter(|&(i, _)| self.entries[i].id == id))?;
        let (cell_width, cell_height) = self.cell;
        let lines = self.entries[i].popover_lines();
        let size = (
            POPOVER_WIDTH as i32 * cell_width,
            2 * self.scaled(POPOVER_PADDING) + lines * cell_height,
        );
        let (right, bottom) = (
            self.columns as i32 * cell_width,
            self.rows as i32 * cell_height,
        );
        let rail = CLOSED_WIDTH.min(self.columns) as i32 * cell_width;
        let left = (rail + self.scaled(POPOVER_GAP)).min(right - size.0).max(0);
        let top = (tile - self.scaled(POPOVER_RISE))
            .min(bottom - size.1)
            .max(0);
        Some(Popover {
            entry: i,
            at: (left, top),
            size,
        })
    }

    /// `px`, a length given at 18 px rows, scaled to the chrome's cell
    /// height.
    fn scaled(&self, px: i32) -> i32 {
        scaled(px, self.cell.1)
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
            let card = 2 * padding + entry.card_lines() * row;
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
        if let Some(Popover {
            entry,
            at: (left, top),
            size: (width, height),
        }) = self.popover()
        {
            // from the rail's edge, so the pointer keeps the
            // popover while it crosses the gap to it
            let rail = CLOSED_WIDTH.min(self.columns) as i32 * self.cell.0;
            if (rail.min(left)..left + width).contains(&x) && (top..top + height).contains(&y) {
                return Some(Hit::Show(self.entries[entry].id));
            }
        }
        let (column, row) = ((x / self.cell.0) as u32, (y / self.cell.1) as u32);
        if row < TOP {
            return if column < NAME_COLUMN {
                Some(Hit::Sidebar)
            } else if column + MENU_WIDTH >= self.columns {
                Some(Hit::Palette)
            } else {
                let mut pills = self.tab_pills().into_iter().chain(self.state_pills());
                pills
                    .find(|pill| pill.contains(column))
                    .map(|pill| pill.hit)
            };
        }
        if column >= sidebar_width(self.open) {
            return None;
        }
        if !self.open {
            let size = self.scaled(TILE);
            let (i, _) =
                (self.tiles().into_iter()).find(|&(_, top)| (top..top + size).contains(&y))?;
            return Some(i.map_or(Hit::New, |i| Hit::Show(self.entries[i].id)));
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
            return Some(if on_glyph && entry.state() >= Some(AgentState::Done) {
                Hit::Clear(entry.id)
            } else {
                Hit::Show(entry.id)
            });
        }
        let new = new?;
        (new..new + self.cell.1).contains(&y).then_some(Hit::New)
    }

    /// What is under the pointer at pixel `x`, `y`: what a click there
    /// does, with a card's state glyph taken as the whole card.
    pub fn hover(&self, x: i32, y: i32) -> Option<Hit> {
        self.hit(x, y).map(|hit| match hit {
            Hit::Clear(id) => Hit::Show(id),
            hit => hit,
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
        let (cell_width, cell_height) = self.cell;
        let (width, height) = (self.columns as i32 * cell_width, TOP as i32 * cell_height);
        let line = scene.line_width();
        scene.rect(0, 0, width, height, BAR);
        scene.rect(0, height - line, width, line, EDGE);

        let y = cell_height / 2;
        let x = |column: u32| column as i32 * cell_width;
        let diamond = DIAMOND.to_string();
        scene.text_px(x(1), y, &diamond, Face::ChromeBold, palette::ACCENT);
        scene.text_px(x(NAME_COLUMN), y, &self.name(), Face::ChromeBold, TEXT);

        let tabs = self.tab_pills();
        for pill in &tabs {
            let pulse = match pill.hit {
                Hit::Tab(i) => self.tabs[i].pulse,
                _ => None,
            };
            let hovered = self.hover == Some(pill.hit);
            let color = if pill.hit == Hit::Tab(self.active_tab) {
                self.draw_pill(scene, pill.column, pill.width(), CURRENT, CURRENT);
                TEXT
            } else if let Some(pulse) = pulse {
                let base = if hovered { palette::SELECTION } else { BAR };
                let fill = palette::mix(base, POINTED, POINTED_FILL * pulse);
                let ring = palette::mix(base, POINTED, pulse);
                self.draw_pill(scene, pill.column, pill.width(), fill, ring);
                palette::mix(DIM, TEXT, pulse)
            } else {
                if hovered {
                    let hover = palette::SELECTION;
                    self.draw_pill(scene, pill.column, pill.width(), hover, hover);
                }
                DIM
            };
            scene.text_px(x(pill.column), y, &pill.text, Face::Chrome, color);
            if let Some(dot) = pill.dot {
                let column = pill.column + pill.text.chars().count() as u32;
                scene.text_px(x(column), y, &DOT.to_string(), Face::Chrome, dot);
            }
        }

        let states_start = self.states_start();
        let all_tabs = tabs.last().is_some_and(|pill| pill.hit == Hit::NewTab);
        if let Some(last) = tabs.last().filter(|_| all_tabs) {
            let separator = last.column + last.width() + 3;
            let column = separator + 3;
            let zoom = if self.zoom { "  zoom" } else { "" };
            let room = states_start.saturating_sub(column + 2) as usize;
            if room > zoom.len() {
                scene.text_px(x(separator), y, "│", Face::Chrome, MUTED);
                let title = fit(&self.title, room - zoom.len());
                scene.text_px(x(column), y, &title, Face::Chrome, DIM);
                let zoom_column = column + title.chars().count() as u32;
                scene.text_px(x(zoom_column), y, zoom, Face::Chrome, palette::ACCENT);
            }
        }

        for pill in self.state_pills() {
            let Hit::Next(state) = pill.hit else {
                continue;
            };
            let background = if self.hover == Some(pill.hit) {
                pill_hover(state)
            } else {
                pill_background(state)
            };
            self.draw_pill(scene, pill.column, pill.width(), background, background);
            scene.text_px(
                x(pill.column),
                y,
                &pill.text,
                Face::Chrome,
                tint(Some(state)),
            );
        }
        let menu = self.columns.saturating_sub(3);
        let color = if self.hover == Some(Hit::Palette) {
            TEXT
        } else {
            DIM
        };
        scene.text_px(x(menu), y, &MENU.to_string(), Face::Chrome, color);
    }

    /// The rounded background of a pill `width` columns wide from `column`.
    fn draw_pill(&self, scene: &mut Scene, column: u32, width: u32, fill: Rgba, ring: Rgba) {
        let (cell_width, cell_height) = self.cell;
        let x = column as i32 * cell_width;
        let size = (width as i32 * cell_width, cell_height);
        let radius = self.scaled(PILL_RADIUS);
        scene.rounded(x, cell_height / 2, size, radius, fill, ring);
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
            for (i, top) in self.tiles() {
                self.draw_tile(scene, i.map(|i| &self.entries[i]), top);
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
        let hovered = self.hover == Some(Hit::Show(entry.id));
        let background = match (entry.current, hovered) {
            (true, _) => CURRENT,
            (false, true) => palette::LIGHTER_BACKGROUND,
            (false, false) => SIDEBAR,
        };
        let pulse = entry.pulse.unwrap_or(0.0);
        let ring = palette::mix(background, POINTED, pulse);
        let background = palette::mix(background, POINTED, POINTED_FILL * pulse);
        if background != SIDEBAR || ring != SIDEBAR {
            scene.rounded(left, top, (width, height), radius, background, ring);
        }
        let agent = entry.agents.first();
        let color = match agent {
            Some(agent) if agent.dim.is_some() => Some(MUTED),
            _ => stripe(entry.state()),
        };
        if let Some(color) = color {
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

        let tabs = tab_count(entry.tabs);
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

        let others = entry.agents.len().saturating_sub(1);
        let state = fit(&state_line(agent, others), room);
        scene.text_px(x, line(2), &state, Face::Chrome, agent_tint(agent));

        let mut next = 3;
        if let Some(message) = agent.and_then(|agent| agent.message.as_deref()) {
            let message = fit(message, room.saturating_sub(2));
            scene.text_px(x + 2 * cell_width, line(next), &message, Face::Chrome, DIM);
            next += 1;
        }
        if let Some(usage) = agent.and_then(|agent| usage(agent, room.saturating_sub(2))) {
            scene.text_px(x + 2 * cell_width, line(next), &usage, Face::Chrome, MUTED);
        }
    }

    /// A workspace's tile in the closed sidebar, or the `+` tile for `None`.
    fn draw_tile(&self, scene: &mut Scene, entry: Option<&Entry>, top: i32) {
        let (left, size) = (self.tile_left(), self.scaled(TILE));
        let hit = entry.map_or(Hit::New, |entry| Hit::Show(entry.id));
        let current = entry.is_some_and(|entry| entry.current);
        let hovered = self.hover == Some(hit);
        let (mut fill, mut ring, color) = match (entry, current, hovered) {
            (_, true, _) => (CURRENT, palette::ACCENT, TEXT),
            (Some(_), false, true) => (palette::LIGHTER_BACKGROUND, DIM, palette::FADED_FOREGROUND),
            (None, false, true) => (palette::LIGHTER_BACKGROUND, DIM, DIM),
            (Some(_), false, false) => (
                palette::RAISED_BACKGROUND,
                palette::LIGHTER_BACKGROUND,
                palette::FADED_FOREGROUND,
            ),
            (None, false, false) => (SIDEBAR, palette::BORDER, DIM),
        };
        if let Some(pulse) = entry.and_then(|entry| entry.pulse) {
            fill = palette::mix(fill, POINTED, POINTED_FILL * pulse);
            ring = palette::mix(ring, POINTED, pulse);
        }
        let radius = self.scaled(TILE_RADIUS);
        scene.rounded(left, top, (size, size), radius, fill, ring);

        let (cell_width, cell_height) = self.cell;
        let (text, face) = match entry {
            Some(entry) => {
                let initial = entry.name.chars().next().unwrap_or(' ');
                (initial.to_uppercase().collect(), Face::ChromeBold)
            }
            None => ("+".to_string(), Face::Chrome),
        };
        let (x, y) = (
            left + (size - cell_width) / 2,
            top + (size - cell_height) / 2,
        );
        scene.text_px(x, y, &text, face, color);

        let Some(dot) = entry.and_then(Entry::state) else {
            return;
        };
        // the mockup sets the ringed dot 3 px past the inside of the
        // tile's 1 px border, so 2 px past its outside
        let ring = self.scaled(STATE_DOT_RING);
        let inner = self.scaled(STATE_DOT);
        let outer = inner + 2 * ring;
        let (x, y) = (left + size + self.scaled(2) - outer, top - self.scaled(2));
        scene.rounded(x, y, (outer, outer), outer / 2, SIDEBAR, SIDEBAR);
        let (_, color) = mark(Some(dot));
        scene.rounded(x + ring, y + ring, (inner, inner), inner / 2, color, color);
    }

    /// What the frame draws over the panes: the popover beside the closed
    /// sidebar for the tile under the pointer.
    pub fn draw_above(&self, scene: &mut Scene) {
        let Some(Popover {
            entry,
            at: (left, top),
            size: (width, height),
        }) = self.popover()
        else {
            return;
        };
        let entry = &self.entries[entry];
        let (cell_width, cell_height) = self.cell;
        let radius = self.scaled(CARD_RADIUS);
        let offset = self.scaled(SHADOW_OFFSET);
        let size = (width, height);
        scene.rounded(left + offset, top + offset, size, radius, SHADOW, SHADOW);
        scene.rounded(left, top, size, radius, BAR, palette::BORDER);

        let x = left + cell_width;
        let right = left + width - cell_width;
        let room = ((right - x) / cell_width).max(0) as usize;
        let line = |n: i32| top + self.scaled(POPOVER_PADDING) + n * cell_height;

        let branch = entry.branch.as_deref().map(|branch| fit(branch, room / 2));
        let branch_width = branch.as_ref().map_or(0, |b| b.chars().count());
        let name = fit(&entry.name, room.saturating_sub(branch_width + 1));
        scene.text_px(x, line(0), &name, Face::ChromeBold, TEXT);
        if let Some(branch) = branch {
            let branch_x = right - branch_width as i32 * cell_width;
            scene.text_px(branch_x, line(0), &branch, Face::Chrome, palette::CYAN);
        }

        let tabs = format!(" · {}", tab_count(entry.tabs));
        let path = tail(&entry.path, room.saturating_sub(tabs.chars().count()));
        scene.text_px(x, line(1), &format!("{path}{tabs}"), Face::Chrome, DIM);

        let mut next = 2;
        if entry.agents.is_empty() {
            let state = fit(&state_line(None, 0), room);
            scene.text_px(x, line(next), &state, Face::Chrome, MUTED);
            next += 1;
        }
        for agent in &entry.agents {
            let state = fit(&state_line(Some(agent), 0), room);
            scene.text_px(x, line(next), &state, Face::Chrome, agent_tint(Some(agent)));
            next += 1;
            if let Some(message) = &agent.message {
                let message = fit(message, room.saturating_sub(2));
                let color = palette::FADED_FOREGROUND;
                scene.text_px(
                    x + 2 * cell_width,
                    line(next),
                    &message,
                    Face::Chrome,
                    color,
                );
                next += 1;
            }
        }
        scene.text_px(x, line(next), "click to show", Face::Chrome, DIM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, state: Option<AgentState>) -> Agent {
        Agent {
            name: name.to_string(),
            state,
            age: Duration::from_secs(240),
            message: None,
            model: None,
            context: None,
            tokens: None,
            cost: None,
            dim: None,
        }
    }

    /// A workspace with one live agent in `state`, or none for `None`.
    fn entry(id: u32, name: &str, state: Option<AgentState>) -> Entry {
        Entry {
            id,
            name: name.to_string(),
            tabs: 1,
            path: format!("~/p/{name}"),
            branch: None,
            agents: state
                .map(|state| agent("coax", Some(state)))
                .into_iter()
                .collect(),
            current: id == 0,
            pulse: None,
        }
    }

    fn dim(mut agent: Agent, dim: Dim) -> Agent {
        agent.dim = Some(dim);
        agent
    }

    fn tab(label: &str, state: Option<AgentState>) -> Tab {
        Tab {
            label: label.to_string(),
            state,
            pulse: None,
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
        entries[2].agents[0].message = Some("which branch?".to_string());
        Frame::new(
            (120, 40),
            open,
            entries,
            (
                vec![tab("coax", Some(AgentState::Working)), tab("cargo", None)],
                1,
            ),
            "~/projects/decade: cargo test".to_string(),
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
        assert_eq!(tops, [(90, 80), (176, 62), (244, 62), (312, 62), (380, 62)]);
        assert_eq!(new, Some(454));
    }

    #[test]
    fn card_sizes_scale_with_the_cell_height() {
        let mut frame = frame(true);
        frame.cell = (12, 27);
        let (cards, _) = frame.cards();
        assert_eq!(cards[0], (0, 5 * 27, 2 * 6 + 4 * 27));
        assert_eq!(cards[1].1, 5 * 27 + 2 * 6 + 4 * 27 + 9);
        assert_eq!(frame.card_span(), (9, 32 * 12 - 18));
    }

    #[test]
    fn cards_that_reach_past_the_bottom_are_left_out() {
        let mut frame = frame(true);
        frame.rows = 14;
        let (cards, new) = frame.cards();
        assert_eq!(cards.len(), 2);
        assert_eq!(new, None);
    }

    #[test]
    fn a_click_anywhere_on_a_card_shows_its_workspace_and_a_gap_does_nothing() {
        let frame = frame(true);
        assert_eq!(frame.hit(100, 90), Some(Hit::Show(2)));
        assert_eq!(frame.hit(250, 169), Some(Hit::Show(2)));
        assert_eq!(frame.hit(100, 170), None);
        assert_eq!(frame.hit(100, 176), Some(Hit::Show(3)));
        assert_eq!(frame.hit(100, 418), Some(Hit::Show(0)));
        assert_eq!(frame.hit(100, 454), Some(Hit::New));
        assert_eq!(frame.hit(100, 78), None);
        assert_eq!(frame.hit(32 * 8, 90), None);
    }

    #[test]
    fn a_click_on_the_state_glyph_clears_done_or_attention_and_nothing_else() {
        let frame = frame(true);

        // the state line is the third, and its glyph sits one cell in
        // from the 3 px stripe of a card inset 6 px
        let glyph = 6 + 3 + 8;
        let state = 90 + 4 + 2 * 18;
        assert_eq!(frame.hit(glyph, state), Some(Hit::Clear(2)));
        assert_eq!(frame.hit(glyph, state - 18), Some(Hit::Show(2)));
        assert_eq!(frame.hit(glyph + 3 * 8, state), Some(Hit::Show(2)));
        assert_eq!(frame.hit(glyph, 176 + 4 + 36), Some(Hit::Clear(3)));

        // nail is working and jack has no agent
        assert_eq!(frame.hit(glyph, 312 + 4 + 36), Some(Hit::Show(1)));
        assert_eq!(frame.hit(glyph, 380 + 4 + 36), Some(Hit::Show(0)));
    }

    #[test]
    fn the_closed_sidebar_is_a_rail_of_tiles_most_urgent_first_with_a_plus_last() {
        // at 18 px rows the mockup's sizes are unchanged: 28 px tiles 9 px
        // apart, the first 9 px below the top bar, centred in 5 columns
        let frame = frame(false);
        assert_eq!(
            frame.tiles(),
            [
                (Some(0), 45),
                (Some(1), 82),
                (Some(2), 119),
                (Some(3), 156),
                (Some(4), 193),
                (None, 230)
            ]
        );
        assert_eq!(frame.entries[0].name, "foam");
        assert_eq!(frame.tile_left(), 6);
    }

    #[test]
    fn tile_sizes_scale_with_the_cell_height() {
        let mut frame = frame(false);
        frame.cell = (12, 27);
        let tops: Vec<_> = frame.tiles().iter().map(|&(_, top)| top).take(2).collect();
        assert_eq!(tops, [2 * 27 + 14, 2 * 27 + 14 + 42 + 14]);
        assert_eq!(frame.tile_left(), (5 * 12 - 42) / 2);
    }

    #[test]
    fn tiles_that_reach_past_the_bottom_are_left_out_with_the_plus() {
        let mut frame = frame(false);
        frame.rows = 8;
        assert_eq!(frame.tiles(), [(Some(0), 45), (Some(1), 82)]);
    }

    #[test]
    fn a_click_on_a_tile_shows_its_workspace_and_the_plus_starts_a_new_one() {
        let frame = frame(false);
        assert_eq!(frame.hit(20, 45), Some(Hit::Show(2)));
        assert_eq!(frame.hit(2, 72), Some(Hit::Show(2)));
        assert_eq!(frame.hit(20, 73), None);
        assert_eq!(frame.hit(20, 82), Some(Hit::Show(3)));
        assert_eq!(frame.hit(20, 193), Some(Hit::Show(0)));
        assert_eq!(frame.hit(20, 230), Some(Hit::New));
        assert_eq!(frame.hit(20, 258), None);
        assert_eq!(frame.hit(5 * 8, 45), None);
        assert_eq!(frame.hit(8, 0), Some(Hit::Sidebar));
    }

    #[test]
    fn the_hovered_item_is_what_a_click_does_with_a_state_glyph_taken_as_its_card() {
        let frame = frame(true);
        let (glyph, state) = (6 + 3 + 8, 90 + 4 + 2 * 18);
        assert_eq!(frame.hit(glyph, state), Some(Hit::Clear(2)));
        assert_eq!(frame.hover(glyph, state), Some(Hit::Show(2)));
        assert_eq!(frame.hover(100, 176), Some(Hit::Show(3)));
        assert_eq!(frame.hover(115 * 8, 18), Some(Hit::Palette));
        assert_eq!(frame.hover(20 * 8, 18), Some(Hit::Tab(1)));
        assert_eq!(frame.hover(60 * 8, 200), None);
    }

    #[test]
    fn hovering_a_tile_puts_its_popover_beside_the_rail_level_with_the_tile() {
        let mut frame = frame(false);
        assert!(frame.popover().is_none());
        frame.hover = Some(Hit::Show(2));
        let popover = frame.popover().unwrap();
        assert_eq!(frame.entries[popover.entry].id, 2);
        assert_eq!(popover.at, (5 * 8 + 6, 45 - 5));
        assert_eq!(popover.size, (34 * 8, 2 * 6 + 5 * 18));

        // jack has no message, so its popover is a line shorter
        frame.hover = Some(Hit::Show(0));
        let popover = frame.popover().unwrap();
        assert_eq!(popover.at, (46, 193 - 5));
        assert_eq!(popover.size.1, 2 * 6 + 4 * 18);

        frame.hover = Some(Hit::New);
        assert!(frame.popover().is_none());
        frame.hover = Some(Hit::Show(2));
        frame.open = true;
        assert!(frame.popover().is_none());
    }

    #[test]
    fn the_hover_stays_on_a_tile_while_the_pointer_is_on_its_popover_or_the_gap_before_it() {
        let mut frame = frame(false);
        let (inside, beyond) = ((200, 45), (5 * 8 + 6 + 34 * 8, 45));
        assert_eq!(frame.hover(inside.0, inside.1), None);
        frame.hover = Some(Hit::Show(2));
        assert_eq!(frame.hover(inside.0, inside.1), Some(Hit::Show(2)));
        assert_eq!(frame.hover(5 * 8 + 3, 45), Some(Hit::Show(2)));
        assert_eq!(frame.hover(beyond.0, beyond.1), None);
    }

    #[test]
    fn a_click_on_a_popover_shows_its_workspace() {
        let mut frame = frame(false);
        frame.hover = Some(Hit::Show(0));
        let Popover {
            at: (left, top),
            size: (width, height),
            ..
        } = frame.popover().unwrap();
        assert_eq!(frame.hit(left, top), Some(Hit::Show(0)));
        assert_eq!(
            frame.hit(left + width - 1, top + height - 1),
            Some(Hit::Show(0))
        );
        assert_eq!(frame.hit(left + width, top), None);
    }

    #[test]
    fn a_popover_that_would_leave_the_window_is_moved_back_inside() {
        let mut frame = frame(false);
        frame.rows = 12;
        frame.hover = Some(Hit::Show(1));
        let popover = frame.popover().unwrap();
        assert_eq!(popover.at.1 + popover.size.1, 12 * 18);
        assert_eq!(popover.at.1, 12 * 18 - (2 * 6 + 4 * 18));

        frame.columns = 30;
        let popover = frame.popover().unwrap();
        assert_eq!(popover.at.0, 0);
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
    fn the_two_row_top_bar_toggles_the_sidebar_on_the_left_and_opens_the_palette_on_the_right() {
        let frame = frame(true);
        assert_eq!(pane_area(120, 40, true).row, 2);
        assert_eq!(frame.hit(8, 0), Some(Hit::Sidebar));
        assert_eq!(frame.hit(8, 35), Some(Hit::Sidebar));
        assert_eq!(frame.hit(8, 36), None);
        assert_eq!(hit_cell(&frame, 115, 1), Some(Hit::Palette));
        assert_eq!(hit_cell(&frame, 60, 1), None);
    }

    #[test]
    fn the_top_bar_has_a_pill_per_tab_after_the_name_and_a_plus_after_them() {
        let frame = frame(true);

        // "jack" takes columns 3 to 6 and two spaces follow it; " 1 coax "
        // and its dot take 10 columns, with one between pills
        assert_eq!(hit_cell(&frame, 8, 1), None);
        assert_eq!(hit_cell(&frame, 9, 1), Some(Hit::Tab(0)));
        assert_eq!(hit_cell(&frame, 18, 1), Some(Hit::Tab(0)));
        assert_eq!(hit_cell(&frame, 19, 1), None);
        assert_eq!(hit_cell(&frame, 20, 1), Some(Hit::Tab(1)));
        assert_eq!(hit_cell(&frame, 28, 1), Some(Hit::Tab(1)));
        assert_eq!(hit_cell(&frame, 30, 1), Some(Hit::NewTab));
        assert_eq!(hit_cell(&frame, 33, 1), None);
    }

    #[test]
    fn a_tab_pill_names_what_runs_and_has_a_dot_when_an_agent_in_it_has_reported() {
        let pills = frame(true).tab_pills();
        let texts: Vec<_> = pills
            .iter()
            .map(|pill| (pill.text.as_str(), pill.dot))
            .collect();
        assert_eq!(
            texts,
            [
                (" 1 coax ", Some(palette::YELLOW)),
                (" 2 cargo ", None),
                (" + ", None)
            ]
        );
    }

    #[test]
    fn the_state_pills_read_needs_you_done_working_left_to_right_before_the_menu() {
        let frame = frame(true);
        let mut pills = frame.state_pills();
        pills.reverse();
        let texts: Vec<_> = pills
            .iter()
            .map(|pill| (pill.column, pill.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                (75, " ! 1 needs you "),
                (91, " ✓ 2 done "),
                (102, " ● 1 working ")
            ]
        );
        assert_eq!(
            hit_cell(&frame, 75, 1),
            Some(Hit::Next(AgentState::Attention))
        );
        assert_eq!(hit_cell(&frame, 90, 1), None);
        assert_eq!(hit_cell(&frame, 100, 1), Some(Hit::Next(AgentState::Done)));
        assert_eq!(
            hit_cell(&frame, 114, 1),
            Some(Hit::Next(AgentState::Working))
        );
    }

    #[test]
    fn a_state_without_workspaces_has_no_pill() {
        let mut frame = frame(true);
        frame
            .entries
            .retain(|entry| entry.state() != Some(AgentState::Done));
        let states: Vec<_> = frame.state_pills().iter().map(|pill| pill.hit).collect();
        assert_eq!(
            states,
            [
                Hit::Next(AgentState::Working),
                Hit::Next(AgentState::Attention)
            ]
        );
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
        let pills = frame.tab_pills();
        let last = pills.last().unwrap();
        assert_eq!(last.hit, Hit::NewTab);
        assert!(last.column < 60);
    }

    #[test]
    fn tab_pills_stop_before_they_reach_the_state_pills() {
        let mut frame = frame(true);
        frame.columns = 80;
        frame.tabs = (0..30).map(|_| tab("cargo", None)).collect();
        let start = frame.states_start();
        let pills = frame.tab_pills();
        assert!(pills.len() < 31);
        let last = pills.last().unwrap();
        assert!(last.column + last.width() <= start);
        assert_eq!(frame.state_pills().len(), 3);
    }

    #[test]
    fn a_state_pill_shows_the_next_workspace_in_its_state_and_cycles() {
        let mut frame = frame(true);
        assert_eq!(frame.next_in(AgentState::Done), Some(3));
        let current = |frame: &mut Frame, id: u32| {
            for entry in &mut frame.entries {
                entry.current = entry.id == id;
            }
        };
        current(&mut frame, 3);
        assert_eq!(frame.next_in(AgentState::Done), Some(4));
        current(&mut frame, 4);
        assert_eq!(frame.next_in(AgentState::Done), Some(3));
        current(&mut frame, 2);
        assert_eq!(frame.next_in(AgentState::Attention), None);
    }

    #[test]
    fn the_next_workspace_needing_the_person_skips_the_current_one() {
        assert_eq!(frame(true).next_needing(), Some(2));
    }

    #[test]
    fn a_state_line_names_the_agent_with_its_state_and_age() {
        let working = agent("coax", Some(AgentState::Working));
        assert_eq!(state_line(Some(&working), 0), "● coax working · 4m");
        let mut asking = agent("coax", Some(AgentState::Attention));
        asking.age = Duration::from_secs(12 * 60);
        assert_eq!(state_line(Some(&asking), 0), "! coax needs you · 12m");
        let silent = agent("coax", None);
        assert_eq!(state_line(Some(&silent), 0), "· coax");
        assert_eq!(state_line(None, 0), "· no agent");
    }

    #[test]
    fn a_state_line_counts_the_agents_it_does_not_show() {
        let done = agent("coax", Some(AgentState::Done));
        assert_eq!(state_line(Some(&done), 2), "✓ coax done · 4m +2");
    }

    #[test]
    fn a_dim_agent_reads_disconnected_with_its_age_or_no_pane() {
        let working = || agent("coax", Some(AgentState::Working));
        let gone = dim(working(), Dim::Disconnected);
        assert_eq!(state_line(Some(&gone), 0), "○ coax disconnected · 4m");
        let paneless = dim(working(), Dim::NoPane);
        assert_eq!(state_line(Some(&paneless), 1), "○ coax no pane +1");
        assert_eq!(agent_tint(Some(&gone)), MUTED);
        assert_eq!(agent_tint(Some(&working())), palette::LIGHT_YELLOW);
    }

    #[test]
    fn the_usage_line_gives_the_model_context_percent_tokens_and_cost() {
        let mut coax = agent("coax", Some(AgentState::Working));
        assert_eq!(usage(&coax, usize::MAX), None);
        coax.model = Some("claude-opus".to_string());
        coax.context = Some(Context {
            used: 124_000,
            limit: 200_000,
        });
        coax.tokens = Some(Tokens {
            input: 15_000,
            output: 3_400,
        });
        coax.cost = Some(0.4213);
        assert_eq!(
            usage(&coax, usize::MAX).as_deref(),
            Some("claude-opus · 62% ctx · 18.4k tok · $0.42")
        );
    }

    #[test]
    fn the_usage_line_leaves_out_what_the_agent_has_not_reported() {
        let mut coax = agent("coax", None);
        coax.tokens = Some(Tokens {
            input: 900_000,
            output: 340_000,
        });
        coax.context = Some(Context { used: 5, limit: 0 });
        assert_eq!(usage(&coax, usize::MAX).as_deref(), Some("1.2M tok"));
        coax.tokens = None;
        coax.cost = Some(3.0);
        assert_eq!(usage(&coax, usize::MAX).as_deref(), Some("$3.00"));
    }

    #[test]
    fn a_usage_line_too_wide_for_its_room_drops_the_model_before_cutting_the_numbers() {
        let mut coax = agent("coax", Some(AgentState::Working));
        coax.model = Some("claude-opus".to_string());
        coax.tokens = Some(Tokens {
            input: 15_000,
            output: 3_400,
        });
        coax.cost = Some(0.42);
        assert_eq!(
            usage(&coax, 40).as_deref(),
            Some("claude-opus · 18.4k tok · $0.42")
        );
        assert_eq!(usage(&coax, 20).as_deref(), Some("18.4k tok · $0.42"));
        coax.tokens = None;
        coax.cost = None;
        assert_eq!(usage(&coax, 6).map(|line| line.chars().count()), Some(6));
    }

    #[test]
    fn a_token_count_takes_a_k_or_m_suffix_past_a_thousand() {
        let counts = [512, 1000, 18_432, 999_949, 999_950, 12_500_000].map(count);
        assert_eq!(counts, ["512", "1.0k", "18.4k", "999.9k", "1.0M", "12.5M"]);
    }

    #[test]
    fn a_card_with_usage_takes_a_line_more() {
        let mut frame = frame(true);
        frame.entries[1].agents[0].cost = Some(0.1);
        let (cards, _) = frame.cards();
        assert_eq!(cards[1].2, 2 * 4 + 4 * 18);
    }

    #[test]
    fn the_card_shows_the_most_urgent_live_agent_and_a_dim_one_only_when_none_is_live() {
        let mut busy = entry(1, "busy", None);
        let mut recent = dim(agent("old", Some(AgentState::Idle)), Dim::Disconnected);
        recent.age = Duration::from_secs(60);
        busy.agents = vec![
            dim(
                agent("asking", Some(AgentState::Attention)),
                Dim::Disconnected,
            ),
            agent("idle", Some(AgentState::Idle)),
            agent("working", Some(AgentState::Working)),
            recent,
        ];
        let mut gone = entry(2, "gone", None);
        gone.agents = vec![
            dim(agent("older", Some(AgentState::Attention)), Dim::NoPane),
            dim(agent("newer", Some(AgentState::Idle)), Dim::Disconnected),
        ];
        gone.agents[0].age = Duration::from_secs(600);
        let frame = Frame::new(
            (120, 40),
            true,
            vec![entry(0, "jack", None), gone, busy],
            (vec![], 0),
            String::new(),
            false,
            (8, 18),
        );
        let names: Vec<Vec<_>> = (frame.entries.iter())
            .map(|entry| entry.agents.iter().map(|a| a.name.as_str()).collect())
            .collect();
        assert_eq!(
            names,
            [
                vec!["working", "idle", "old", "asking"],
                vec![],
                vec!["newer", "older"],
            ]
        );
        assert_eq!(frame.entries[0].state(), Some(AgentState::Working));
        assert_eq!(frame.entries[2].state(), None);
    }

    #[test]
    fn dim_agents_do_not_count_toward_the_state_pills() {
        let mut frame = frame(true);
        for entry in &mut frame.entries {
            if entry.name == "foam" {
                entry.agents[0].dim = Some(Dim::Disconnected);
            }
        }
        let states: Vec<_> = frame.state_pills().iter().map(|pill| pill.hit).collect();
        assert_eq!(
            states,
            [Hit::Next(AgentState::Working), Hit::Next(AgentState::Done)]
        );
        assert_eq!(frame.next_needing(), Some(3));
    }

    #[test]
    fn the_popover_lists_every_agent_with_its_message() {
        let mut frame = frame(false);
        let foam = frame.entries.iter_mut().find(|e| e.name == "foam").unwrap();
        foam.agents.push(agent("helper", Some(AgentState::Working)));
        foam.agents.push(dim(agent("old", None), Dim::Disconnected));
        foam.agents[2].message = Some("reading".to_string());
        frame.hover = Some(Hit::Show(2));
        let popover = frame.popover().unwrap();

        // the name, the path, three agents of which two have a
        // message, and the hint
        assert_eq!(popover.size.1, 2 * 6 + 8 * 18);
    }
}
