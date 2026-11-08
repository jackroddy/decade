use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::files;
use crate::panes::{Axis, CellRect, Layout, Node, Pane};
use crate::proc;
use crate::workspace::Workspace;

const VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub struct File {
    version: u32,
    pub sidebar_open: bool,
    #[serde(default, rename = "workspace")]
    pub workspaces: Vec<Saved>,
}

/// One workspace as the file holds it.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Saved {
    name: String,
    cwd: PathBuf,
    active_tab: usize,

    /// The name of the scratch pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scratch: Option<String>,
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

    /// The directory of each terminal tab decade started in the pane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tabs: Vec<PathBuf>,
}

pub fn path() -> Option<PathBuf> {
    let state = files::xdg_home("XDG_STATE_HOME", ".local/state")?;
    Some(state.join("decade").join("workspaces.toml"))
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
                    scratch: None,
                    tabs: vec![old.tab],
                })
                .collect();
            Ok(File {
                version: VERSION,
                sidebar_open: true,
                workspaces,
            })
        }
        VERSION => toml::from_str(text).map_err(error),
        other => Err(format!("version {other} is not one this decade reads")),
    }
}

pub fn save(path: &Path, workspaces: &[Workspace], sidebar_open: bool) -> io::Result<()> {
    let text =
        to_text(workspaces.iter().map(saved).collect(), sidebar_open).map_err(io::Error::other)?;
    files::write(path, &text)
}

fn to_text(workspaces: Vec<Saved>, sidebar_open: bool) -> Result<String, toml::ser::Error> {
    toml::to_string(&File {
        version: VERSION,
        sidebar_open,
        workspaces,
    })
}

fn saved(workspace: &Workspace) -> Saved {
    Saved {
        name: workspace.name.clone(),
        cwd: workspace.cwd.clone(),
        active_tab: workspace.active_tab,
        scratch: workspace.scratch_pane().map(str::to_string),
        tabs: workspace
            .tabs
            .iter()
            .map(|tab| saved_tab(&tab.layout))
            .collect(),
    }
}

