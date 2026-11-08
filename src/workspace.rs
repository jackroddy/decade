use std::collections::HashMap;
use std::path::PathBuf;

use smithay::reexports::calloop::RegistrationToken;

use crate::channel::{AGENT, Router};
use crate::panes::{CellRect, Layout, Pane};
use crate::scratch::Scratch;
use decade_channel::AgentState;

pub struct Workspace {
    pub id: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub router: Router,

    /// The event source accepting connections on this workspace's socket.
    pub listener: Option<RegistrationToken>,

    /// Whether the workspace goes once its last client has.
    pub closing: bool,

    /// The agent state each pane last reported, by pane name.
    pub agents: HashMap<String, Agent>,
    pub scratch: Option<Scratch>,
}

/// One split tree of panes in a workspace.
pub struct Tab {
    pub layout: Layout,

    /// Whether the tab goes once its last client has.
    pub closing: bool,
}

pub struct Agent {
    pub state: AgentState,
    pub message: Option<String>,

    /// The socket client that reported it.
    pub client: usize,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// The base name, or the base name with the first free count after it.
pub fn unique_name<'a>(base: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    let free = |name: &str| !taken.clone().any(|taken| taken == name);
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| free(name))
        .unwrap()
}

impl Workspace {
    pub fn new(id: u32, cwd: PathBuf, area: CellRect) -> Workspace {
        let name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        let mut layout = Layout::new(area);
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
        let names = tabs
            .iter()
            .flat_map(|tab| &tab.layout.panes)
            .map(|pane| pane.name.clone())
            .collect();
        let mut router = Router::new(names);
        router.connected(AGENT);
        router.handle(AGENT, r#"{"hello":{"name":"agent"}}"#);
        Workspace {
            id,
            name,
            cwd,
            active_tab: active_tab.min(tabs.len() - 1),
            tabs,
            router,
            listener: None,
            closing: false,
            agents: HashMap::new(),
            scratch: None,
        }
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
    pub fn unique_pane_name(&self, base: &str) -> String {
        if self.by_name(base).is_none() {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}{n}"))
            .find(|name| self.by_name(name).is_none())
            .unwrap()
    }

    /// Add a tab holding one terminal in the workspace's cwd, and return
    /// its index and the terminal's name.
    pub fn add_tab(&mut self, area: CellRect) -> (usize, String) {
        let mut layout = Layout::new(area);
        let name = self.unique_pane_name("terminal");
        layout.panes[0].name = name.clone();
        layout.panes[0].cwd = Some(self.cwd.clone());
        self.tabs.push(Tab {
            layout,
            closing: false,
        });
        self.router.add_pane(name.clone());
        (self.tabs.len() - 1, name)
    }

    /// The most urgent state of the workspace's agents, or `None` when no
    /// agent has reported.
    pub fn agent_state(&self) -> Option<AgentState> {
        self.agents.values().map(|agent| agent.state).max()
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

    /// The app id prefix of a terminal started for one of this workspace's panes.
    pub fn app_id_prefix(&self) -> String {
        format!("decade.w{}.", self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_gets_the_first_free_count() {
        assert_eq!(unique_name("decade", ["foam"].into_iter()), "decade");
        assert_eq!(unique_name("decade", ["decade"].into_iter()), "decade 2");
        let taken = ["decade", "decade 2", "decade 3"];
        assert_eq!(unique_name("decade", taken.into_iter()), "decade 4");
    }

    #[test]
    fn a_workspace_is_named_after_its_directory() {
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack/projects/decade"),
            crate::panes::tests::AREA,
        );
        assert_eq!(workspace.name, "decade");
    }

    #[test]
    fn a_workspace_at_the_root_is_named_after_the_path() {
        let workspace = Workspace::new(0, PathBuf::from("/"), crate::panes::tests::AREA);
        assert_eq!(workspace.name, "/");
    }

    #[test]
    fn a_new_workspace_holds_one_terminal_pane() {
        let workspace = Workspace::new(0, PathBuf::from("/home/jack"), crate::panes::tests::AREA);
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
        let mut workspace =
            Workspace::new(0, PathBuf::from("/home/jack"), crate::panes::tests::AREA);
        let (second, name) = workspace.add_tab(crate::panes::tests::AREA);
        assert_eq!((second, name.as_str()), (1, "terminal2"));
        let (_, name) = workspace.add_tab(crate::panes::tests::AREA);
        assert_eq!(name, "terminal3");
        assert_eq!(workspace.unique_pane_name("terminal"), "terminal4");
        assert_eq!(workspace.unique_pane_name("nautilus"), "nautilus");
        assert_eq!(workspace.by_name("terminal2"), Some((1, 0)));
        let cwd = workspace.tabs[1].layout.panes[0].cwd.as_deref();
        assert_eq!(cwd, Some(std::path::Path::new("/home/jack")));
    }

    #[test]
    fn marking_a_pane_scratch_unmarks_the_one_before_it() {
        let mut workspace =
            Workspace::new(0, PathBuf::from("/home/jack"), crate::panes::tests::AREA);
        workspace.add_tab(crate::panes::tests::AREA);
        workspace.toggle_scratch("terminal");
        assert_eq!(workspace.scratch_pane(), Some("terminal"));
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_pane(), Some("terminal2"));
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_pane(), None);
    }
}
