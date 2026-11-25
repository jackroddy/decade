use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use decade_ui::CellRect;

use crate::chrome::frame::Group;
use crate::files;
use crate::server::agents::Agents;
use crate::server::channel::Router;
use crate::server::proc;
use crate::server::scratch::descends_from;
use crate::server::workspace::{Layout, Pane, Tab, Workspace};
use decade_channel::Restore;
use decade_ui::{Axis, Node};

const VERSION: u32 = 8;

#[derive(Serialize, Deserialize)]
pub struct File {
    version: u32,
    pub sidebar_open: bool,

    /// The sidebar's groups, in the order it lists them.
    #[serde(default, rename = "group", skip_serializing_if = "Vec::is_empty")]
    groups: Vec<SavedGroup>,
    #[serde(default, rename = "workspace")]
    pub workspaces: Vec<Saved>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct SavedGroup {
    name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    folded: bool,
}

/// One workspace as the file holds it.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Saved {
    name: String,
    cwd: PathBuf,
    active_tab: usize,

    /// The names of the scratch panes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scratch: Vec<String>,

    /// The name of the group the workspace is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    sleeping: bool,
    #[serde(rename = "tab")]
    tabs: Vec<SavedTab>,
}

/// A version 1 file, whose workspaces hold one tree each and which keeps no
/// sidebar state.
#[derive(Deserialize)]
struct FileV1 {
    #[serde(default, rename = "workspace")]
    workspaces: Vec<SavedV1>,
}

