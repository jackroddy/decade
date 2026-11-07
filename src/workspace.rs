use std::path::PathBuf;

use crate::panes::Layout;

pub struct Workspace {
    pub id: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub layout: Layout,
}

impl Workspace {
    pub fn new(id: u32, cwd: PathBuf, columns: u32, rows: u32) -> Workspace {
        let name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        Workspace {
            id,
            name,
            cwd,
            layout: Layout::new(columns, rows),
        }
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
    fn a_workspace_is_named_after_its_directory() {
        let workspace = Workspace::new(0, PathBuf::from("/home/jack/projects/decade"), 100, 41);
        assert_eq!(workspace.name, "decade");
    }

    #[test]
    fn a_workspace_at_the_root_is_named_after_the_path() {
        let workspace = Workspace::new(0, PathBuf::from("/"), 100, 41);
        assert_eq!(workspace.name, "/");
    }

    #[test]
    fn a_new_workspace_holds_one_terminal_pane() {
        let workspace = Workspace::new(0, PathBuf::from("/home/jack"), 100, 41);
        let names: Vec<&str> = workspace
            .layout
            .panes
            .iter()
            .map(|pane| pane.name.as_str())
            .collect();
        assert_eq!(names, ["terminal"]);
    }
}
