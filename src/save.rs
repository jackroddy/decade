use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::panes::{Axis, CellRect, Kind, Layout, Node, Pane};
use crate::workspace::Workspace;

const VERSION: u32 = 2;

#[derive(Deserialize)]
struct Version {
    version: u32,
}

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
    pub name: String,
    pub cwd: PathBuf,
    pub active_tab: usize,

    /// The name of the scratch pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratch: Option<String>,
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
}

pub fn path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(state.join("decade").join("workspaces.toml"))
}

/// The saved file, or `None` when there is none.
pub fn load(path: &Path) -> Result<Option<File>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    read(&text).map(Some)
}

fn read(text: &str) -> Result<File, String> {
    let error = |error: toml::de::Error| error.to_string();
    let version: Version = toml::from_str(text).map_err(error)?;
    match version.version {
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
    let file = File {
        version: VERSION,
        sidebar_open,
        workspaces: workspaces.iter().map(saved).collect(),
    };
    let text = toml::to_string(&file).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("toml.partial");
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, path)
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
            let cwd = pane
                .cwd
                .as_ref()
                .map(|cwd| pane.pid.and_then(shell_cwd).unwrap_or_else(|| cwd.clone()));
            let (pinned, app_id) = match &pane.kind {
                Kind::Anchor { app_id } => (true, app_id.clone()),
                Kind::Transient => (false, pane.app_id.clone().unwrap_or_default()),
            };
            SavedPane {
                name: pane.name.clone(),
                pinned,
                app_id: (cwd.is_none() && !app_id.is_empty()).then_some(app_id),
                cwd,
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
    let tasks = std::fs::read_dir(format!("/proc/{ghostty}/task")).ok()?;
    let shell = tasks
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .flat_map(|children| {
            children
                .split_whitespace()
                .filter_map(|pid| pid.parse::<u32>().ok())
                .collect::<Vec<_>>()
        })
        .next()?;
    std::fs::read_link(format!("/proc/{shell}/cwd")).ok()
}

impl Saved {
    /// Rebuild the layout of each tab, with each pinned terminal waiting for
    /// the app id that starts with `prefix`.
    pub fn layouts(&self, area: CellRect, prefix: &str) -> Result<Vec<Layout>, String> {
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
            .map(|tab| tab.layout(area, prefix, &self.name))
            .collect()
    }
}

impl SavedTab {
    fn layout(&self, area: CellRect, prefix: &str, workspace: &str) -> Result<Layout, String> {
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
                let kind = match (&saved.cwd, &saved.app_id, saved.pinned) {
                    (Some(_), _, true) => Kind::Anchor {
                        app_id: format!("{prefix}{}", saved.name),
                    },
                    (None, Some(app_id), true) => Kind::Anchor {
                        app_id: app_id.clone(),
                    },
                    _ => Kind::Transient,
                };
                let mut pane = Pane::new(id, saved.name.clone(), kind);
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
        saved
            .layouts(crate::panes::tests::AREA, "decade.w0.")
            .unwrap()
            .remove(0)
    }

    #[test]
    fn a_saved_workspace_reads_back_with_the_same_tree_and_panes() {
        let workspace = workspace();
        let text = toml::to_string(&File {
            version: VERSION,
            sidebar_open: true,
            workspaces: vec![saved(&workspace)],
        })
        .unwrap();
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
        let app_id = |id: usize| match &layout.pane(id).unwrap().kind {
            Kind::Anchor { app_id } => Some(app_id.clone()),
            Kind::Transient => None,
        };
        assert_eq!(app_id(0).as_deref(), Some("decade.w0.terminal"));
        assert_eq!(app_id(2).as_deref(), Some("org.gnome.Nautilus"));
        assert_eq!(app_id(1), None);
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
            Some(2)
        );
    }

    #[test]
    fn a_tree_naming_a_missing_pane_is_refused() {
        let mut saved = saved(&workspace());
        saved.tabs[0].panes.pop();
        assert!(
            saved
                .layouts(crate::panes::tests::AREA, "decade.w0.")
                .is_err()
        );
    }

    #[test]
    fn two_tabs_read_back_with_their_trees_and_the_active_tab() {
        let mut workspace = workspace();
        let (second, _) = workspace.add_tab(crate::panes::tests::AREA);
        workspace.tabs[second]
            .layout
            .add_transient("logs".to_string());
        workspace.active_tab = second;
        let text = toml::to_string(&File {
            version: VERSION,
            sidebar_open: false,
            workspaces: vec![saved(&workspace)],
        })
        .unwrap();
        let file = read(&text).unwrap();
        assert!(!file.sidebar_open);
        let back = &file.workspaces[0];
        assert_eq!(back, &saved(&workspace));
        assert_eq!(back.active_tab, 1);
        let layouts = back
            .layouts(crate::panes::tests::AREA, "decade.w0.")
            .unwrap();
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
        let text = toml::to_string(&File {
            version: VERSION,
            sidebar_open: true,
            workspaces: vec![saved(&workspace), saved(&self::workspace())],
        })
        .unwrap();
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
        let layouts = back
            .layouts(crate::panes::tests::AREA, "decade.w0.")
            .unwrap();
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
                .layouts(crate::panes::tests::AREA, "decade.w0.")
                .is_err()
        );
    }

    #[test]
    fn the_shell_cwd_is_read_from_the_first_child_of_the_process() {
        let mut parent = std::process::Command::new("sh")
            .args(["-c", "(cd / && sleep 5); true"])
            .current_dir(std::env::temp_dir())
            .spawn()
            .unwrap();
        let mut cwd = None;
        for _ in 0..100 {
            cwd = shell_cwd(parent.id());
            if cwd.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = parent.kill();
        let _ = parent.wait();
        assert_eq!(cwd.as_deref(), Some(Path::new("/")));
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
}