#[derive(Deserialize)]
struct SavedV1 {
    name: String,
    cwd: PathBuf,
    #[serde(flatten)]
    tab: SavedTab,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct SavedTab {
    /// The name the person gave the tab; version 2 has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    focused: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zoom: Option<String>,
    tree: SavedNode,
    #[serde(rename = "pane")]
    panes: Vec<SavedPane>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
enum SavedNode {
    Leaf {
        pane: String,
    },
    Split {
        axis: SavedAxis,
        ratio: f32,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum SavedAxis {
    Horizontal,
    Vertical,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct SavedPane {
    name: String,
    #[serde(default)]
    pinned: bool,

    // a terminal pane has a cwd, and any other pane may have an app id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,

    /// The coax session running in the pane's terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<Session>,

    /// The restore block of the program in the pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore: Option<Restore>,

    /// Each terminal tab decade started in the pane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tabs: Vec<SavedTerminal>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct SavedTerminal {
    cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<Session>,

    /// The restore block of the program running in the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore: Option<Restore>,
}

/// A coax session running in a terminal, to start again on restore.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Session {
    /// The name of the agent coax runs.
    pub agent: String,
    pub id: String,

    /// The flags coax was started with, without `--session`.
    pub args: Vec<String>,

    /// The directory coax runs in.
    pub cwd: PathBuf,
}

pub fn path() -> Option<PathBuf> {
    let state = files::xdg_home("XDG_STATE_HOME", ".local/state")?;
    Some(state.join(files::DIR).join("workspaces.toml"))
}

/// The saved file, or `None` when there is none.
pub fn load(path: &Path) -> Result<Option<File>, String> {
    files::read(path)?.map(|text| read(&text)).transpose()
}

fn read(text: &str) -> Result<File, String> {
    let error = |error: toml::de::Error| error.to_string();
    match files::version(text)? {
        1 => {
            let old: FileV1 = toml::from_str(text).map_err(error)?;
            let workspaces = old
                .workspaces
                .into_iter()
                .map(|old| Saved {
                    name: old.name,
                    cwd: old.cwd,
                    active_tab: 0,
                    scratch: Vec::new(),
                    group: None,
                    sleeping: false,
                    tabs: vec![old.tab],
                })
                .collect();
            Ok(File {
                version: VERSION,
                sidebar_open: true,
                groups: Vec::new(),
                workspaces,
            })
        }
        // version 3 adds only the optional tab name, version 4 only
        // the optional groups, version 5 only the sleeping flag, and
        // version 8 only the optional restore blocks
        old @ 2..=7 => {
            let mut file: toml::Table = toml::from_str(text).map_err(error)?;
            if old <= 5 {
                terminal_tabs_as_tables(&mut file);
            }
            if old <= 6 {
                scratch_as_list(&mut file);
            }
            let file: File = file.try_into().map_err(error)?;
            Ok(File {
                version: VERSION,
                ..file
            })
        }
        VERSION => toml::from_str(text).map_err(error),
        other => Err(format!("version {other} is not one this decade reads")),
    }
}

/// Rewrite each pane's `tabs` from a list of directories, as versions 5
/// and before hold them, to a list of tables with a `cwd`.
fn terminal_tabs_as_tables(file: &mut toml::Table) {
    for workspace in tables(file.get_mut("workspace")) {
        for tab in tables(workspace.get_mut("tab")) {
            for pane in tables(tab.get_mut("pane")) {
                let Some(toml::Value::Array(tabs)) = pane.get_mut("tabs") else {
                    continue;
                };
                for cwd in tabs {
                    let table = toml::Table::from_iter([("cwd".to_string(), cwd.clone())]);
                    *cwd = toml::Value::Table(table);
                }
            }
        }
    }
}

/// Rewrite each workspace's `scratch` from one pane name, as versions 6
/// and before hold it, to a list of names.
fn scratch_as_list(file: &mut toml::Table) {
    for workspace in tables(file.get_mut("workspace")) {
        if let Some(scratch) = workspace.get_mut("scratch") {
            *scratch = toml::Value::Array(vec![scratch.clone()]);
        }
    }
}

/// The tables in `value`, an array of tables.
fn tables(value: Option<&mut toml::Value>) -> impl Iterator<Item = &mut toml::Table> {
    (value.and_then(toml::Value::as_array_mut).into_iter())
        .flatten()
        .filter_map(toml::Value::as_table_mut)
}

impl File {
    /// The saved groups, with ids from 0 in the order they are listed.
    pub fn groups(&self) -> Vec<Group> {
        (self.groups.iter().zip(0..))
            .map(|(saved, id)| Group {
                id,
                name: saved.name.clone(),
                folded: saved.folded,
            })
            .collect()
    }
}

pub fn save(
    path: &Path,
    workspaces: &[Workspace],
    groups: &[Group],
    sidebar_open: bool,
) -> io::Result<()> {
    let saved = workspaces
        .iter()
        .map(|workspace| saved(workspace, groups))
        .collect();
    let text = to_text(saved, groups, sidebar_open).map_err(io::Error::other)?;
    files::write(path, &text)
}

fn to_text(
    workspaces: Vec<Saved>,
    groups: &[Group],
    sidebar_open: bool,
) -> Result<String, toml::ser::Error> {
    let groups = (groups.iter())
        .map(|group| SavedGroup {
            name: group.name.clone(),
            folded: group.folded,
        })
        .collect();
    toml::to_string(&File {
        version: VERSION,
        sidebar_open,
        groups,
        workspaces,
    })
}

/// The workspace as the file holds it, with the cwd of each terminal read
/// from its shell.
pub fn saved(workspace: &Workspace, groups: &[Group]) -> Saved {
    let group = (groups.iter())
        .find(|group| Some(group.id) == workspace.group)
        .map(|group| group.name.clone());
    let asleep = workspace.asleep.as_deref().unwrap_or_default();
    Saved {
        name: workspace.name.clone(),
        cwd: workspace.cwd.clone(),
        active_tab: workspace.active_tab,
        scratch: (workspace.scratch_panes().into_iter())
            .map(str::to_string)
            .collect(),
        group,
        sleeping: workspace.asleep.is_some(),
        tabs: (workspace.tabs.iter().enumerate())
            .map(|(index, tab)| SavedTab {
                name: tab.name.clone(),
                ..saved_tab(tab, index, asleep, &workspace.router)
            })
            .collect(),
    }
}

/// Tab `index` as the file holds it, with the terminal tabs `asleep`
/// keeps for it to start on wake, the sessions of the router's agents
/// running in its terminals, and the restore blocks of its clients.
fn saved_tab(tab: &Tab, index: usize, asleep: &[TerminalTab], router: &Router) -> SavedTab {
    let agents = &router.agents;
    let layout = &tab.layout;
    let name_of = |id: usize| {
        layout
            .pane(id)
            .map(|pane| pane.name.clone())
            .unwrap_or_default()
    };
    let panes = layout
        .panes
        .iter()
        .map(|pane| {
            let info = tab.info(pane.id);
            let mut cwd = info
                .cwd
                .as_ref()
                .map(|cwd| info.pid.and_then(shell_cwd).unwrap_or_else(|| cwd.clone()));

            // a session not yet started again is kept until its
            // terminal starts, which is while the workspace sleeps
            let mut session = (info.pid)
                .and_then(|ghostty| session_in(agents, ghostty))
                .or_else(|| info.resume.clone());
            let occupant = router.occupant(&pane.name);
            let block = (occupant.and_then(|(_, client)| client.restore.clone()))
                .or_else(|| info.restore.clone());
            let peer = occupant.and_then(|(id, _)| router.peers.get(&id).copied());
            let runs_under = |ghostty: u32| {
                block.as_ref().is_some_and(|block| block.terminal)
                    && peer.is_some_and(|peer| descends_from(peer, ghostty))
            };
            let stored = (asleep.iter())
                .filter(|stored| stored.tab == index && stored.pane == pane.id)
                .map(|stored| SavedTerminal {
                    cwd: stored.cwd.clone(),
                    session: stored.session.clone(),
                    restore: stored.restore.clone(),
                });
            let mut tabs: Vec<SavedTerminal> = tab
                .clients_in(pane)
                .filter(|client| client.terminal_tab)
                .filter_map(|client| {
                    let cwd = client
                        .pid
                        .and_then(shell_cwd)
                        .or_else(|| info.cwd.clone())?;
                    let session = client.pid.and_then(|ghostty| session_in(agents, ghostty));
                    let restore = block.clone().filter(|_| client.pid.is_some_and(runs_under));
                    Some(SavedTerminal {
                        cwd,
                        session,
                        restore,
                    })
                })
                .chain(stored)
                .collect();
            let mut restore = block.filter(|_| tabs.iter().all(|tab| tab.restore.is_none()));

            // with the pane's own terminal gone, its first tab takes its
            // place, so a restore starts as many terminals as are running
            if cwd.is_some() && !tabs.is_empty() && tabs.len() == pane.tabs.len() {
                let first = tabs.remove(0);
                cwd = Some(first.cwd);
                session = first.session;
                restore = first.restore.or(restore);
            }
            SavedPane {
                name: pane.name.clone(),
                pinned: info.pinned,
                app_id: info
                    .app_id
                    .clone()
                    .filter(|app_id| cwd.is_none() && !app_id.is_empty()),
                session,
                restore,
                cwd,
                tabs,
            }
        })
        .collect();
    SavedTab {
        name: None,
        focused: name_of(layout.focused),
        zoom: layout.zoom.map(name_of),
        tree: saved_node(layout.root(), &name_of),
        panes,
    }
}

fn saved_node(node: &Node, name_of: &impl Fn(usize) -> String) -> SavedNode {
    match node {
        Node::Leaf(id) => SavedNode::Leaf { pane: name_of(*id) },
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => SavedNode::Split {
            axis: match axis {
                Axis::Horizontal => SavedAxis::Horizontal,
                Axis::Vertical => SavedAxis::Vertical,
            },
            ratio: *ratio,
            first: Box::new(saved_node(first, name_of)),
            second: Box::new(saved_node(second, name_of)),
        },
    }
}

/// The cwd of the shell running under a ghostty.
fn shell_cwd(ghostty: u32) -> Option<PathBuf> {
    proc::first_child(ghostty).and_then(proc::cwd)
}

/// The session of the connected agent among `agents` that runs under the
/// ghostty `ghostty`, when coax was started on a named agent and so can
/// be started on the session again.
fn session_in(agents: &Agents, ghostty: u32) -> Option<Session> {
    agents.list.iter().find_map(|agent| {
        let pid = agent.pid.filter(|_| agent.connected)?;
        if !agent.args.iter().any(|arg| arg == "--agent") || !descends_from(pid, ghostty) {
            return None;
        }
        Some(Session {
            agent: agent.name.clone(),
            id: agent.session.clone(),
            args: agent.args.clone(),
            cwd: proc::cwd(pid)?,
        })
    })
}

/// A terminal tab to start in a restored pane.
#[derive(Debug, PartialEq)]
pub struct TerminalTab {
    pub tab: usize,
    pub pane: usize,
    pub cwd: PathBuf,
    pub session: Option<Session>,
    pub restore: Option<Restore>,
}

impl Saved {
    /// Rebuild the workspace with the id `id`, in its group among `groups`,
    /// and list the terminal tabs to start in its panes. A sleeping
    /// workspace keeps its terminal tabs for its wake and lists none.
    pub fn workspace(
        &self,
        id: u32,
        area: CellRect,
        groups: &[Group],
    ) -> Result<(Workspace, Vec<TerminalTab>), String> {
        let tabs = self.tabs(area)?;
        let mut restored = Workspace::with_tabs(
            id,
            self.name.clone(),
            self.cwd.clone(),
            tabs,
            self.active_tab,
        );
        for (tab, saved) in restored.tabs.iter_mut().zip(&self.tabs) {
            tab.name = saved.name.clone();
        }
        restored.group = (groups.iter())
            .find(|group| Some(&group.name) == self.group.as_ref())
            .map(|group| group.id);
        for name in &self.scratch {
            if restored.by_name(name).is_some() {
                restored.toggle_scratch(name);
            }
        }
        let mut tabs = Vec::new();
        for (tab, saved) in self.tabs.iter().enumerate() {
            for (pane, saved) in saved.panes.iter().enumerate() {
                for terminal in &saved.tabs {
                    tabs.push(TerminalTab {
                        tab,
                        pane,
                        cwd: terminal.cwd.clone(),
                        session: terminal.session.clone(),
                        restore: terminal.restore.clone(),
                    });
                }
            }
        }
        if self.sleeping {
            restored.asleep = Some(tabs);
            restored.mark_panes();
            return Ok((restored, Vec::new()));
        }
        Ok((restored, tabs))
    }

    /// Rebuild each tab.
    fn tabs(&self, area: CellRect) -> Result<Vec<Tab>, String> {
        if self.tabs.is_empty() {
            return Err(format!("workspace {} has no tab", self.name));
        }
        let mut names = HashSet::new();
        let repeats = self
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .any(|pane| !names.insert(pane.name.as_str()));
        if repeats {
            return Err(format!("workspace {} repeats a pane name", self.name));
        }
        self.tabs
            .iter()
            .map(|tab| tab.tab(area, &self.name))
            .collect()
    }
}

impl SavedTab {
    fn tab(&self, area: CellRect, workspace: &str) -> Result<Tab, String> {
        let ids: HashMap<&str, usize> = self
            .panes
            .iter()
            .enumerate()
            .map(|(id, pane)| (pane.name.as_str(), id))
            .collect();
        let id_of = |name: &str| {
            ids.get(name)
                .copied()
                .ok_or_else(|| format!("workspace {workspace} has no pane named {name}"))
        };
        let mut used = vec![false; self.panes.len()];
        let root = node(&self.tree, &id_of, &mut used)?;
        if used.contains(&false) {
            return Err(format!("workspace {workspace} has a pane outside its tree"));
        }
        let panes = (self.panes.iter().enumerate())
            .map(|(id, saved)| Pane::new(id, saved.name.clone()))
            .collect();
        let focused = id_of(&self.focused)?;
        let zoom = self.zoom.as_deref().map(id_of).transpose()?;
        let mut tab = Tab::new(Layout::restore(area, panes, root, focused, zoom));
        for (id, saved) in self.panes.iter().enumerate() {
            let info = tab.info_mut(id);

            // a pinned terminal waits for its ghostty by name, and a
            // pinned pane with neither a cwd nor an app id has
            // nothing to wait for
            info.pinned = saved.pinned && (saved.cwd.is_some() || saved.app_id.is_some());
            info.cwd = saved.cwd.clone();
            info.resume = saved.session.clone();
            info.restore = saved.restore.clone();
            info.app_id = saved.app_id.clone();
        }
        Ok(tab)
    }
}

fn node(
    saved: &SavedNode,
    id_of: &impl Fn(&str) -> Result<usize, String>,
    used: &mut [bool],
) -> Result<Node, String> {
    Ok(match saved {
        SavedNode::Leaf { pane } => {
            let id = id_of(pane)?;
            if std::mem::replace(&mut used[id], true) {
                return Err(format!("pane {pane} is in the tree twice"));
            }
            Node::Leaf(id)
        }
        SavedNode::Split {
            axis,
            ratio,
            first,
            second,
        } => Node::Split {
            axis: match axis {
                SavedAxis::Horizontal => Axis::Horizontal,
                SavedAxis::Vertical => Axis::Vertical,
            },
            ratio: ratio.clamp(0.05, 0.95),
            first: Box::new(node(first, id_of, used)?),
            second: Box::new(node(second, id_of, used)?),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(
            2,
            PathBuf::from("/home/jack/projects/decade"),
            crate::server::workspace::tests::AREA,
        );
        let tab = workspace.tab_mut();
        let code = tab.add_pane(tab.layout.focused, "code".to_string());
        tab.info_mut(code).cwd = Some(PathBuf::from("/tmp"));
        let files = tab.add_pane(tab.layout.focused, "nautilus".to_string());
        tab.pin(files, "org.gnome.Nautilus".to_string());
        tab.layout.focused = code;
        workspace
    }

    fn terminal(cwd: PathBuf) -> SavedTerminal {
        SavedTerminal {
            cwd,
            session: None,
            restore: None,
        }
    }

    fn session(id: &str) -> Session {
        Session {
            agent: "claude".to_string(),
            id: id.to_string(),
            args: ["--agent", "claude", "--allow"]
                .map(str::to_string)
                .to_vec(),
            cwd: PathBuf::from("/home/jack/projects/coax"),
        }
    }

    /// The workspace's record with a session on the main terminal of
    /// pane `code` and on the first of its two terminal tabs.
    fn with_sessions(sleeping: bool) -> Saved {
        let mut saved = saved(&workspace(), &[]);
        let code = &mut saved.tabs[0].panes[1];
        code.session = Some(session("main"));
        code.tabs = vec![
            SavedTerminal {
                cwd: PathBuf::from("/tmp"),
                session: Some(session("tab")),
                restore: None,
            },
            terminal(PathBuf::from("/home/jack")),
        ];
        saved.sleeping = sleeping;
        saved
    }

    fn first_tab(saved: &Saved) -> Tab {
        saved
            .tabs(crate::server::workspace::tests::AREA)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn a_saved_workspace_reads_back_with_the_same_tree_and_panes() {
        let workspace = workspace();
        let text = to_text(vec![saved(&workspace, &[])], &[], true).unwrap();
        let file = read(&text).unwrap();
        let back = &file.workspaces[0];
        assert_eq!(back, &saved(&workspace, &[]));
        let tab = first_tab(back);
        let layout = &tab.layout;
        let rects: Vec<_> = layout.panes.iter().map(|pane| pane.rect).collect();
        let before: Vec<_> = workspace
            .layout()
            .panes
            .iter()
            .map(|pane| pane.rect)
            .collect();
        assert_eq!(rects, before);
        assert_eq!(layout.pane(layout.focused).unwrap().name, "code");
        assert_eq!(
            tab.info(0).cwd.as_deref(),
            Some(Path::new("/home/jack/projects/decade"))
        );
    }

    #[test]
    fn a_pinned_gui_pane_waits_for_its_app_and_a_pinned_terminal_for_its_new_class() {
        let mut workspace = workspace();
        workspace.tab_mut().pin(0, "decade.w2.terminal".to_string());
        let tab = first_tab(&saved(&workspace, &[]));
        let pinned: Vec<bool> = (tab.layout.panes.iter())
            .map(|pane| tab.info(pane.id).pinned)
            .collect();
        assert_eq!(pinned, [true, false, true]);
        assert_eq!(
            tab.place(Some("decade.w0.terminal"), "decade.w0."),
            Some((0, false))
        );
        assert_eq!(
            tab.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some((2, false))
        );
        assert_eq!(tab.place(Some("decade.w2.terminal"), "decade.w0."), None);
    }

    #[test]
    fn an_empty_gui_pane_is_filled_again_by_a_window_with_its_app_id() {
        let mut workspace = workspace();
        let tab = workspace.tab_mut();
        tab.unpin(2);
        tab.info_mut(2).app_id = Some("org.gnome.Nautilus".to_string());
        let tab = first_tab(&saved(&workspace, &[]));
        assert_eq!(
            tab.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some((2, false))
        );
    }

    #[test]
    fn a_tree_naming_a_missing_pane_is_refused() {
        let mut saved = saved(&workspace(), &[]);
        saved.tabs[0].panes.pop();
        assert!(saved.tabs(crate::server::workspace::tests::AREA).is_err());
    }

    #[test]
    fn two_tabs_read_back_with_their_trees_and_the_active_tab() {
        let mut workspace = workspace();
        let (second, _) = workspace.add_tab(crate::server::workspace::tests::AREA);
        let tab = &mut workspace.tabs[second];
        tab.add_pane(tab.layout.focused, "logs".to_string());
        workspace.active_tab = second;
        let text = to_text(vec![saved(&workspace, &[])], &[], false).unwrap();
        let file = read(&text).unwrap();
        assert!(!file.sidebar_open);
        let back = &file.workspaces[0];
        assert_eq!(back, &saved(&workspace, &[]));
        assert_eq!(back.active_tab, 1);
        let tabs = back.tabs(crate::server::workspace::tests::AREA).unwrap();
        let layouts: Vec<&Layout> = tabs.iter().map(|tab| &tab.layout).collect();
        let names: Vec<Vec<&str>> = layouts
            .iter()
            .map(|layout| layout.panes.iter().map(|pane| pane.name.as_str()).collect())
            .collect();
        assert_eq!(
            names,
            [
                vec!["terminal", "code", "nautilus"],
                vec!["terminal2", "logs"]
            ]
        );
        assert_eq!(layouts[1].pane(layouts[1].focused).unwrap().name, "logs");
    }

    #[test]
    fn the_scratch_panes_read_back_by_name() {
        let mut workspace = workspace();
        workspace.toggle_scratch("nautilus");
        workspace.toggle_scratch("terminal");
        let text = to_text(
            vec![saved(&workspace, &[]), saved(&self::workspace(), &[])],
            &[],
            true,
        )
        .unwrap();
        let file = read(&text).unwrap();
        assert_eq!(file.workspaces[0].scratch, ["nautilus", "terminal"]);
        assert!(file.workspaces[1].scratch.is_empty());
        assert!(!text.contains("scratch = []"));
        let area = crate::server::workspace::tests::AREA;
        let (back, _) = file.workspaces[0].workspace(0, area, &[]).unwrap();
        assert_eq!(back.scratch_panes(), ["nautilus", "terminal"]);
    }

    #[test]
    fn a_version_6_file_reads_its_one_scratch_pane_as_a_list() {
        let mut workspace = workspace();
        workspace.toggle_scratch("nautilus");
        let text = to_text(vec![saved(&workspace, &[])], &[], true).unwrap();
        let old = text
            .replacen("version = 8", "version = 6", 1)
            .replace(r#"scratch = ["nautilus"]"#, r#"scratch = "nautilus""#);
        assert!(old.contains(r#"scratch = "nautilus""#));
        let file = read(&old).unwrap();
        assert_eq!(file.workspaces[0].scratch, ["nautilus"]);
    }

    #[test]
    fn a_version_1_file_reads_as_one_tab_with_the_sidebar_open() {
        let text = r#"
version = 1

[[workspace]]
name = "decade"
cwd = "/home/jack/projects/decade"
focused = "code"

[workspace.tree]
axis = "horizontal"
ratio = 0.5

[workspace.tree.first]
pane = "terminal"

[workspace.tree.second]
pane = "code"

[[workspace.pane]]
name = "terminal"
cwd = "/home/jack/projects/decade"

[[workspace.pane]]
name = "code"
cwd = "/tmp"
"#;
        let file = read(text).unwrap();
        assert!(file.sidebar_open);
        let back = &file.workspaces[0];
        assert_eq!(back.active_tab, 0);
        assert_eq!(back.tabs[0].name, None);
        let tabs = back.tabs(crate::server::workspace::tests::AREA).unwrap();
        let layouts: Vec<&Layout> = tabs.iter().map(|tab| &tab.layout).collect();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].panes.len(), 2);
        assert_eq!(layouts[0].pane(layouts[0].focused).unwrap().name, "code");
    }

    #[test]
    fn a_pane_name_repeated_in_another_tab_is_refused() {
        let mut workspace = workspace();
        workspace.add_tab(crate::server::workspace::tests::AREA);
        workspace.tabs[1].layout.panes[0].name = "code".to_string();
        assert!(
            saved(&workspace, &[])
                .tabs(crate::server::workspace::tests::AREA)
                .is_err()
        );
    }

    #[test]
    fn a_file_of_another_version_is_refused() {
        let dir = std::env::temp_dir().join(format!("decade-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspaces.toml");
        std::fs::write(&path, "version = 9\n").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_directories_of_a_panes_terminal_tabs_read_back_with_the_pane() {
        let mut saved = saved(&workspace(), &[]);
        let tabs = vec![PathBuf::from("/tmp"), PathBuf::from("/home/jack")];
        saved.tabs[0].panes[1].tabs = tabs.iter().cloned().map(terminal).collect();
        let text = to_text(vec![saved], &[], true).unwrap();
        let back = &read(&text).unwrap().workspaces[0];
        let (_, restored) = back
            .workspace(0, crate::server::workspace::tests::AREA, &[])
            .unwrap();
        let expected: Vec<TerminalTab> = tabs
            .into_iter()
            .map(|cwd| TerminalTab {
                tab: 0,
                pane: 1,
                cwd,
                session: None,
                restore: None,
            })
            .collect();
        assert_eq!(restored, expected);
        assert!(!text.contains("tabs = []"));
    }

    #[test]
    fn a_tab_name_reads_back_and_a_version_2_file_reads_with_unnamed_tabs() {
        let mut workspace = workspace();
        workspace.add_tab(crate::server::workspace::tests::AREA);
        workspace.rename_tab(1, "logs").unwrap();
        let text = to_text(vec![saved(&workspace, &[])], &[], true).unwrap();
        assert!(text.starts_with("version = 8"));
        let file = read(&text).unwrap();
        let (back, _) = (file.workspaces[0])
            .workspace(2, crate::server::workspace::tests::AREA, &[])
            .unwrap();
        let names: Vec<_> = back.tabs.iter().map(|tab| tab.name.as_deref()).collect();
        assert_eq!(names, [None, Some("logs")]);

        let old = text.replacen("version = 8", "version = 2", 1);
        let old = old.replace("name = \"logs\"\n", "");
        let file = read(&old).unwrap();
        assert!(file.workspaces[0].tabs.iter().all(|tab| tab.name.is_none()));
    }

    #[test]
    fn groups_read_back_in_order_with_their_members_and_folds() {
        let groups = vec![
            Group {
                id: 3,
                name: "work".to_string(),
                folded: true,
            },
            Group {
                id: 8,
                name: "home".to_string(),
                folded: false,
            },
        ];
        let mut grouped = workspace();
        grouped.group = Some(8);
        let saved = vec![saved(&grouped, &groups), saved(&workspace(), &groups)];
        let text = to_text(saved, &groups, true).unwrap();
        let file = read(&text).unwrap();
        let back = file.groups();
        let names: Vec<_> = back.iter().map(|group| group.name.as_str()).collect();
        assert_eq!(names, ["work", "home"]);
        let folds: Vec<_> = back.iter().map(|group| group.folded).collect();
        assert_eq!(folds, [true, false]);
        let area = crate::server::workspace::tests::AREA;
        let (first, _) = file.workspaces[0].workspace(0, area, &back).unwrap();
        let (second, _) = file.workspaces[1].workspace(1, area, &back).unwrap();
        assert_eq!(first.group, Some(back[1].id));
        assert_eq!(second.group, None);
    }

    #[test]
    fn a_version_3_file_reads_with_no_groups() {
        let text = to_text(vec![saved(&workspace(), &[])], &[], true).unwrap();
        assert!(!text.contains("[[group]]"));
        let old = text.replacen("version = 8", "version = 3", 1);
        let file = read(&old).unwrap();
        assert!(file.groups().is_empty());
        let area = crate::server::workspace::tests::AREA;
        let (back, _) = file.workspaces[0].workspace(0, area, &[]).unwrap();
        assert_eq!(back.group, None);
    }

    fn asleep() -> Workspace {
        let workspace = workspace();
        let mut saved = saved(&workspace, &[]);
        saved.tabs[0].panes[1].tabs = vec![terminal(PathBuf::from("/home/jack"))];
        saved.sleeping = true;
        let (asleep, tabs) = saved
            .workspace(2, crate::server::workspace::tests::AREA, &[])
            .unwrap();
        assert!(tabs.is_empty());
        asleep
    }

    #[test]
    fn a_sleeping_workspace_keeps_its_terminal_tabs_for_its_wake_and_lists_none_to_start() {
        let asleep = asleep();
        let tab = TerminalTab {
            tab: 0,
            pane: 1,
            cwd: PathBuf::from("/home/jack"),
            session: None,
            restore: None,
        };
        assert_eq!(asleep.asleep, Some(vec![tab]));
        let tab = asleep.tab();
        assert!(tab.layout.panes.iter().all(|pane| pane.tabs.is_empty()));
        assert!(tab.info.values().all(|info| info.pid.is_none()));
    }

    #[test]
    fn a_sleeping_workspace_reads_back_asleep_at_version_8() {
        let asleep = asleep();
        let text = to_text(vec![saved(&asleep, &[])], &[], true).unwrap();
        assert!(text.starts_with("version = 8"));
        assert!(text.contains("sleeping = true"));
        let file = read(&text).unwrap();
        assert_eq!(file.workspaces[0], saved(&asleep, &[]));
        let area = crate::server::workspace::tests::AREA;
        let (back, tabs) = file.workspaces[0].workspace(2, area, &[]).unwrap();
        assert!(tabs.is_empty());
        assert_eq!(back.asleep, asleep.asleep);

        let awake = to_text(vec![saved(&workspace(), &[])], &[], true).unwrap();
        assert!(!awake.contains("sleeping"));
    }

    #[test]
    fn a_version_4_file_reads_with_every_workspace_awake() {
        let text = to_text(vec![saved(&workspace(), &[])], &[], true).unwrap();
        let old = text.replacen("version = 8", "version = 4", 1);
        let file = read(&old).unwrap();
        let area = crate::server::workspace::tests::AREA;
        let (back, _) = file.workspaces[0].workspace(0, area, &[]).unwrap();
        assert_eq!(back.asleep, None);
        let names: Vec<_> = back.panes().map(|pane| pane.name.as_str()).collect();
        assert_eq!(names, ["terminal", "code", "nautilus"]);
    }

    #[test]
    fn sleeping_rebuilds_the_tabs_from_the_saved_record_keeping_unpinned_panes_and_the_tree() {
        let mut workspace = workspace();
        let (second, _) = workspace.add_tab(crate::server::workspace::tests::AREA);
        let tab = &mut workspace.tabs[second];
        let logs = tab.add_pane(tab.layout.focused, "logs".to_string());
        tab.info_mut(logs).app_id = Some("org.gnome.Logs".to_string());
        workspace.active_tab = second;
        workspace.toggle_scratch("code");
        workspace.group = Some(4);
        let before: Vec<Vec<(String, CellRect)>> = (workspace.tabs.iter())
            .map(|tab| {
                (tab.layout.panes.iter())
                    .map(|pane| (pane.name.clone(), pane.rect))
                    .collect()
            })
            .collect();
        let record = saved(&workspace, &[]);
        let area = crate::server::workspace::tests::AREA;
        let (restored, tabs) = record.workspace(2, area, &[]).unwrap();
        workspace.sleep(restored, tabs);

        let after: Vec<Vec<(String, CellRect)>> = (workspace.tabs.iter())
            .map(|tab| {
                (tab.layout.panes.iter())
                    .map(|pane| (pane.name.clone(), pane.rect))
                    .collect()
            })
            .collect();
        assert_eq!(after, before);
        assert_eq!(saved(&workspace, &[]).tabs, record.tabs);
        assert_eq!(workspace.active_tab, second);
        assert_eq!(workspace.scratch_panes(), ["code"]);
        assert_eq!(workspace.group, Some(4));
        assert_eq!(workspace.asleep, Some(Vec::new()));
        assert!(workspace.tabs.iter().all(|tab| tab.clients.is_empty()));
        let tab = &workspace.tabs[second];
        let logs = tab.layout.by_name("logs").unwrap();
        assert!(!tab.info(logs).pinned);
        assert!(workspace.keeps_empty(second, tab.layout.pane(logs).unwrap()));
        assert!(!workspace.tab_is_spent(second, |_| false));
    }

    #[test]
    fn sessions_on_a_main_terminal_and_a_terminal_tab_read_back_at_version_8_to_start_again() {
        let text = to_text(vec![with_sessions(false)], &[], true).unwrap();
        assert!(text.starts_with("version = 8"));
        let back = &read(&text).unwrap().workspaces[0];
        assert_eq!(back, &with_sessions(false));
        let area = crate::server::workspace::tests::AREA;
        let (restored, tabs) = back.workspace(0, area, &[]).unwrap();
        assert_eq!(restored.tab().info(1).resume, Some(session("main")));
        assert_eq!(restored.tab().info(0).resume, None);
        let sessions: Vec<_> = tabs.iter().map(|tab| tab.session.clone()).collect();
        assert_eq!(sessions, [Some(session("tab")), None]);
    }

    #[test]
    fn a_sleeping_workspace_keeps_its_sessions_in_the_next_save() {
        let area = crate::server::workspace::tests::AREA;
        let (asleep, _) = with_sessions(true).workspace(0, area, &[]).unwrap();
        assert_eq!(saved(&asleep, &[]), with_sessions(true));
    }

    fn restore(argv: &[&str], terminal: bool) -> Restore {
        Restore {
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            terminal,
        }
    }

    /// The workspace's record with a restore block on pane `nautilus` and
    /// a terminal one on the second terminal tab of pane `code`.
    fn with_restores(sleeping: bool) -> Saved {
        let mut saved = with_sessions(sleeping);
        saved.tabs[0].panes[2].restore = Some(restore(&["decade-notes"], false));
        saved.tabs[0].panes[1].tabs[1].restore = Some(restore(&["htop", "-d", "5"], true));
        saved
    }

    #[test]
    fn restore_blocks_on_a_pane_and_a_terminal_tab_read_back_at_version_8_to_start_again() {
        let text = to_text(vec![with_restores(false)], &[], true).unwrap();
        assert!(text.starts_with("version = 8"));
        let back = &read(&text).unwrap().workspaces[0];
        assert_eq!(back, &with_restores(false));
        let area = crate::server::workspace::tests::AREA;
        let (restored, tabs) = back.workspace(0, area, &[]).unwrap();
        let notes = Some(restore(&["decade-notes"], false));
        assert_eq!(restored.tab().info(2).restore, notes);
        assert_eq!(restored.tab().info(1).restore, None);
        let restores: Vec<_> = tabs.iter().map(|tab| tab.restore.clone()).collect();
        assert_eq!(restores, [None, Some(restore(&["htop", "-d", "5"], true))]);
    }

    #[test]
    fn a_sleeping_workspace_keeps_its_restore_blocks_in_the_next_save() {
        let area = crate::server::workspace::tests::AREA;
        let (asleep, _) = with_restores(true).workspace(0, area, &[]).unwrap();
        assert_eq!(saved(&asleep, &[]), with_restores(true));
    }

    #[test]
    fn a_version_7_file_reads_as_version_8_with_no_restore_blocks() {
        let record = saved(&workspace(), &[]);
        let text = to_text(vec![record], &[], true).unwrap();
        let old = text.replacen("version = 8", "version = 7", 1);
        let file = read(&old).unwrap();
        assert_eq!(file.version, VERSION);
        assert_eq!(file.workspaces[0], saved(&workspace(), &[]));
    }

    #[test]
    fn a_pane_saves_its_occupants_restore_block_and_else_the_one_waiting_for_a_client() {
        let mut workspace = workspace();
        let waiting = restore(&["decade-notes", "--file", "todo.md"], false);
        workspace.tab_mut().info_mut(2).restore = Some(waiting.clone());
        let saved_block =
            |workspace: &Workspace| saved(workspace, &[]).tabs[0].panes[2].restore.clone();
        assert_eq!(saved_block(&workspace), Some(waiting.clone()));
        let running = restore(&["htop"], true);
        workspace.router.clients.insert(
            1,
            crate::server::channel::Client {
                name: "htop".to_string(),
                accepts: Vec::new(),
                pane: Some("nautilus".to_string()),
                shows: Vec::new(),
                session: None,
                restore: Some(running.clone()),
            },
        );
        assert_eq!(saved_block(&workspace), Some(running));
        workspace.router.clients.remove(&1);
        assert_eq!(saved_block(&workspace), Some(waiting));
    }

    #[test]
    fn a_version_5_file_reads_its_terminal_tabs_from_a_list_of_directories() {
        let text = r#"
version = 5
sidebar_open = true

[[workspace]]
name = "decade"
cwd = "/home/jack/projects/decade"
active_tab = 0

[[workspace.tab]]
focused = "terminal"

[workspace.tab.tree]
pane = "terminal"

[[workspace.tab.pane]]
name = "terminal"
cwd = "/home/jack/projects/decade"
tabs = ["/tmp", "/home/jack"]
"#;
        let file = read(text).unwrap();
        let area = crate::server::workspace::tests::AREA;
        let (restored, tabs) = file.workspaces[0].workspace(0, area, &[]).unwrap();
        assert_eq!(restored.tab().info(0).resume, None);
        let expected: Vec<TerminalTab> = ["/tmp", "/home/jack"]
            .into_iter()
            .map(|cwd| TerminalTab {
                tab: 0,
                pane: 0,
                cwd: PathBuf::from(cwd),
                session: None,
                restore: None,
            })
            .collect();
        assert_eq!(tabs, expected);
    }

    #[test]
    fn only_a_connected_agent_started_on_a_named_agent_under_the_terminal_is_saved() {
        let identity = |session: &str, args: &[&str]| decade_channel::Agent {
            name: "claude".to_string(),
            version: "0.4.0".to_string(),
            session: session.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
        };
        let named = ["--agent", "claude"];
        let found = proc::tests::under_shell("(cd / && sleep 5); true", |shell| {
            let coax = proc::first_child(shell)?;
            let mut workspace = workspace();
            workspace.tab_mut().info_mut(1).pid = Some(shell);
            let session_of = |workspace: &Workspace| {
                saved(workspace, &[]).tabs[0].panes[1]
                    .session
                    .clone()
                    .map(|session| (session.id, session.cwd))
            };
            let now = std::time::Instant::now();
            let agents = &mut workspace.router.agents;
            agents.attach(identity("picked", &["--allow"]), None, Some(coax), now);
            agents.attach(identity("gone", &named), None, Some(coax), now);
            agents.detach("gone", now);
            let elsewhere = Some(std::process::id());
            agents.attach(identity("elsewhere", &named), None, elsewhere, now);
            let none = session_of(&workspace);
            let agents = &mut workspace.router.agents;
            agents.attach(identity("named", &named), None, Some(coax), now);
            let found = session_of(&workspace)?;
            (found.1 == Path::new("/")).then_some((none, found.0))
        });
        assert_eq!(found, Some((None, "named".to_string())));
    }
}
