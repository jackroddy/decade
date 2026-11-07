use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::panes::{Axis, Kind, Layout, Node, Pane};
use crate::workspace::Workspace;

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default, rename = "workspace")]
    workspaces: Vec<Saved>,
}

/// One workspace as the file holds it.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Saved {
    pub name: String,
    pub cwd: PathBuf,
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

/// The saved workspaces, or `None` when there is no file.
pub fn load(path: &Path) -> Result<Option<Vec<Saved>>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let file: File = toml::from_str(&text).map_err(|error| error.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "version {} is not one this decade reads",
            file.version
        ));
    }
    Ok(Some(file.workspaces))
}

pub fn save(path: &Path, workspaces: &[Workspace]) -> io::Result<()> {
    let file = File {
        version: VERSION,
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
    let layout = &workspace.layout;
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
    Saved {
        name: workspace.name.clone(),
        cwd: workspace.cwd.clone(),
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
    /// Rebuild the layout, with each pinned terminal waiting for the app id
    /// that starts with `prefix`.
    pub fn layout(&self, columns: u32, rows: u32, prefix: &str) -> Result<Layout, String> {
        let ids: HashMap<&str, usize> = self
            .panes
            .iter()
            .enumerate()
            .map(|(id, pane)| (pane.name.as_str(), id))
            .collect();
        if ids.len() != self.panes.len() {
            return Err(format!("workspace {} repeats a pane name", self.name));
        }
        let id_of = |name: &str| {
            ids.get(name)
                .copied()
                .ok_or_else(|| format!("workspace {} has no pane named {name}", self.name))
        };
        let mut used = vec![false; self.panes.len()];
        let root = node(&self.tree, &id_of, &mut used)?;
        if used.contains(&false) {
            return Err(format!(
                "workspace {} has a pane outside its tree",
                self.name
            ));
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
        Ok(Layout::restore(columns, rows, panes, root, focused, zoom))
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
        let mut workspace = Workspace::new(2, PathBuf::from("/home/jack/projects/decade"), 100, 41);
        let layout = &mut workspace.layout;
        let code = layout.add_transient("code".to_string());
        layout.pane_mut(code).unwrap().cwd = Some(PathBuf::from("/tmp"));
        let files = layout.add_transient("nautilus".to_string());
        layout.pin(files, "org.gnome.Nautilus".to_string());
        layout.focused = code;
        workspace
    }

    #[test]
    fn a_saved_workspace_reads_back_with_the_same_tree_and_panes() {
        let workspace = workspace();
        let text = toml::to_string(&File {
            version: VERSION,
            workspaces: vec![saved(&workspace)],
        })
        .unwrap();
        let file: File = toml::from_str(&text).unwrap();
        let back = &file.workspaces[0];
        assert_eq!(back, &saved(&workspace));
        let layout = back.layout(100, 41, "decade.w0.").unwrap();
        let rects: Vec<_> = layout.panes.iter().map(|pane| pane.rect).collect();
        let before: Vec<_> = workspace
            .layout
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
        workspace.layout.pin(0, "decade.w2.terminal".to_string());
        let layout = saved(&workspace).layout(100, 41, "decade.w0.").unwrap();
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
        workspace.layout.unpin(2);
        workspace.layout.pane_mut(2).unwrap().app_id = Some("org.gnome.Nautilus".to_string());
        let layout = saved(&workspace).layout(100, 41, "decade.w0.").unwrap();
        assert_eq!(
            layout.place(Some("org.gnome.Nautilus"), "decade.w0."),
            Some(2)
        );
    }

    #[test]
    fn a_tree_naming_a_missing_pane_is_refused() {
        let mut saved = saved(&workspace());
        saved.panes.pop();
        assert!(saved.layout(100, 41, "decade.w0.").is_err());
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
