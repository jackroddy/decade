use std::path::PathBuf;

use smithay::reexports::calloop::RegistrationToken;

use crate::channel::{AGENT, Router};
use crate::panes::Layout;

pub struct Workspace {
    pub id: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub layout: Layout,
    pub router: Router,

    /// The event source accepting connections on this workspace's socket.
    pub listener: Option<RegistrationToken>,

    /// Whether the workspace goes once its last client has.
    pub closing: bool,
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
    pub fn new(id: u32, cwd: PathBuf, columns: u32, rows: u32) -> Workspace {
        let name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        let mut layout = Layout::new(columns, rows);
        layout.panes[0].cwd = Some(cwd.clone());
        Workspace::with_layout(id, name, cwd, layout)
    }

    pub fn with_layout(id: u32, name: String, cwd: PathBuf, layout: Layout) -> Workspace {
        let mut router = Router::new(layout.panes.iter().map(|pane| pane.name.clone()).collect());
        router.connected(AGENT);
        router.handle(AGENT, r#"{"hello":{"name":"agent"}}"#);
        Workspace {
            id,
            name,
            cwd,
            layout,
            router,
            listener: None,
            closing: false,
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
    fn a_taken_name_gets_the_first_free_count() {
        assert_eq!(unique_name("decade", ["foam"].into_iter()), "decade");
        assert_eq!(unique_name("decade", ["decade"].into_iter()), "decade 2");
        let taken = ["decade", "decade 2", "decade 3"];
        assert_eq!(unique_name("decade", taken.into_iter()), "decade 4");
    }

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
