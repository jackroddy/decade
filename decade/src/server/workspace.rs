use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::shell::xdg::ToplevelSurface;

use decade_ui::{Button, CellRect, ContentId, Marks, Placeholder, Tag};

use crate::server::channel::{Out, PaneNames, Router};
use crate::server::clients::Client;
use crate::server::save::TerminalTab;
use crate::server::scratch::Scratch;
use decade_channel::{AgentState, Restore};
use serde_json::Value;

const PIN_ICON: char = '\u{f0403}';
const UNPINNED_ICON: char = '\u{f0931}';
const NO_SCRATCH: &str =
    "this workspace has no scratch pane; the person marks one from the Ctrl bar";

const SCRATCH_ICON: char = '\u{f0093}';
const UNSCRATCHED_ICON: char = '\u{f0096}';
const CAPTURES_ICON: char = '\u{f0100}';
const NO_CAPTURES_ICON: char = '\u{f05df}';

pub type Layout = decade_ui::Layout<PaneAction>;
pub type Pane = decade_ui::Pane<PaneAction>;

/// What a button decade puts on a pane runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneAction {
    Captures,
    Scratch,
    Pin,
    Restart,
    Wake,
}

pub struct Workspace {
    pub id: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub router: Router,

    /// The event source accepting connections on this workspace's socket.
    pub listener: Option<RegistrationToken>,

    /// The scratch panes, in the order they were marked.
    pub scratch: Vec<Scratch>,

    /// The git branch checked out at `cwd`, as last read.
    pub branch: Option<String>,
    branch_read: Option<Instant>,

    /// The pane the workspace's agent points at.
    pub point: Option<Point>,

    /// The id of the sidebar group the workspace is in.
    pub group: Option<u32>,

    /// The pane captured last, and when, while its camera tag flashes.
    pub flash: Option<(String, Instant)>,

    /// The terminal tabs to start on wake, or `None` while the workspace
    /// is awake.
    pub asleep: Option<Vec<TerminalTab>>,

    /// The id the next client to arrive in a pane gets.
    next_content: u64,
}

/// An agent pointing at a pane, from the MCP tool `point`.
pub struct Point {
    pub pane: String,
    pub note: Option<String>,
    pub since: Instant,
}

/// One split tree of panes in a workspace.
pub struct Tab {
    pub layout: Layout,

    /// What decade keeps about each pane of `layout`, by pane id.
    pub info: HashMap<usize, PaneInfo>,

    /// The window each tab of the panes shows.
    pub clients: HashMap<ContentId, Client>,

    /// The name the person gave the tab, shown in place of its label.
    pub name: Option<String>,

    /// Whether the tab goes once its last client has.
    pub closing: bool,
}

/// What decade keeps about a pane besides its place in the layout.
pub struct PaneInfo {
    /// Whether the pane stays, waiting for its program, when its last
    /// client goes.
    pub pinned: bool,

    /// The number in the app id of the next terminal tab decade starts.
    pub next_tab: u32,

    /// The app id of the last client in the pane that is not a terminal
    /// tab, which an empty pane waits for.
    pub app_id: Option<String>,

    /// Where decade starts the pane's shell, for a pane that holds a terminal.
    pub cwd: Option<PathBuf>,

    /// The ghostty decade started for the pane.
    pub pid: Option<u32>,

    /// The restore block of the program decade starts in the pane, until
    /// a client claims the pane.
    pub restore: Option<Restore>,

    /// The name of the live agent in the pane, which labels its first tab.
    pub agent: Option<String>,

    /// Whether an agent may capture the pane without asking.
    pub captures: bool,
}

impl Default for PaneInfo {
    fn default() -> PaneInfo {
        PaneInfo {
            pinned: false,
            next_tab: 2,
            app_id: None,
            cwd: None,
            pid: None,
            restore: None,
            agent: None,
            captures: false,
        }
    }
}

impl Tab {
    /// A tab holding `layout`, with nothing kept yet about its panes.
    pub fn new(layout: Layout) -> Tab {
        let info = (layout.panes.iter())
            .map(|pane| (pane.id, PaneInfo::default()))
            .collect();
        Tab {
            layout,
            info,
            clients: HashMap::new(),
            name: None,
            closing: false,
        }
    }

    pub fn info(&self, id: usize) -> &PaneInfo {
        &self.info[&id]
    }

    pub fn info_mut(&mut self, id: usize) -> &mut PaneInfo {
        self.info.get_mut(&id).unwrap()
    }

    /// Split pane `at` to add a pane named `name`, focus the new pane, and
    /// return its id.
    pub fn add_pane(&mut self, at: usize, name: String) -> usize {
        let id = self.layout.add_beside(at, name);
        self.info.insert(id, PaneInfo::default());
        id
    }

    /// Remove pane `id` and the clients in its tabs. The layout's last
    /// pane stays, with no tabs.
    pub fn remove_pane(&mut self, id: usize) {
        if let Some(pane) = self.layout.pane(id) {
            for tab in &pane.tabs {
                self.clients.remove(&tab.content);
            }
        }
        self.layout.remove(id);
        if self.layout.pane(id).is_none() {
            self.info.remove(&id);
        }
    }

    /// The clients in the pane's tabs, in tab order.
    pub fn clients_in<'a>(&'a self, pane: &'a Pane) -> impl Iterator<Item = &'a Client> {
        (pane.tabs.iter()).map(|tab| &self.clients[&tab.content])
    }

    /// The window of the tab pane `id` draws.
    pub fn toplevel(&self, id: usize) -> Option<&ToplevelSurface> {
        let content = self.layout.pane(id)?.content()?;
        Some(&self.clients[&content].toplevel)
    }

    /// The pane with a tab showing `surface`.
    pub fn pane_of_surface(&self, surface: &WlSurface) -> Option<usize> {
        (self.layout.panes.iter())
            .find(|pane| {
                self.clients_in(pane)
                    .any(|client| client.toplevel.wl_surface() == surface)
            })
            .map(|pane| pane.id)
    }

    /// Add a window as a tab of pane `id` and show it. A terminal tab
    /// decade started goes last. Any other window goes first, and an empty
    /// pane waits for its app id.
    pub fn add_client(&mut self, id: usize, content: ContentId, client: Client) {
        let Some(pane) = self.layout.pane_mut(id) else {
            return;
        };
        let tab = decade_ui::Tab {
            content,
            label: String::new(),
        };
        if client.terminal_tab {
            pane.add_tab(pane.tabs.len(), tab);
        } else {
            if let Some(info) = self.info.get_mut(&id) {
                info.app_id = client.app_id.clone();
            }
            pane.add_tab(0, tab);
        }
        self.clients.insert(content, client);
        self.relabel(id);
    }

    /// Remove the tab of pane `id` that shows `toplevel`.
    pub fn remove_client(&mut self, id: usize, toplevel: &ToplevelSurface) {
        let Some(pane) = self.layout.pane_mut(id) else {
            return;
        };
        let index =
            (pane.tabs.iter()).position(|tab| self.clients[&tab.content].toplevel == *toplevel);
        if let Some(index) = index {
            let removed = pane.remove_tab(index);
            self.clients.remove(&removed.content);
        }
        self.relabel(id);
    }

    /// Record `pid` as the ghostty decade started for pane `id`.
    pub fn started(&mut self, id: usize, pid: u32) {
        self.info_mut(id).pid = Some(pid);
        self.relabel(id);
    }

    /// Whether pane `id` is a pinned terminal whose shell has exited.
    pub fn restartable(&self, id: usize) -> bool {
        let (Some(pane), Some(info)) = (self.layout.pane(id), self.info.get(&id)) else {
            return false;
        };
        pane.tabs.is_empty() && info.cwd.is_some() && info.pinned
    }

    /// The pane a new window with this app id goes in, and whether the
    /// window is a terminal tab decade started for that pane.
    pub fn place(&self, app_id: Option<&str>, prefix: &str) -> Option<(usize, bool)> {
        let app_id = app_id?;
        let panes = &self.layout.panes;
        if let Some(pane) = (panes.iter())
            .find(|pane| (pane.names()).any(|name| is_terminal_tab(app_id, prefix, name)))
        {
            return Some((pane.id, true));
        }

        // a restored pane's own terminal can arrive after its tabs
        if let Some(pane) = panes.iter().find(|pane| {
            (app_id.strip_prefix(prefix)).is_some_and(|name| pane.answers_to(name))
                && self.clients_in(pane).all(|client| client.terminal_tab)
        }) {
            return Some((pane.id, false));
        }
        (panes.iter())
            .find(|pane| {
                pane.tabs.is_empty() && self.info(pane.id).app_id.as_deref() == Some(app_id)
            })
            .map(|pane| (pane.id, false))
    }

