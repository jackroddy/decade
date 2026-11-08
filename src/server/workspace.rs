use std::path::PathBuf;

use smithay::reexports::calloop::RegistrationToken;

use crate::server::channel::{Out, Router};
use crate::server::panes::{CellRect, Layout, Pane};
use crate::server::scratch::Scratch;
use decade_channel::AgentState;
use serde_json::Value;

pub struct Workspace {
    pub id: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub router: Router,

    /// The event source accepting connections on this workspace's socket.
    pub listener: Option<RegistrationToken>,
    pub scratch: Option<Scratch>,
}

/// One split tree of panes in a workspace.
pub struct Tab {
    pub layout: Layout,

    /// Whether the tab goes once its last client has.
    pub closing: bool,
}

/// The name a pane sees in `from` on a verb sent over MCP.
const AGENT: &str = "agent";

fn pane_names(tabs: &[Tab]) -> Vec<&str> {
    (tabs.iter())
        .flat_map(|tab| &tab.layout.panes)
        .map(|pane| pane.name.as_str())
        .collect()
}

/// The app id prefix of a terminal started for one of the workspace's panes.
pub fn app_id_prefix(workspace: u32) -> String {
    format!("decade.w{workspace}.")
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
        let mut layout = Layout::new(area, "terminal".to_string());
        layout.panes[0].cwd = Some(cwd.clone());
        Workspace::with_tabs(id, name, cwd, vec![layout], 0)
    }

    pub fn with_tabs(
        id: u32,
        name: String,
        cwd: PathBuf,
        layouts: Vec<Layout>,
        active_tab: usize,
    ) -> Workspace {
        let tabs: Vec<Tab> = layouts
            .into_iter()
            .map(|layout| Tab {
                layout,
                closing: false,
            })
            .collect();
        Workspace {
            id,
            name,
            cwd,
            active_tab: active_tab.min(tabs.len() - 1),
            tabs,
            router: Router::default(),
            listener: None,
            scratch: None,
        }
    }

    /// Answer one line from socket client `id`.
    pub fn channel_line(&mut self, id: usize, line: &str) -> Vec<Out> {
        self.router.handle(id, line, &pane_names(&self.tabs))
    }

    /// The id of the client in pane `to` and the line carrying it a pane
    /// verb from the agent, or the reason it cannot go.
    pub fn agent_send(
        &self,
        to: &str,
        verb: String,
        payload: Value,
    ) -> Result<(usize, String), String> {
        let panes = pane_names(&self.tabs);
        self.router.send(AGENT, to, verb, payload, &panes)
    }

    /// Every pane in tab order and the name of the client occupying it.
    pub fn pane_list(&self) -> Vec<decade_channel::Pane> {
        self.router.panes(&pane_names(&self.tabs))
    }

    /// The layout of the active tab.
    pub fn layout(&self) -> &Layout {
        &self.tabs[self.active_tab].layout
    }

    pub fn layout_mut(&mut self) -> &mut Layout {
        &mut self.tabs[self.active_tab].layout
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

    /// The base name, or the base name with the first free count after it,
    /// free across every tab.
    fn unique_pane_name(&self, base: &str) -> String {
        unique_name(base, "", |name| self.by_name(name).is_none())
    }

    /// Add an empty pane named after `base` beside the focused pane of tab
    /// `tab`, and return its id.
    pub fn add_pane(&mut self, tab: usize, base: &str, cwd: Option<PathBuf>) -> usize {
        let name = self.unique_pane_name(base);
        let layout = &mut self.tabs[tab].layout;
        let id = layout.add_transient(name);
        if let Some(pane) = layout.pane_mut(id) {
            pane.cwd = cwd;
        }
        id
    }

    /// Add a tab holding one terminal in the workspace's cwd, and return
    /// its index and the terminal's name.
    pub fn add_tab(&mut self, area: CellRect) -> (usize, String) {
        let name = self.unique_pane_name("terminal");
        let mut layout = Layout::new(area, name.clone());
        layout.panes[0].cwd = Some(self.cwd.clone());
        self.tabs.push(Tab {
            layout,
            closing: false,
        });
        (self.tabs.len() - 1, name)
    }

    /// Whether every tab goes once its last client has.
    pub fn closing(&self) -> bool {
        self.tabs.iter().all(|tab| tab.closing)
    }

    /// The closing tabs that no client is left in.
    pub fn closed_tabs(&self) -> impl Iterator<Item = usize> + '_ {
        self.tabs.iter().enumerate().filter_map(|(index, tab)| {
            let clients = tab.layout.panes.iter().any(|pane| !pane.clients.is_empty());
            (tab.closing && !clients).then_some(index)
        })
    }

    /// Remove a tab and its panes, keeping the active tab where it was
    /// unless it was the one removed. Return whether it was.
    pub fn remove_tab(&mut self, tab: usize) -> bool {
        let removed = self.tabs.remove(tab);
        for pane in &removed.layout.panes {
            self.forget_pane(&pane.name);
        }
        let was_active = self.active_tab == tab;
        if self.active_tab > tab || self.active_tab == self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        was_active
    }

    /// Drop what the workspace keeps about a pane that has gone: its
    /// client's claim, its agent state and its scratch mark.
    pub fn forget_pane(&mut self, name: &str) {
        self.router.release(name);
        if self.scratch_pane() == Some(name) {
            self.scratch = None;
        }
    }

    /// Whether the pane stays when it has no client: it is pinned or it is
    /// the scratch pane.
    pub fn keeps_empty(&self, pane: &Pane) -> bool {
        pane.pinned || self.is_scratch(pane)
    }

    pub fn is_scratch(&self, pane: &Pane) -> bool {
        self.scratch_pane() == Some(pane.name.as_str())
    }

    /// The most urgent state of the workspace's agents, or `None` when no
    /// agent has reported.
    pub fn agent_state(&self) -> Option<AgentState> {
        self.router.statuses().map(|status| status.state).max()
    }

    /// The name of the scratch pane, if the workspace has one.
    pub fn scratch_pane(&self) -> Option<&str> {
        self.scratch.as_ref().map(|scratch| scratch.pane.as_str())
    }

    /// Mark the pane as the scratch pane, or unmark it if it is already.
    pub fn toggle_scratch(&mut self, name: &str) {
        self.scratch = if self.scratch_pane() == Some(name) {
            None
        } else {
            Some(Scratch::new(name.to_string()))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_home() -> Workspace {
        Workspace::new(
            0,
            PathBuf::from("/home/jack"),
            crate::server::panes::tests::AREA,
        )
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
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack/projects/decade"),
            crate::server::panes::tests::AREA,
        );
        assert_eq!(workspace.name, "decade");
    }

    #[test]
    fn a_workspace_at_the_root_is_named_after_the_path() {
        let workspace = Workspace::new(0, PathBuf::from("/"), crate::server::panes::tests::AREA);
        assert_eq!(workspace.name, "/");
    }

    #[test]
    fn a_new_workspace_holds_one_terminal_pane() {
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack"),
            crate::server::panes::tests::AREA,
        );
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
        let (second, name) = workspace.add_tab(crate::server::panes::tests::AREA);
        assert_eq!((second, name.as_str()), (1, "terminal2"));
        let (_, name) = workspace.add_tab(crate::server::panes::tests::AREA);
        assert_eq!(name, "terminal3");
        assert_eq!(workspace.unique_pane_name("terminal"), "terminal4");
        assert_eq!(workspace.unique_pane_name("nautilus"), "nautilus");
        assert_eq!(workspace.by_name("terminal2"), Some((1, 0)));
        let cwd = workspace.tabs[1].layout.panes[0].cwd.as_deref();
        assert_eq!(cwd, Some(std::path::Path::new("/home/jack")));
    }

    #[test]
    fn an_added_pane_takes_a_free_name() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        let id = workspace.add_pane(0, "terminal", Some(PathBuf::from("/tmp")));
        let pane = workspace.tabs[0].layout.pane(id).unwrap();
        assert_eq!(pane.name, "terminal3");
        assert_eq!(pane.cwd.as_deref(), Some(std::path::Path::new("/tmp")));
        assert_eq!(workspace.tabs[0].layout.focused, id);
    }

    #[test]
    fn an_empty_pane_is_kept_while_it_is_pinned_or_the_scratch_pane() {
        let mut workspace = at_home();
        assert!(!workspace.keeps_empty(&workspace.layout().panes[0]));
        workspace.toggle_scratch("terminal");
        assert!(workspace.keeps_empty(&workspace.layout().panes[0]));
        workspace.toggle_scratch("terminal");
        workspace.layout_mut().pin(0, "decade.terminal".to_string());
        assert!(workspace.keeps_empty(&workspace.layout().panes[0]));
    }

    #[test]
    fn a_closing_tab_with_no_client_is_closed_and_the_workspace_closes_with_its_last_tab() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
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
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.active_tab = 2;
        assert!(workspace.remove_tab(2));
        assert_eq!(workspace.active_tab, 1);
        assert!(!workspace.remove_tab(0));
        assert_eq!(workspace.active_tab, 0);
        assert_eq!(workspace.by_name("terminal3"), None);
        assert_eq!(workspace.by_name("terminal2"), Some((0, 0)));
    }

    #[test]
    fn removing_the_tab_that_holds_the_scratch_pane_leaves_no_scratch_pane() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.toggle_scratch("terminal2");
        workspace.remove_tab(1);
        assert_eq!(workspace.scratch_pane(), None);
    }

    #[test]
    fn marking_a_pane_scratch_unmarks_the_one_before_it() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.toggle_scratch("terminal");
        assert_eq!(workspace.scratch_pane(), Some("terminal"));
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_pane(), Some("terminal2"));
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_pane(), None);
    }

    #[test]
    fn a_pane_that_goes_takes_its_clients_claim_and_agent_state_with_it() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(1, r#"{"hello": {"name": "coax", "pane": "terminal2"}}"#);
        workspace.channel_line(1, r#"{"status": {"state": "working"}}"#);
        assert_eq!(workspace.agent_state(), Some(AgentState::Working));
        workspace.remove_tab(1);
        assert_eq!(workspace.agent_state(), None);
        assert!(workspace.router.occupant("terminal2").is_none());
    }

    #[test]
    fn a_verb_from_the_agent_reaches_the_pane_occupant_from_agent() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(
            4,
            r#"{"hello": {"name": "code-pane", "pane": "terminal2"}}"#,
        );
        let show = serde_json::json!({"path": "a.rs"});
        let (client, line) = workspace
            .agent_send("terminal2", "show".into(), show)
            .unwrap();
        assert_eq!(client, 4);
        assert_eq!(line, r#"{"from":"agent","show":{"path":"a.rs"}}"#);
        let refused = workspace.agent_send("terminal", "show".into(), Value::Null);
        assert_eq!(refused, Err("no client occupies pane terminal".to_string()));
        let panes: Vec<_> = (workspace.pane_list().into_iter())
            .map(|pane| (pane.name, pane.client))
            .collect();
        let occupant = Some("code-pane".to_string());
        assert_eq!(
            panes,
            [("terminal".into(), None), ("terminal2".into(), occupant)]
        );
    }
}