fn saved_tab(layout: &Layout) -> SavedTab {
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
            let mut cwd = pane
                .cwd
                .as_ref()
                .map(|cwd| pane.pid.and_then(shell_cwd).unwrap_or_else(|| cwd.clone()));
            let mut tabs: Vec<PathBuf> = pane
                .clients
                .iter()
                .filter(|client| client.terminal_tab)
                .filter_map(|client| client.pid.and_then(shell_cwd).or_else(|| pane.cwd.clone()))
                .collect();

            // with the pane's own terminal gone, its first tab takes its
            // place, so a restore starts as many terminals as are running
            if cwd.is_some() && !tabs.is_empty() && tabs.len() == pane.clients.len() {
                cwd = Some(tabs.remove(0));
            }
            SavedPane {
                name: pane.name.clone(),
                pinned: pane.pinned,
                app_id: pane
                    .app_id
                    .clone()
                    .filter(|app_id| cwd.is_none() && !app_id.is_empty()),
                cwd,
                tabs,
            }
        })
        .collect();
    SavedTab {
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

/// A terminal tab to start in a restored pane.
#[derive(Debug, PartialEq)]
pub struct TerminalTab {
    pub tab: usize,
    pub pane: usize,
    pub cwd: PathBuf,
}

impl Saved {
    /// Rebuild the workspace with the id `id`, and list the terminal tabs
    /// to start in its panes.
    pub fn workspace(
        &self,
        id: u32,
        area: CellRect,
    ) -> Result<(Workspace, Vec<TerminalTab>), String> {
        let layouts = self.layouts(area)?;
        let mut restored = Workspace::with_tabs(
            id,
            self.name.clone(),
            self.cwd.clone(),
            layouts,
            self.active_tab,
        );
        if let Some(name) = &self.scratch
            && restored.by_name(name).is_some()
        {
            restored.toggle_scratch(name);
        }
        let mut tabs = Vec::new();
        for (tab, saved) in self.tabs.iter().enumerate() {
            for (pane, saved) in saved.panes.iter().enumerate() {
                for cwd in &saved.tabs {
                    let cwd = cwd.clone();
                    tabs.push(TerminalTab { tab, pane, cwd });
                }
            }
        }
        Ok((restored, tabs))
    }

    /// Rebuild the layout of each tab.
    fn layouts(&self, area: CellRect) -> Result<Vec<Layout>, String> {
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
            .map(|tab| tab.layout(area, &self.name))
            .collect()
    }
}

impl SavedTab {
    fn layout(&self, area: CellRect, workspace: &str) -> Result<Layout, String> {
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
        let panes = self
            .panes
            .iter()
            .enumerate()
            .map(|(id, saved)| {
                let mut pane = Pane::new(id, saved.name.clone());

                // a pinned terminal waits for its ghostty by name, and a
                // pinned pane with neither a cwd nor an app id has
                // nothing to wait for
                pane.pinned = saved.pinned && (saved.cwd.is_some() || saved.app_id.is_some());
                pane.cwd = saved.cwd.clone();
                pane.app_id = saved.app_id.clone();
                pane
            })
            .collect();
        let focused = id_of(&self.focused)?;
        let zoom = self.zoom.as_deref().map(id_of).transpose()?;
        Ok(Layout::restore(area, panes, root, focused, zoom))
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
            crate::panes::tests::AREA,
        );
        let layout = workspace.layout_mut();
        let code = layout.add_transient("code".to_string());
        layout.pane_mut(code).unwrap().cwd = Some(PathBuf::from("/tmp"));
        let files = layout.add_transient("nautilus".to_string());
        layout.pin(files, "org.gnome.Nautilus".to_string());
        layout.focused = code;
        workspace
    }

    fn first_layout(saved: &Saved) -> Layout {
        saved.layouts(crate::panes::tests::AREA).unwrap().remove(0)
    }

    #[test]
    fn a_saved_workspace_reads_back_with_the_same_tree_and_panes() {
        let workspace = workspace();
        let text = to_text(vec![saved(&workspace)], true).unwrap();
        let file = read(&text).unwrap();
        let back = &file.workspaces[0];
        assert_eq!(back, &saved(&workspace));
        let layout = first_layout(back);
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
            layout.pane(0).unwrap().cwd.as_deref(),
            Some(Path::new("/home/jack/projects/decade"))
        );
    }

    #[test]
    fn a_pinned_gui_pane_waits_for_its_app_and_a_pinned_terminal_for_its_new_class() {
        let mut workspace = workspace();
        workspace
            .layout_mut()
            .pin(0, "decade.w2.terminal".to_string());
        let layout = first_layout(&saved(&workspace));
        let pinned: Vec<bool> = layout.panes.iter().map(|pane| pane.pinned).collect();
        assert_eq!(pinned, [true, false, true]);
        assert_eq!(
            layout.place(Some("decade.w0.terminal"), "decade.w0."),
            Some((0, false))
        );
        assert_eq!(
            layout.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some((2, false))
        );
        assert_eq!(layout.place(Some("decade.w2.terminal"), "decade.w0."), None);
    }

    #[test]
    fn an_empty_gui_pane_is_filled_again_by_a_window_with_its_app_id() {
        let mut workspace = workspace();
        let layout = workspace.layout_mut();
        layout.unpin(2);
        layout.pane_mut(2).unwrap().app_id = Some("org.gnome.Nautilus".to_string());
        let layout = first_layout(&saved(&workspace));
        assert_eq!(
            layout.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some((2, false))
        );
    }

    #[test]
    fn a_tree_naming_a_missing_pane_is_refused() {
        let mut saved = saved(&workspace());
        saved.tabs[0].panes.pop();
        assert!(saved.layouts(crate::panes::tests::AREA).is_err());
    }

    #[test]
    fn two_tabs_read_back_with_their_trees_and_the_active_tab() {
        let mut workspace = workspace();
        let (second, _) = workspace.add_tab(crate::panes::tests::AREA);
        workspace.tabs[second]
            .layout
            .add_transient("logs".to_string());
        workspace.active_tab = second;
        let text = to_text(vec![saved(&workspace)], false).unwrap();
        let file = read(&text).unwrap();
        assert!(!file.sidebar_open);
        let back = &file.workspaces[0];
        assert_eq!(back, &saved(&workspace));
        assert_eq!(back.active_tab, 1);
        let layouts = back.layouts(crate::panes::tests::AREA).unwrap();
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
    fn the_scratch_pane_reads_back_by_name() {
        let mut workspace = workspace();
        workspace.toggle_scratch("nautilus");
        let text = to_text(vec![saved(&workspace), saved(&self::workspace())], true).unwrap();
        let file = read(&text).unwrap();
        assert_eq!(file.workspaces[0].scratch.as_deref(), Some("nautilus"));
        assert_eq!(file.workspaces[1].scratch, None);
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
        let layouts = back.layouts(crate::panes::tests::AREA).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].panes.len(), 2);
        assert_eq!(layouts[0].pane(layouts[0].focused).unwrap().name, "code");
    }

    #[test]
    fn a_pane_name_repeated_in_another_tab_is_refused() {
        let mut workspace = workspace();
        workspace.add_tab(crate::panes::tests::AREA);
        workspace.tabs[1].layout.panes[0].name = "code".to_string();
        assert!(
            saved(&workspace)
                .layouts(crate::panes::tests::AREA)
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
        let mut saved = saved(&workspace());
        let tabs = vec![PathBuf::from("/tmp"), PathBuf::from("/home/jack")];
        saved.tabs[0].panes[1].tabs = tabs.clone();
        let text = to_text(vec![saved], true).unwrap();
        let back = &read(&text).unwrap().workspaces[0];
        let (_, restored) = back.workspace(0, crate::panes::tests::AREA).unwrap();
        let expected: Vec<TerminalTab> = tabs
            .into_iter()
            .map(|cwd| TerminalTab {
                tab: 0,
                pane: 1,
                cwd,
            })
            .collect();
        assert_eq!(restored, expected);
        assert!(!text.contains("tabs = []"));
    }
}