    /// Pin the pane to wait for a window with the app id when it is empty.
    pub fn pin(&mut self, id: usize, app_id: String) {
        if let Some(info) = self.info.get_mut(&id) {
            info.pinned = true;
            info.app_id = Some(app_id);
        }
    }

    pub fn unpin(&mut self, id: usize) {
        if let Some(info) = self.info.get_mut(&id) {
            info.pinned = false;
        }
    }

    /// Read again what the clients of pane `id` whose labels are drawn
    /// run: every client when the pane shows its tabs, or else the shown
    /// one.
    fn refresh_running(&mut self, id: usize, now: Instant, home: &Path) {
        let Some(pane) = self.layout.pane(id) else {
            return;
        };
        let pane_pid = self.info(id).pid;
        let drawn = if pane.tabs.len() > 1 {
            &pane.tabs[..]
        } else {
            pane.tabs.get(pane.shown..=pane.shown).unwrap_or_default()
        };
        for tab in drawn {
            if let Some(client) = self.clients.get_mut(&tab.content) {
                client.refresh_running(pane_pid, now, home);
            }
        }
        self.relabel(id);
    }

    /// Label each tab of pane `id` with what its client runs, or else
    /// with its number.
    fn relabel(&mut self, id: usize) {
        let Some(pane) = self.layout.pane_mut(id) else {
            return;
        };
        let info = &self.info[&id];
        for (index, tab) in pane.tabs.iter_mut().enumerate() {
            let client = &self.clients[&tab.content];
            tab.label = tab_label(info, client, index).unwrap_or_else(|| (index + 1).to_string());
        }
    }

    /// What runs in pane `id`: the agent's name when the shown tab is its
    /// first, or else what its shown client runs, or the pane's name.
    pub fn running_label(&self, id: usize) -> Option<String> {
        let pane = self.layout.pane(id)?;
        let label = (pane.tabs.get(pane.shown))
            .and_then(|tab| tab_label(self.info(id), &self.clients[&tab.content], pane.shown));
        Some(label.unwrap_or_else(|| pane.name.clone()))
    }

    /// The title of pane `id`'s shown client, or the last part of its app
    /// id, or else the pane's name.
    pub fn title(&self, id: usize) -> Option<String> {
        let pane = self.layout.pane(id)?;
        let client = pane.content().map(|content| &self.clients[&content]);
        Some(
            client
                .and_then(Client::label)
                .unwrap_or_else(|| pane.name.clone()),
        )
    }

    /// Set the marks of every pane from what decade keeps about it, with
    /// the panes named in `scratch` as scratch panes, in a workspace that
    /// is `asleep` or awake.
    pub fn mark_panes(&mut self, scratch: &[&str], asleep: bool) {
        for pane in &mut self.layout.panes {
            let scratched = scratch.contains(&pane.name.as_str());
            pane.marks = marks(&self.info[&pane.id], scratched, asleep);
        }
    }
}

/// The buttons, tags and placeholder of a pane with `info`.
fn marks(info: &PaneInfo, scratched: bool, asleep: bool) -> Marks<PaneAction> {
    let button = |on: bool, icons: (char, char), action| Button {
        icon: if on { icons.0 } else { icons.1 },
        on,
        action,
    };
    let buttons = vec![
        button(
            info.captures,
            (CAPTURES_ICON, NO_CAPTURES_ICON),
            PaneAction::Captures,
        ),
        button(
            scratched,
            (SCRATCH_ICON, UNSCRATCHED_ICON),
            PaneAction::Scratch,
        ),
        button(info.pinned, (PIN_ICON, UNPINNED_ICON), PaneAction::Pin),
    ];
    let tags = [
        info.pinned.then_some(Tag::Icon(PIN_ICON)),
        Some(Tag::Zoom),
        scratched.then(|| Tag::Text("scratch".to_string())),
    ];

    // `Tab::restartable` without its check for no tabs, since
    // the layout draws a placeholder only on a pane with none
    let placeholder = if asleep {
        Placeholder {
            lines: vec!["asleep".to_string()],
            button: Some(("wake".to_string(), PaneAction::Wake)),
        }
    } else if info.cwd.is_some() && info.pinned {
        Placeholder {
            lines: vec!["shell exited".to_string()],
            button: Some(("restart".to_string(), PaneAction::Restart)),
        }
    } else if scratched {
        Placeholder {
            lines: vec!["scratch".to_string(), "agents show things here".to_string()],
            button: None,
        }
    } else {
        let line = match &info.app_id {
            Some(app_id) if info.pinned => format!("waiting for {app_id}"),
            _ => "no client".to_string(),
        };
        Placeholder {
            lines: vec![line],
            button: None,
        }
    };
    Marks {
        buttons,
        left_tag: info.captures.then_some(CAPTURES_ICON),
        tags: tags.into_iter().flatten().collect(),
        placeholder,
    }
}

/// The agent's name for the first tab, or else what tab `index`'s client
/// runs.
fn tab_label(info: &PaneInfo, client: &Client, index: usize) -> Option<String> {
    // an agent names only the pane it claimed, not which of the
    // pane's clients runs it, so the first tab takes the name
    match &info.agent {
        Some(agent) if index == 0 => Some(agent.clone()),
        _ => client.running_label(info.pid),
    }
}

/// Whether the app id is `{prefix}{pane}.t{n}`, the class of a terminal
/// decade starts as a tab of the pane.
fn is_terminal_tab(app_id: &str, prefix: &str, pane: &str) -> bool {
    app_id
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(pane))
        .and_then(|rest| rest.strip_prefix(".t"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The name a pane sees in `from` on a verb sent over MCP.
const AGENT: &str = "agent";

const BRANCH_EVERY: Duration = Duration::from_secs(2);

/// The time from one peak of a point's pulse to the next.
const PULSE_PERIOD: Duration = Duration::from_millis(1600);

/// How long a point lasts unless something ends it sooner.
const POINT_FOR: Duration = Duration::from_secs(30);

/// How long a pane's camera tag flashes after a capture.
const FLASH_FOR: Duration = Duration::from_millis(700);

/// How opaque the white layer over a captured pane starts.
const SHUTTER_ALPHA: f32 = 0.3;

/// How long the white layer over a captured pane takes to fade out.
const SHUTTER_FOR: Duration = Duration::from_millis(300);

/// The opacity of the white layer over a captured pane `elapsed` after
/// the capture, easing out from `SHUTTER_ALPHA` to 0.
pub fn shutter(elapsed: Duration) -> f32 {
    let left = (1.0 - elapsed.as_secs_f32() / SHUTTER_FOR.as_secs_f32()).max(0.0);
    SHUTTER_ALPHA * left * left
}

/// How far a point's pulse is toward its peak `elapsed` after it began,
/// from 0 to 1.
pub fn pulse(elapsed: Duration) -> f32 {
    let phase = elapsed.as_secs_f32() / PULSE_PERIOD.as_secs_f32();
    (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0
}

pub fn expired(elapsed: Duration) -> bool {
    elapsed >= POINT_FOR
}

impl Point {
    pub fn pulse(&self, now: Instant) -> f32 {
        pulse(now.saturating_duration_since(self.since))
    }
}

fn pane_names(tabs: &[Tab]) -> PaneNames<'_> {
    let panes = || tabs.iter().flat_map(|tab| &tab.layout.panes);
    PaneNames {
        current: panes().map(|pane| pane.name.as_str()).collect(),
        former: panes()
            .flat_map(|pane| {
                (pane.aliases.iter()).map(|alias| (alias.as_str(), pane.name.as_str()))
            })
            .collect(),
    }
}

/// Refuse an empty name, and one that `taken` says something else holds.
pub fn check_name(name: &str, taken: bool) -> Result<(), String> {
    if name.is_empty() {
        return Err("a name cannot be empty".to_string());
    }
    if taken {
        return Err(format!("{name} is taken"));
    }
    Ok(())
}

/// The app id prefix of a terminal started for one of the workspace's panes.
pub fn app_id_prefix(workspace: u32) -> String {
    format!("decade.w{workspace}.")
}

/// The branch checked out in the git repository holding `cwd`, or the
/// first 7 characters of the commit when HEAD is detached.
pub fn branch(cwd: &Path) -> Option<String> {
    let dot_git = cwd
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|git| git.exists())?;
    let git_dir = if dot_git.is_file() {
        let link = std::fs::read_to_string(&dot_git).ok()?;
        let target = link.strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(target)
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            Some(name.to_string())
        }
        None => Some(head.chars().take(7).collect()),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `base` if `free` accepts it, or else `base`, `separator` and the first
/// count from 2 that `free` accepts.
pub fn unique_name(base: &str, separator: &str, free: impl Fn(&str) -> bool) -> String {
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}{separator}{n}"))
        .find(|name| free(name))
        .unwrap()
}

impl Workspace {
    pub fn new(id: u32, cwd: PathBuf, area: CellRect) -> Workspace {
        let name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        let mut tab = Tab::new(Layout::new(area, "terminal".to_string()));
        tab.info_mut(0).cwd = Some(cwd.clone());
        Workspace::with_tabs(id, name, cwd, vec![tab], 0)
    }

    pub fn with_tabs(
        id: u32,
        name: String,
        cwd: PathBuf,
        tabs: Vec<Tab>,
        active_tab: usize,
    ) -> Workspace {
        let mut workspace = Workspace {
            id,
            name,
            cwd,
            active_tab: active_tab.min(tabs.len() - 1),
            tabs,
            router: Router::default(),
            listener: None,
            scratch: Vec::new(),
            branch: None,
            branch_read: None,
            point: None,
            group: None,
            flash: None,
            asleep: None,
            next_content: 0,
        };
        workspace.mark_panes();
        workspace
    }

    /// Read the branch again if it was last read `BRANCH_EVERY` or more
    /// before `now`.
    pub fn refresh_branch(&mut self, now: Instant) {
        if self
            .branch_read
            .is_some_and(|read| now.duration_since(read) < BRANCH_EVERY)
        {
            return;
        }
        self.branch = branch(&self.cwd);
        self.branch_read = Some(now);
    }

    /// Make `cwd` the workspace's directory, and read its branch at `now`.
    pub fn change_directory(&mut self, cwd: PathBuf, now: Instant) {
        self.branch = branch(&cwd);
        self.branch_read = Some(now);
        self.cwd = cwd;
    }

    /// Read again what runs in each pane of the active tab and in each
    /// other tab's focused pane, where it was last read long enough
    /// before `now`.
    pub fn refresh_running(&mut self, now: Instant, home: &Path) {
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let focused = tab.layout.focused;
            let active = index == self.active_tab;
            for index in 0..tab.layout.panes.len() {
                let id = tab.layout.panes[index].id;
                if active || id == focused {
                    tab.refresh_running(id, now, home);
                }
            }
        }
    }

    /// Give each pane the name of the live agent in it, or `None`.
    pub fn name_agents(&mut self) {
        let agents = &self.router.agents.list;
        for tab in &mut self.tabs {
            for index in 0..tab.layout.panes.len() {
                let pane = &tab.layout.panes[index];
                let agent = (agents.iter())
                    .find(|agent| agent.live() && agent.pane.as_deref() == Some(pane.name.as_str()))
                    .map(|agent| agent.name.clone());
                let id = pane.id;
                let info = tab.info_mut(id);
                if info.agent != agent {
                    info.agent = agent;
                    tab.relabel(id);
                }
            }
        }
    }

    /// Add a window as a tab of pane `id` in tab `tab`, and show it.
    pub fn add_client(&mut self, tab: usize, id: usize, client: Client) {
        let content = ContentId(self.next_content);
        self.next_content += 1;
        self.tabs[tab].add_client(id, content, client);
        self.mark_panes();
    }

    /// Answer one line from socket client `id`.
    pub fn channel_line(&mut self, id: usize, line: &str) -> Vec<Out> {
        let scratch: Vec<&str> = (self.scratch.iter())
            .map(|scratch| scratch.pane.as_str())
            .collect();
        let out = self
            .router
            .handle(id, line, &pane_names(&self.tabs), &scratch);
        let claimed = self
            .router
            .clients
            .get(&id)
            .and_then(|client| client.pane.as_deref());
        if let Some((tab, pane)) = claimed.and_then(|pane| self.by_name(pane)) {
            self.tabs[tab].info_mut(pane).restore = None;
        }
        out
    }

    /// The id of the client in pane `to` and the line carrying it a pane
    /// verb from the agent, or the reason it cannot go.
    pub fn agent_send(
        &self,
        to: &str,
        verb: String,
        payload: Value,
    ) -> Result<(usize, String), String> {
        self.router
            .send(AGENT, to, verb, payload, &pane_names(&self.tabs))
    }

    /// The pane an agent's show, highlight or clear of `kind` goes to.
    pub fn showing(&self, kind: &str, pane: Option<&str>) -> Result<String, String> {
        self.router.showing(kind, pane, &pane_names(&self.tabs))
    }

    /// Every pane in tab order and the name of the client occupying it,
    /// marking the scratch panes and the pane `you`.
    pub fn pane_list(&self, you: Option<&str>) -> Vec<decade_channel::Pane> {
        let names = pane_names(&self.tabs);
        let you = you.and_then(|you| names.resolve(you));
        self.router.panes(&names, &self.scratch_panes(), you)
    }

    /// The layout of the active tab.
    pub fn layout(&self) -> &Layout {
        &self.tabs[self.active_tab].layout
    }

    pub fn layout_mut(&mut self) -> &mut Layout {
        &mut self.tabs[self.active_tab].layout
    }

    /// The active tab.
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    /// The panes of every tab.
    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.tabs.iter().flat_map(|tab| &tab.layout.panes)
    }

    /// The tab and id of the pane with this name.
    pub fn by_name(&self, name: &str) -> Option<(usize, usize)> {
        self.tabs
            .iter()
            .enumerate()
            .find_map(|(index, tab)| Some((index, tab.layout.by_name(name)?)))
    }

    /// The tab and pane a window from a process that named `into` and
    /// `pane` goes by, and whether it joins that pane as a tab rather than
    /// opening a pane beside it. A name no pane answers to is skipped, and
    /// the focused pane is the last resort.
    pub fn launch_target(&self, into: Option<&str>, pane: Option<&str>) -> (usize, usize, bool) {
        if let Some((tab, id)) = into.and_then(|name| self.by_name(name)) {
            return (tab, id, true);
        }
        let (tab, id) = pane
            .and_then(|name| self.by_name(name))
            .unwrap_or((self.active_tab, self.layout().focused));
        (tab, id, false)
    }

    /// The base name, or the base name with the first free count after it,
    /// free across every tab.
    fn unique_pane_name(&self, base: &str) -> String {
        unique_name(base, "", |name| self.by_name(name).is_none())
    }

    /// Add an empty pane named after `base` beside pane `at` of tab `tab`,
    /// and return its id.
    pub fn add_pane(&mut self, tab: usize, at: usize, base: &str, cwd: Option<PathBuf>) -> usize {
        let name = self.unique_pane_name(base);
        let entry = &mut self.tabs[tab];
        let id = entry.add_pane(at, name);
        entry.info_mut(id).cwd = cwd;
        self.mark_panes();
        id
    }

    /// Add a tab holding one terminal in the workspace's cwd, and return
    /// its index and the terminal's name.
    pub fn add_tab(&mut self, area: CellRect) -> (usize, String) {
        let name = self.unique_pane_name("terminal");
        let mut tab = Tab::new(Layout::new(area, name.clone()));
        tab.info_mut(0).cwd = Some(self.cwd.clone());
        self.tabs.push(tab);
        self.mark_panes();
        (self.tabs.len() - 1, name)
    }

    /// Rename pane `id` of tab `tab` to `name`, which no other pane may
    /// answer to. Its old name stays an alias, and what the workspace keeps
    /// by pane name follows it.
    pub fn rename_pane(&mut self, tab: usize, id: usize, name: &str) -> Result<(), String> {
        let old = (self.tabs.get(tab))
            .and_then(|entry| entry.layout.pane(id))
            .map(|pane| pane.name.clone())
            .ok_or("the pane has gone")?;
        if name == old {
            return Ok(());
        }
        if name.contains(['&', '=', ' ']) {
            return Err("a pane name holds no &, = or space".to_string());
        }
        let taken = self.by_name(name).is_some_and(|found| found != (tab, id));
        check_name(name, taken)?;
        if let Some(pane) = self.tabs[tab].layout.pane_mut(id) {
            pane.rename(name.to_string());
        }
        self.router.rename(&old, name);
        if let Some(scratch) = self.scratch.iter_mut().find(|scratch| scratch.pane == old) {
            scratch.pane = name.to_string();
        }
        if let Some(point) = &mut self.point
            && point.pane == old
        {
            point.pane = name.to_string();
        }
        self.mark_panes();
        Ok(())
    }

    /// Name tab `tab` `name`, which no other tab may hold.
    pub fn rename_tab(&mut self, tab: usize, name: &str) -> Result<(), String> {
        if tab >= self.tabs.len() {
            return Err("the tab has gone".to_string());
        }
        let taken = (self.tabs.iter().enumerate())
            .any(|(index, other)| index != tab && other.name.as_deref() == Some(name));
        check_name(name, taken)?;
        self.tabs[tab].name = Some(name.to_string());
        Ok(())
    }

    /// Whether every tab goes once its last client has.
    pub fn closing(&self) -> bool {
        self.tabs.iter().all(|tab| tab.closing)
    }

    /// The closing tabs that no client is left in.
    pub fn closed_tabs(&self) -> impl Iterator<Item = usize> + '_ {
        (self.tabs.iter().enumerate())
            .filter_map(|(index, tab)| (tab.closing && tab.clients.is_empty()).then_some(index))
    }

    /// Remove a tab and its panes, keeping the active tab where it was
    /// unless it was the one removed. Return whether it was.
    pub fn remove_tab(&mut self, tab: usize) -> bool {
        let removed = self.tabs.remove(tab);
        for pane in &removed.layout.panes {
            self.forget_pane(&pane.name);
        }
        if let Some(asleep) = &mut self.asleep {
            asleep.retain(|stored| stored.tab != tab);
            for stored in asleep.iter_mut().filter(|stored| stored.tab > tab) {
                stored.tab -= 1;
            }
        }
        let was_active = self.active_tab == tab;
        if self.active_tab > tab || self.active_tab == self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        was_active
    }

    /// Drop what the workspace keeps about a pane that has gone: its
    /// client's and agent's claim, its scratch mark and a point at it.
    pub fn forget_pane(&mut self, name: &str) {
        self.router.release(name);
        self.scratch.retain(|scratch| scratch.pane != name);
        self.end_point_at(name);
        self.mark_panes();
    }

    /// Point at pane `pane` from `now`, in place of any earlier point.
    pub fn point_at(
        &mut self,
        pane: &str,
        note: Option<String>,
        now: Instant,
    ) -> Result<(), String> {
        let Some((tab, id)) = self.by_name(pane) else {
            return Err(format!("no pane named {pane}"));
        };
        let pane = &self.tabs[tab].layout.pane(id).unwrap().name;
        self.point = Some(Point {
            pane: pane.clone(),
            note,
            since: now,
        });
        Ok(())
    }

    /// End the point if it is at pane `pane`.
    pub fn end_point_at(&mut self, pane: &str) {
        if self.point.as_ref().is_some_and(|point| point.pane == pane) {
            self.point = None;
        }
    }

    /// End the point if it has expired by `now`, and return whether one is
    /// left.
    pub fn keep_point(&mut self, now: Instant) -> bool {
        if (self.point.as_ref())
            .is_some_and(|point| expired(now.saturating_duration_since(point.since)))
        {
            self.point = None;
        }
        self.point.is_some()
    }

    /// The pane whose camera tag flashes at `now`, and how bright it is,
    /// fading from 1 to 0 over `FLASH_FOR`.
    pub fn flash_at(&self, now: Instant) -> Option<(&str, f32)> {
        let (pane, since) = self.flash.as_ref()?;
        let elapsed = now.saturating_duration_since(*since);
        let left = 1.0 - elapsed.as_secs_f32() / FLASH_FOR.as_secs_f32();
        (left > 0.0).then_some((pane.as_str(), left))
    }

    /// The pane under the white layer of a capture at `now`, and the
    /// layer's opacity.
    pub fn shutter_at(&self, now: Instant) -> Option<(&str, f32)> {
        let (pane, since) = self.flash.as_ref()?;
        let alpha = shutter(now.saturating_duration_since(*since));
        (alpha > 0.0).then_some((pane.as_str(), alpha))
    }

    /// End the flash if it has faded by `now`, and return whether one is
    /// left.
    pub fn keep_flash(&mut self, now: Instant) -> bool {
        if self.flash_at(now).is_none() {
            self.flash = None;
        }
        self.flash.is_some()
    }

    /// Whether the pane of tab `tab` stays when it has no client: it is
    /// pinned, it is a scratch pane, or the workspace is asleep.
    pub fn keeps_empty(&self, tab: usize, pane: &Pane) -> bool {
        self.asleep.is_some() || self.tabs[tab].info(pane.id).pinned || self.is_scratch(pane)
    }

    /// Whether tab `tab` has nothing left in it and closes: no pane has a
    /// client or a running program, and none is kept empty. The
    /// workspace's only tab never does.
    pub fn tab_is_spent(&self, tab: usize, mut running: impl FnMut(u32) -> bool) -> bool {
        let entry = &self.tabs[tab];
        self.tabs.len() > 1
            && entry.layout.panes.iter().all(|pane| {
                pane.tabs.is_empty()
                    && !self.keeps_empty(tab, pane)
                    && !entry.info(pane.id).pid.is_some_and(&mut running)
            })
    }

    pub fn is_scratch(&self, pane: &Pane) -> bool {
        self.scratch.iter().any(|scratch| scratch.pane == pane.name)
    }

    /// Whether a live agent in the workspace is working.
    pub fn working(&self) -> bool {
        (self.router.agents.states()).any(|state| state == AgentState::Working)
    }

    /// The most urgent state of the workspace's live agents, or `None`
    /// when no live agent has reported.
    pub fn agent_state(&self) -> Option<AgentState> {
        self.router.agents.states().max()
    }

    /// Set the marks of every pane in every tab from what decade keeps
    /// about it.
    pub fn mark_panes(&mut self) {
        let scratch: Vec<&str> = (self.scratch.iter())
            .map(|scratch| scratch.pane.as_str())
            .collect();
        for tab in &mut self.tabs {
            tab.mark_panes(&scratch, self.asleep.is_some());
        }
    }

    /// Put the workspace to sleep as `restored`, the same workspace rebuilt
    /// from its saved record: take its tabs and scratch panes, drop its
    /// agents, point and flash, and keep `tabs` to start on wake.
    pub fn sleep(&mut self, restored: Workspace, tabs: Vec<TerminalTab>) {
        self.tabs = restored.tabs;
        self.active_tab = restored.active_tab;
        self.scratch = restored.scratch;
        self.router = Router::default();
        self.point = None;
        self.flash = None;
        self.asleep = Some(tabs);
        self.mark_panes();
    }

    /// The names of the scratch panes, in the order they were marked.
    pub fn scratch_panes(&self) -> Vec<&str> {
        (self.scratch.iter())
            .map(|scratch| scratch.pane.as_str())
            .collect()
    }

    /// The index in `scratch` of the scratch pane `pane` names, now or
    /// formerly, or of the only scratch pane when `pane` is `None`.
    pub fn find_scratch(&self, pane: Option<&str>) -> Result<usize, String> {
        let names = self.scratch_panes();
        let found = match pane {
            None if names.len() == 1 => Some(0),
            None => None,
            Some(pane) => (pane_names(&self.tabs).resolve(pane))
                .and_then(|pane| names.iter().position(|&name| name == pane)),
        };
        found.ok_or_else(|| {
            let listed = names.join(", ");
            match pane {
                _ if names.is_empty() => NO_SCRATCH.to_string(),
                None => format!(
                    "this workspace has several scratch panes, so name one of them: {listed}"
                ),
                Some(pane) => {
                    format!("{pane} is not a scratch pane; the scratch panes are: {listed}")
                }
            }
        })
    }

    /// Mark the pane as a scratch pane, or unmark it if it is one.
    pub fn toggle_scratch(&mut self, name: &str) {
        match self.scratch.iter().position(|scratch| scratch.pane == name) {
            Some(index) => {
                self.scratch.remove(index);
            }
            None => self.scratch.push(Scratch::new(name.to_string())),
        }
        self.mark_panes();
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use decade_ui::{BarButton, Chrome, Shape};

    pub const AREA: CellRect = CellRect {
        column: 0,
        row: 0,
        columns: 100,
        rows: 40,
    };

    fn at_home() -> Workspace {
        Workspace::new(0, PathBuf::from("/home/jack"), AREA)
    }

    #[test]
    fn the_white_layer_of_a_capture_starts_at_its_alpha_and_eases_out_to_nothing() {
        assert_eq!(shutter(Duration::ZERO), SHUTTER_ALPHA);
        let halfway = shutter(SHUTTER_FOR / 2);
        assert!((halfway - SHUTTER_ALPHA / 4.0).abs() < 1e-6);
        assert_eq!(shutter(SHUTTER_FOR), 0.0);
        assert_eq!(shutter(SHUTTER_FOR * 2), 0.0);
    }

    #[test]
    fn a_flash_keeps_the_timer_running_until_it_has_faded() {
        let mut shown = at_home();
        let now = Instant::now();
        assert!(!shown.keep_flash(now));
        shown.flash = Some(("terminal".to_string(), now));
        assert!(shown.keep_flash(now + SHUTTER_FOR));
        assert!(shown.shutter_at(now + SHUTTER_FOR).is_none());
        assert!(shown.keep_flash(now + FLASH_FOR / 2));
        assert!(!shown.keep_flash(now + FLASH_FOR));
        assert!(shown.flash.is_none());
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("decade-branch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_branch_is_read_from_head_in_the_cwd_or_a_directory_above_it() {
        let repo = fresh_dir("plain");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/server")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/dev\n").unwrap();
        assert_eq!(branch(&repo).as_deref(), Some("dev"));
        assert_eq!(branch(&repo.join("src/server")).as_deref(), Some("dev"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn a_git_file_points_at_the_directory_holding_head() {
        let root = fresh_dir("worktree");
        let git_dir = root.join("repo/.git/worktrees/feature");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::create_dir_all(root.join("feature")).unwrap();
        let link = "gitdir: ../repo/.git/worktrees/feature\n";
        std::fs::write(root.join("feature/.git"), link).unwrap();
        assert_eq!(branch(&root.join("feature")).as_deref(), Some("feature"));

        let absolute = format!("gitdir: {}\n", git_dir.display());
        std::fs::write(root.join("feature/.git"), absolute).unwrap();
        assert_eq!(branch(&root.join("feature")).as_deref(), Some("feature"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_detached_head_shows_seven_characters_of_its_commit() {
        let repo = fresh_dir("detached");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let commit = "b142ad7c0ffee0000000000000000000000000000\n";
        std::fs::write(repo.join(".git/HEAD"), commit).unwrap();
        assert_eq!(branch(&repo).as_deref(), Some("b142ad7"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn changing_the_directory_sets_the_cwd_and_reads_the_branch_there() {
        let repo = fresh_dir("change");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/dev\n").unwrap();
        let mut workspace = Workspace::new(0, PathBuf::from("/"), AREA);
        let name = workspace.name.clone();
        workspace.change_directory(repo.clone(), Instant::now());
        assert_eq!(workspace.cwd, repo);
        assert_eq!(workspace.branch.as_deref(), Some("dev"));
        assert_eq!(workspace.name, name);
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn a_directory_outside_any_repository_has_no_branch() {
        let dir = fresh_dir("none");
        if dir.ancestors().all(|above| !above.join(".git").exists()) {
            assert_eq!(branch(&dir), None);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_taken_name_gets_the_first_free_count() {
        let free_of = |taken: &'static [&str]| move |name: &str| !taken.contains(&name);
        assert_eq!(unique_name("decade", " ", free_of(&["foam"])), "decade");
        assert_eq!(unique_name("decade", " ", free_of(&["decade"])), "decade 2");
        let taken = &["decade", "decade 2", "decade 3"];
        assert_eq!(unique_name("decade", " ", free_of(taken)), "decade 4");
    }

    #[test]
    fn a_workspace_is_named_after_its_directory() {
        let workspace = Workspace::new(0, PathBuf::from("/home/jack/projects/decade"), AREA);
        assert_eq!(workspace.name, "decade");
    }

    #[test]
    fn a_workspace_at_the_root_is_named_after_the_path() {
        let workspace = Workspace::new(0, PathBuf::from("/"), AREA);
        assert_eq!(workspace.name, "/");
    }

    #[test]
    fn a_new_workspace_holds_one_terminal_pane() {
        let workspace = Workspace::new(0, PathBuf::from("/home/jack"), AREA);
        let names: Vec<&str> = workspace
            .layout()
            .panes
            .iter()
            .map(|pane| pane.name.as_str())
            .collect();
        assert_eq!(names, ["terminal"]);
    }

    #[test]
    fn pane_names_stay_unique_across_tabs() {
        let mut workspace = at_home();
        let (second, name) = workspace.add_tab(AREA);
        assert_eq!((second, name.as_str()), (1, "terminal2"));
        let (_, name) = workspace.add_tab(AREA);
        assert_eq!(name, "terminal3");
        assert_eq!(workspace.unique_pane_name("terminal"), "terminal4");
        assert_eq!(workspace.unique_pane_name("nautilus"), "nautilus");
        assert_eq!(workspace.by_name("terminal2"), Some((1, 0)));
        let cwd = workspace.tabs[1].info(0).cwd.as_deref();
        assert_eq!(cwd, Some(std::path::Path::new("/home/jack")));
    }

    #[test]
    fn an_added_pane_takes_a_free_name() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        let id = workspace.add_pane(
            0,
            workspace.tabs[0].layout.focused,
            "terminal",
            Some(PathBuf::from("/tmp")),
        );
        let pane = workspace.tabs[0].layout.pane(id).unwrap();
        assert_eq!(pane.name, "terminal3");
        let cwd = workspace.tabs[0].info(id).cwd.as_deref();
        assert_eq!(cwd, Some(std::path::Path::new("/tmp")));
        assert_eq!(workspace.tabs[0].layout.focused, id);
    }

    #[test]
    fn an_empty_pane_is_kept_while_it_is_pinned_or_the_scratch_pane() {
        let mut workspace = at_home();
        assert!(!workspace.keeps_empty(0, &workspace.layout().panes[0]));
        workspace.toggle_scratch("terminal");
        assert!(workspace.keeps_empty(0, &workspace.layout().panes[0]));
        workspace.toggle_scratch("terminal");
        workspace.tab_mut().pin(0, "decade.terminal".to_string());
        assert!(workspace.keeps_empty(0, &workspace.layout().panes[0]));
    }

    #[test]
    fn a_tab_with_no_client_running_program_or_kept_pane_is_spent_unless_it_is_the_only_tab() {
        let mut workspace = at_home();
        assert!(!workspace.tab_is_spent(0, |_| false));
        workspace.add_tab(AREA);
        assert!(workspace.tab_is_spent(1, |_| false));
        workspace.tabs[1].info_mut(0).pid = Some(7);
        assert!(!workspace.tab_is_spent(1, |pid| pid == 7));
        assert!(workspace.tab_is_spent(1, |_| false));
        let name = workspace.tabs[1].layout.panes[0].name.clone();
        workspace.toggle_scratch(&name);
        assert!(!workspace.tab_is_spent(1, |_| false));
        workspace.toggle_scratch(&name);
        let id = workspace.tabs[1].layout.panes[0].id;
        workspace.tabs[1].pin(id, "decade.terminal".to_string());
        assert!(!workspace.tab_is_spent(1, |_| false));
    }

    #[test]
    fn a_closing_tab_with_no_client_is_closed_and_the_workspace_closes_with_its_last_tab() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        assert_eq!(workspace.closed_tabs().count(), 0);
        workspace.tabs[1].closing = true;
        assert_eq!(workspace.closed_tabs().collect::<Vec<_>>(), [1]);
        assert!(!workspace.closing());
        workspace.tabs[0].closing = true;
        assert!(workspace.closing());
    }

    #[test]
    fn removing_the_active_last_tab_makes_the_one_before_it_active() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.add_tab(AREA);
        workspace.active_tab = 2;
        assert!(workspace.remove_tab(2));
        assert_eq!(workspace.active_tab, 1);
        assert!(!workspace.remove_tab(0));
        assert_eq!(workspace.active_tab, 0);
        assert_eq!(workspace.by_name("terminal3"), None);
        assert_eq!(workspace.by_name("terminal2"), Some((0, 0)));
    }

    #[test]
    fn removing_a_tab_of_a_sleeping_workspace_drops_its_terminal_tabs_and_shifts_the_later_ones() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.add_tab(AREA);
        let stored = |tab| TerminalTab {
            tab,
            pane: 0,
            cwd: PathBuf::from("/tmp"),
            restore: None,
        };
        workspace.asleep = Some(vec![stored(0), stored(1), stored(2)]);
        workspace.remove_tab(1);
        assert_eq!(workspace.asleep, Some(vec![stored(0), stored(1)]));
    }

    #[test]
    fn removing_the_tab_that_holds_the_scratch_pane_leaves_no_scratch_pane() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.toggle_scratch("terminal2");
        workspace.remove_tab(1);
        assert!(workspace.scratch.is_empty());
    }

    #[test]
    fn marking_a_pane_scratch_leaves_the_others_marked() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.toggle_scratch("terminal");
        assert_eq!(workspace.scratch_panes(), ["terminal"]);
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_panes(), ["terminal", "terminal2"]);
        workspace.toggle_scratch("terminal");
        assert_eq!(workspace.scratch_panes(), ["terminal2"]);
    }

    #[test]
    fn a_scratch_pane_is_found_by_name_or_alone_and_a_miss_lists_the_scratch_panes() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        assert!(
            (workspace.find_scratch(None).unwrap_err())
                .starts_with("this workspace has no scratch pane")
        );
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.find_scratch(None), Ok(0));
        workspace.toggle_scratch("terminal");
        assert_eq!(
            workspace.find_scratch(None),
            Err("this workspace has several scratch panes, so name one of them: terminal2, terminal".to_string())
        );
        assert_eq!(workspace.find_scratch(Some("terminal")), Ok(1));
        workspace.rename_pane(1, 0, "build").unwrap();
        assert_eq!(workspace.find_scratch(Some("terminal2")), Ok(0));
        assert_eq!(workspace.find_scratch(Some("build")), Ok(0));
        workspace.toggle_scratch("terminal");
        assert_eq!(
            workspace.find_scratch(Some("terminal")),
            Err("terminal is not a scratch pane; the scratch panes are: build".to_string())
        );
    }

    #[test]
    fn a_restore_block_waiting_in_a_pane_is_dropped_once_a_client_claims_the_pane() {
        let mut workspace = at_home();
        let waiting = Restore {
            argv: vec!["decade-notes".to_string()],
            terminal: false,
            cwd: None,
        };
        workspace.tab_mut().info_mut(0).restore = Some(waiting.clone());
        workspace.channel_line(1, r#"{"hello": {"name": "list"}}"#);
        assert_eq!(workspace.tab().info(0).restore, Some(waiting));
        workspace.channel_line(2, r#"{"hello": {"name": "notes", "pane": "terminal"}}"#);
        assert_eq!(workspace.tab().info(0).restore, None);
    }

    #[test]
    fn a_pane_that_goes_takes_its_clients_claim_and_leaves_its_agent_without_a_pane() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.channel_line(
            1,
            r#"{"hello": {"name": "coax", "pane": "terminal2", "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
        );
        workspace.channel_line(1, r#"{"agent": {"state": "working"}}"#);
        assert_eq!(workspace.agent_state(), Some(AgentState::Working));
        workspace.remove_tab(1);
        assert_eq!(workspace.agent_state(), None);
        assert_eq!(workspace.router.agents.list[0].pane, None);
        assert!(workspace.router.occupant("terminal2").is_none());
    }

    #[test]
    fn a_pane_takes_the_name_of_its_live_agent_and_loses_it_when_the_agent_disconnects() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.channel_line(
            1,
            r#"{"hello": {"name": "coax", "pane": "terminal2", "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
        );
        workspace.name_agents();
        let agent = |workspace: &Workspace, tab: usize| workspace.tabs[tab].info(0).agent.clone();
        assert_eq!(agent(&workspace, 0), None);
        assert_eq!(agent(&workspace, 1).as_deref(), Some("coax"));
        workspace.router.disconnect(1, Instant::now());
        workspace.name_agents();
        assert_eq!(agent(&workspace, 1), None);
    }

    #[test]
    fn a_verb_from_the_agent_reaches_the_pane_occupant_from_agent() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.channel_line(
            4,
            r#"{"hello": {"name": "code-pane", "pane": "terminal2", "shows": [{"kind": "file"}]}}"#,
        );
        let to = workspace.showing("file", None).unwrap();
        assert_eq!(to, "terminal2");
        let show = serde_json::json!({"kind": "file", "target": "a.rs"});
        let (client, line) = workspace.agent_send(&to, "show".into(), show).unwrap();
        assert_eq!(client, 4);
        assert_eq!(
            line,
            r#"{"from":"agent","show":{"kind":"file","target":"a.rs"}}"#
        );
        let refused = workspace.agent_send("terminal", "show".into(), Value::Null);
        assert_eq!(refused, Err("no client occupies pane terminal".to_string()));
        let panes: Vec<_> = (workspace.pane_list(None).into_iter())
            .map(|pane| (pane.name, pane.client))
            .collect();
        let occupant = Some("code-pane".to_string());
        assert_eq!(
            panes,
            [("terminal".into(), None), ("terminal2".into(), occupant)]
        );
    }

    #[test]
    fn a_pulse_rises_from_nothing_to_its_peak_and_back_each_period() {
        let at = |ms| pulse(Duration::from_millis(ms));
        assert!(at(0) < 1e-6);
        assert!((at(400) - 0.5).abs() < 1e-3);
        assert!((at(800) - 1.0).abs() < 1e-6);
        assert!((at(1200) - 0.5).abs() < 1e-3);
        assert!(at(1600) < 1e-6);
        assert!(at(100) < at(200) && at(900) > at(1000));
    }

    #[test]
    fn a_point_expires_after_thirty_seconds() {
        assert!(!expired(Duration::from_millis(29_999)));
        assert!(expired(Duration::from_secs(30)));
        let mut workspace = at_home();
        let start = Instant::now();
        workspace.point_at("terminal", None, start).unwrap();
        assert!(workspace.keep_point(start + Duration::from_secs(29)));
        assert!(!workspace.keep_point(start + Duration::from_secs(30)));
        assert!(workspace.point.is_none());
    }

    #[test]
    fn a_new_point_replaces_the_one_before_and_an_unknown_pane_is_refused() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        let now = Instant::now();
        workspace.point_at("terminal", None, now).unwrap();
        let note = Some("this one".to_string());
        workspace.point_at("terminal2", note.clone(), now).unwrap();
        let point = workspace.point.as_ref().unwrap();
        assert_eq!((point.pane.as_str(), &point.note), ("terminal2", &note));
        let refused = workspace.point_at("chat", None, now);
        assert_eq!(refused, Err("no pane named chat".to_string()));
        assert_eq!(workspace.point.as_ref().unwrap().pane, "terminal2");
    }

    #[test]
    fn a_point_ends_when_its_pane_is_focused_or_goes_and_not_for_another_pane() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        workspace.end_point_at("terminal");
        assert!(workspace.point.is_some());
        workspace.end_point_at("terminal2");
        assert!(workspace.point.is_none());
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        workspace.remove_tab(1);
        assert!(workspace.point.is_none());
    }

    #[test]
    fn a_renamed_pane_keeps_its_claims_and_its_old_name_still_reaches_it() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.channel_line(
            1,
            r#"{"hello": {"name": "coax", "pane": "terminal2", "accepts": ["ping"], "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
        );
        workspace.toggle_scratch("terminal2");
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        workspace.rename_pane(1, 0, "build").unwrap();

        assert_eq!(workspace.by_name("build"), Some((1, 0)));
        assert_eq!(workspace.by_name("terminal2"), Some((1, 0)));
        assert_eq!(workspace.scratch_panes(), ["build"]);
        assert_eq!(workspace.point.as_ref().unwrap().pane, "build");
        assert_eq!(
            workspace.router.agents.list[0].pane.as_deref(),
            Some("build")
        );
        assert!(workspace.router.occupant("build").is_some());

        let (client, _) = (workspace.agent_send("terminal2", "ping".into(), Value::Null)).unwrap();
        assert_eq!(client, 1);
        let listed: Vec<_> = (workspace.pane_list(Some("terminal2")).into_iter())
            .map(|pane| (pane.name, pane.you))
            .collect();
        assert_eq!(
            listed,
            [("terminal".to_string(), false), ("build".to_string(), true)]
        );
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        assert_eq!(workspace.point.as_ref().unwrap().pane, "build");
    }

    #[test]
    fn a_pane_name_must_be_new_to_the_workspace_and_hold_no_address_characters() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.rename_pane(1, 0, "build").unwrap();
        for name in ["", "a b", "a&b", "a=b", "build"] {
            assert!(workspace.rename_pane(0, 0, name).is_err(), "{name:?}");
        }
        assert!(workspace.rename_pane(0, 0, "terminal2").is_err());
        assert_eq!(workspace.tabs[0].layout.panes[0].name, "terminal");

        // a pane may take back a name it had
        workspace.rename_pane(1, 0, "terminal2").unwrap();
        assert_eq!(workspace.tabs[1].layout.panes[0].aliases, ["build"]);
        workspace.rename_pane(1, 0, "build").unwrap();
        assert_eq!(workspace.tabs[1].layout.panes[0].aliases, ["terminal2"]);
    }

    #[test]
    fn a_new_pane_does_not_take_a_name_a_renamed_pane_answers_to() {
        let mut workspace = at_home();
        workspace.rename_pane(0, 0, "shell").unwrap();
        let id = workspace.add_pane(0, workspace.tabs[0].layout.focused, "terminal", None);
        assert_eq!(workspace.tabs[0].layout.pane(id).unwrap().name, "terminal2");
    }

    #[test]
    fn a_tab_name_must_be_new_among_the_tabs_and_not_empty() {
        let mut workspace = at_home();
        workspace.add_tab(AREA);
        workspace.rename_tab(0, "work").unwrap();
        assert!(workspace.rename_tab(1, "work").is_err());
        assert!(workspace.rename_tab(1, "").is_err());
        workspace.rename_tab(0, "work").unwrap();
        workspace.rename_tab(1, "play").unwrap();
        assert_eq!(workspace.tabs[1].name.as_deref(), Some("play"));
    }

    fn three_anchors() -> Tab {
        let mut layout = Layout::new(AREA, "chat".to_string());
        layout.add_beside(0, "terminal".to_string());
        layout.add_beside(1, "code".to_string());
        layout.focused = 1;
        let mut tab = Tab::new(layout);
        for (id, name) in [(0, "chat"), (1, "terminal"), (2, "code")] {
            tab.pin(id, format!("decade.{name}"));
        }
        tab
    }

    fn add_transient(tab: &mut Tab, name: &str) -> usize {
        let focused = tab.layout.focused;
        tab.add_pane(focused, name.to_string())
    }

    fn drawn_texts(tab: &mut Tab) -> Vec<String> {
        tab.mark_panes(&[], false);
        layout_texts(&tab.layout)
    }

    /// The text of every shape the layout draws, trimmed.
    fn layout_texts(layout: &Layout) -> Vec<String> {
        let font = decade_ui::Font::load(14.0);
        let mut scene = decade_ui::Scene::new(&font);
        layout.draw(&mut scene, Chrome::default());
        (scene.shapes.iter())
            .filter_map(|shape| match shape {
                Shape::Text { text, .. } => Some(text.trim().to_string()),
                _ => None,
            })
            .collect()
    }

    /// The text of each tag on pane `id`'s outline, left to right.
    fn tags(layout: &Layout, id: usize) -> Vec<String> {
        let font = decade_ui::Font::load(14.0);
        let mut scene = decade_ui::Scene::new(&font);
        layout.draw(&mut scene, Chrome::default());
        let (cell_width, cell_height) = scene.cell();
        let rect = layout.pane(id).unwrap().rect;
        let (left, top) = scene.px(rect.column, rect.row);
        let right = left + rect.columns as i32 * cell_width;
        let mut tags: Vec<(i32, String)> = (scene.shapes.iter())
            .filter_map(|shape| match shape {
                Shape::Text { at, text, .. }
                    if at.y < top + cell_height / 2 && (left..right).contains(&at.x) =>
                {
                    Some((at.x, text.clone()))
                }
                _ => None,
            })
            .collect();
        tags.sort_by_key(|&(x, _)| x);
        tags.into_iter().map(|(_, text)| text).collect()
    }

    /// The icon and colour of each of decade's buttons on pane `id`'s bar,
    /// left to right.
    fn bar_icons(layout: &Layout, id: usize) -> Vec<(char, decade_ui::palette::Rgba)> {
        let font = decade_ui::Font::load(14.0);
        let mut scene = decade_ui::Scene::new(&font);
        let chrome = Chrome {
            bar: true,
            ..Chrome::default()
        };
        layout.draw_bars(&mut scene, chrome);
        let row = scene.px(0, layout.pane(id).unwrap().rect.inner().row).1;
        let rect = layout.pane(id).unwrap().rect;
        let (left, _) = scene.px(rect.column, rect.row);
        let right = left + scene.px(rect.columns, 0).0;
        let icons: Vec<_> = (scene.shapes.iter())
            .filter_map(|shape| match shape {
                Shape::Text {
                    at,
                    text,
                    face: decade_ui::Face::Icon,
                    color,
                } if at.y == row && (left..right).contains(&at.x) => {
                    Some((text.chars().next().unwrap(), *color))
                }
                _ => None,
            })
            .collect();
        icons[..icons.len() - 2].to_vec()
    }

    #[test]
    fn a_terminal_started_under_a_former_name_lands_in_the_renamed_pane() {
        let mut tab = three_anchors();
        let id = add_transient(&mut tab, "terminal2");
        tab.layout.pane_mut(id).unwrap().rename("build".to_string());
        assert_eq!(tab.layout.by_name("terminal2"), Some(id));
        assert_eq!(
            tab.place(Some("decade.w0.terminal2"), "decade.w0."),
            Some((id, false))
        );
        assert_eq!(
            tab.place(Some("decade.w0.terminal2.t3"), "decade.w0."),
            Some((id, true))
        );
        assert_eq!(
            tab.place(Some("decade.w0.build.t2"), "decade.w0."),
            Some((id, true))
        );
    }

    #[test]
    fn a_transient_pane_is_placed_by_the_app_id_of_the_terminal_started_for_it() {
        let mut tab = three_anchors();
        let id = add_transient(&mut tab, "terminal2");
        assert_eq!(
            tab.place(Some("decade.w0.terminal2"), "decade.w0."),
            Some((id, false))
        );
        assert_eq!(tab.place(Some("decade.w0.terminal3"), "decade.w0."), None);
    }

    #[test]
    fn a_pinned_transient_pane_waits_for_its_clients_app_id() {
        let mut tab = three_anchors();
        let id = add_transient(&mut tab, "nautilus");
        tab.pin(id, "org.gnome.Nautilus".to_string());
        assert_eq!(
            tab.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some((id, false))
        );
        assert_eq!(tab.layout.pane(id).unwrap().name, "nautilus");
    }

    #[test]
    fn an_unpinned_pane_no_longer_waits_for_its_window() {
        let mut tab = three_anchors();
        tab.unpin(0);
        let texts = drawn_texts(&mut tab);
        assert!(!texts.iter().any(|text| text == "waiting for decade.chat"));
        assert!(texts.iter().any(|text| text == "waiting for decade.code"));
    }

    #[test]
    fn a_swapped_pane_keeps_its_pin_and_app_id() {
        let mut tab = three_anchors();
        tab.layout.apply_drop(0, decade_ui::Drop::Swap(2));
        let code = tab.info(2);
        assert!(code.pinned);
        assert_eq!(code.app_id.as_deref(), Some("decade.code"));
    }

    #[test]
    fn a_pinned_terminal_whose_shell_exited_shows_a_restart_button() {
        let mut tab = Tab::new(Layout::new(AREA, "terminal".to_string()));
        tab.info_mut(0).cwd = Some(PathBuf::from("/home/jack"));
        tab.pin(0, "decade.w0.terminal".to_string());
        assert!(tab.restartable(0));
        let texts = drawn_texts(&mut tab);
        assert!(texts.iter().any(|text| text == "shell exited"));
        assert!(texts.iter().any(|text| text == "restart"));
    }

    #[test]
    fn a_pinned_gui_pane_without_its_window_waits_and_offers_no_restart() {
        let mut tab = Tab::new(Layout::new(AREA, "terminal".to_string()));
        tab.pin(0, "org.gnome.Nautilus".to_string());
        assert!(!tab.restartable(0));
        let texts = drawn_texts(&mut tab);
        assert!(
            texts
                .iter()
                .any(|text| text == "waiting for org.gnome.Nautilus")
        );
        assert!(!texts.iter().any(|text| text == "restart"));
    }

    #[test]
    fn a_click_anywhere_on_a_restartable_pane_restarts_it_and_not_once_it_has_a_tab() {
        let mut workspace = at_home();
        workspace.tab_mut().pin(0, "decade.w0.terminal".to_string());
        workspace.mark_panes();
        assert_eq!(
            workspace.layout().placeholder_action(0),
            Some(PaneAction::Restart)
        );
        workspace.tab_mut().unpin(0);
        workspace.mark_panes();
        assert_eq!(workspace.layout().placeholder_action(0), None);
    }

    #[test]
    fn every_empty_pane_of_a_sleeping_workspace_says_asleep_with_a_wake_button() {
        let mut workspace = at_home();
        let tab = workspace.tab_mut();
        tab.pin(0, "decade.w0.terminal".to_string());
        let files = add_transient(tab, "files");
        tab.info_mut(files).app_id = Some("org.gnome.Nautilus".to_string());
        add_transient(tab, "notes");
        workspace.toggle_scratch("notes");
        workspace.asleep = Some(Vec::new());
        workspace.mark_panes();
        let texts = layout_texts(workspace.layout());
        assert_eq!(texts.iter().filter(|text| *text == "asleep").count(), 3);
        assert_eq!(texts.iter().filter(|text| *text == "wake").count(), 3);
        for gone in [
            "shell exited",
            "restart",
            "no client",
            "agents show things here",
        ] {
            assert!(!texts.iter().any(|text| text == gone), "{gone}");
        }
        for id in 0..3 {
            assert_eq!(
                workspace.layout().placeholder_action(id),
                Some(PaneAction::Wake)
            );
        }
    }

    #[test]
    fn an_empty_scratch_pane_says_it_is_the_scratch_pane() {
        let mut workspace = Workspace::new(0, PathBuf::from("/"), AREA);
        workspace.toggle_scratch("terminal");
        let texts = layout_texts(workspace.layout());
        assert!(texts.iter().any(|text| text == "scratch"));
        assert!(texts.iter().any(|text| text == "agents show things here"));
        assert!(!texts.iter().any(|text| text == "no client"));
    }

    #[test]
    fn a_pinned_zoomed_scratch_pane_has_its_pin_in_the_corner_then_zoom_then_scratch() {
        let mut workspace = at_home();
        workspace.tab_mut().pin(0, "decade.w0.terminal".to_string());
        workspace.layout_mut().toggle_zoom(0);
        workspace.toggle_scratch("terminal");
        assert_eq!(
            tags(workspace.layout(), 0),
            ["scratch", "zoom", &PIN_ICON.to_string()]
        );
        workspace.tab_mut().unpin(0);
        workspace.toggle_scratch("terminal");
        assert_eq!(tags(workspace.layout(), 0), ["zoom"]);
    }

    #[test]
    fn a_pane_accepting_captures_has_the_camera_tag_at_the_left() {
        let mut workspace = at_home();
        workspace.tab_mut().info_mut(0).captures = true;
        workspace.mark_panes();
        assert_eq!(tags(workspace.layout(), 0), [CAPTURES_ICON.to_string()]);
    }

    #[test]
    fn the_bar_has_captures_scratch_and_pin_buttons_bright_when_on_or_focused() {
        let mut workspace = at_home();
        workspace.add_pane(0, 0, "code", None);
        workspace.layout_mut().focused = 1;
        let muted = decade_ui::palette::get().muted;
        let bright = decade_ui::palette::get().foreground;
        assert_eq!(
            bar_icons(workspace.layout(), 0),
            [
                (NO_CAPTURES_ICON, muted),
                (UNSCRATCHED_ICON, muted),
                (UNPINNED_ICON, muted),
            ]
        );
        assert_eq!(
            bar_icons(workspace.layout(), 1),
            [
                (NO_CAPTURES_ICON, bright),
                (UNSCRATCHED_ICON, bright),
                (UNPINNED_ICON, bright),
            ]
        );
        workspace.tab_mut().info_mut(0).captures = true;
        workspace.tab_mut().pin(0, "decade.w0.terminal".to_string());
        workspace.toggle_scratch("terminal");
        assert_eq!(
            bar_icons(workspace.layout(), 0),
            [
                (CAPTURES_ICON, bright),
                (SCRATCH_ICON, bright),
                (PIN_ICON, bright),
            ]
        );
    }

    #[test]
    fn the_bar_buttons_hit_the_actions_where_they_are_drawn() {
        let workspace = at_home();
        let layout = workspace.layout();
        let inner = layout.pane(0).unwrap().rect.inner();
        let end = inner.column + inner.columns;
        let hit = |column| layout.bar_hit(column, inner.row).map(|(_, button)| button);
        assert_eq!(hit(end - 1), Some(BarButton::Close));
        assert_eq!(hit(end - 5), Some(BarButton::Zoom));
        assert_eq!(hit(end - 9), Some(BarButton::Action(PaneAction::Pin)));
        assert_eq!(hit(end - 13), Some(BarButton::Action(PaneAction::Scratch)));
        assert_eq!(hit(end - 17), Some(BarButton::Action(PaneAction::Captures)));
        assert_eq!(hit(end - 21), Some(BarButton::Name));
    }

    #[test]
    fn a_window_with_a_tab_class_goes_into_its_pane_even_when_the_pane_has_a_client() {
        let mut tab = three_anchors();
        let id = add_transient(&mut tab, "terminal2");
        assert_eq!(
            tab.place(Some("decade.w0.terminal2.t2"), "decade.w0."),
            Some((id, true))
        );
        assert_eq!(
            tab.place(Some("decade.w0.terminal.t13"), "decade.w0."),
            Some((1, true))
        );
        assert_eq!(tab.place(Some("decade.w0.terminal2.t"), "decade.w0."), None);
        assert_eq!(
            tab.place(Some("decade.w0.terminal3.t2"), "decade.w0."),
            None
        );
        assert_eq!(
            tab.place(Some("decade.w1.terminal2.t2"), "decade.w0."),
            None
        );
    }

    #[test]
    fn removing_a_pane_drops_what_is_kept_about_it_but_the_last_pane_keeps_its_own() {
        let mut tab = three_anchors();
        tab.remove_pane(2);
        assert!(!tab.info.contains_key(&2));
        tab.remove_pane(1);
        tab.remove_pane(0);
        assert!(tab.info(0).pinned);
        assert_eq!(tab.info.len(), 1);
    }

    #[test]
    fn a_launch_into_a_named_pane_joins_it_and_otherwise_falls_back_to_the_launching_pane() {
        let mut shown = at_home();
        let first = shown.layout().focused;
        let second = shown.add_pane(0, first, "scratch", None);
        let third = shown.add_pane(0, second, "logs", None);
        shown.rename_pane(0, second, "notes").unwrap();
        let focused = shown.layout().focused;
        assert_eq!(shown.launch_target(Some("notes"), None), (0, second, true));
        assert_eq!(
            shown.launch_target(Some("scratch"), Some("logs")),
            (0, second, true)
        );
        assert_eq!(
            shown.launch_target(Some("nowhere"), Some("logs")),
            (0, third, false)
        );
        assert_eq!(
            shown.launch_target(Some("nowhere"), None),
            (0, focused, false)
        );
        assert_eq!(shown.launch_target(None, Some("logs")), (0, third, false));
    }
}
