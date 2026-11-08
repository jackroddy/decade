use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::workspace::Workspace;

/// The programs decade started, and what each one is told about decade.
pub struct Children {
    wayland: OsString,
    runtime: PathBuf,

    /// The MCP server's address, without the workspace it serves.
    mcp: Option<String>,
    running: Vec<Child>,
}

impl Children {
    pub fn new(wayland: OsString, runtime: PathBuf, mcp: Option<String>) -> Children {
        Children {
            wayland,
            runtime,
            mcp,
            running: Vec::new(),
        }
    }

    /// The path of the socket a workspace's channel clients connect to.
    pub fn socket_path(&self, workspace: u32) -> PathBuf {
        let pid = std::process::id();
        self.runtime.join(format!("decade-{pid}-w{workspace}.sock"))
    }

    /// A program started in the workspace's directory, told its workspace
    /// and, for a program that goes in a pane, the pane's name.
    pub fn command(&self, workspace: &Workspace, program: &str, pane: Option<&str>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&workspace.cwd)
            .env("DECADE_WORKSPACE", &workspace.name)
            .env("WAYLAND_DISPLAY", &self.wayland)
            .env("DECADE_SOCKET", self.socket_path(workspace.id))
            .env_remove("DISPLAY");
        if let Some(mcp) = &self.mcp {
            command.env("DECADE_MCP", format!("{mcp}?workspace={}", workspace.id));
        }
        if let Some(pane) = pane {
            command.env("DECADE_PANE", pane);
        }
        command
    }

    /// A ghostty in `cwd` whose window goes in the pane, with the app id
    /// `class` after the workspace's prefix.
    pub fn ghostty(&self, workspace: &Workspace, pane: &str, class: &str, cwd: &Path) -> Command {
        let mut ghostty = self.command(workspace, "ghostty", Some(pane));
        ghostty
            .current_dir(cwd)
            .args(["--window-decoration=none", "--gtk-titlebar=false"])
            .arg(format!("--class={}{class}", workspace.app_id_prefix()));
        ghostty
    }

    /// Start the command, and return its pid.
    pub fn spawn(&mut self, mut command: Command) -> Result<u32, String> {
        let program = command.get_program().to_string_lossy().into_owned();
        let child = command
            .spawn()
            .map_err(|error| format!("could not start {program}: {error}"))?;
        let pid = child.id();
        self.running.push(child);
        Ok(pid)
    }

    /// Whether the child with this pid has not exited.
    pub fn running(&mut self, pid: u32) -> bool {
        self.running
            .iter_mut()
            .find(|child| child.id() == pid)
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    pub fn kill_all(&mut self) {
        for child in &mut self.running {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(command: &Command, name: &str) -> Option<String> {
        command
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
    }

    #[test]
    fn a_ghostty_for_a_pane_is_told_its_workspace_pane_socket_and_mcp_address() {
        let children = Children::new(
            OsString::from("wayland-9"),
            PathBuf::from("/run/user/1000"),
            Some("http://127.0.0.1:4000/mcp".to_string()),
        );
        let workspace = Workspace::new(3, PathBuf::from("/home/jack"), crate::panes::tests::AREA);
        let ghostty = children.ghostty(&workspace, "logs", "logs.t2", Path::new("/tmp"));
        let pid = std::process::id();
        assert_eq!(ghostty.get_program(), "ghostty");
        assert_eq!(ghostty.get_current_dir(), Some(Path::new("/tmp")));
        assert_eq!(env(&ghostty, "DECADE_WORKSPACE").as_deref(), Some("jack"));
        assert_eq!(env(&ghostty, "DECADE_PANE").as_deref(), Some("logs"));
        assert_eq!(
            env(&ghostty, "WAYLAND_DISPLAY").as_deref(),
            Some("wayland-9")
        );
        assert_eq!(
            env(&ghostty, "DECADE_SOCKET"),
            Some(format!("/run/user/1000/decade-{pid}-w3.sock"))
        );
        assert_eq!(
            env(&ghostty, "DECADE_MCP").as_deref(),
            Some("http://127.0.0.1:4000/mcp?workspace=3")
        );
        let args: Vec<_> = ghostty.get_args().collect();
        assert_eq!(args.last().unwrap(), &"--class=decade.w3.logs.t2");
    }

    #[test]
    fn a_program_outside_a_pane_runs_in_the_workspace_directory_with_no_pane() {
        let children = Children::new(OsString::from("wayland-9"), PathBuf::from("/run"), None);
        let workspace = Workspace::new(0, PathBuf::from("/home/jack"), crate::panes::tests::AREA);
        let command = children.command(&workspace, "nautilus", None);
        assert_eq!(command.get_current_dir(), Some(Path::new("/home/jack")));
        assert_eq!(env(&command, "DECADE_PANE"), None);
        assert_eq!(env(&command, "DECADE_MCP"), None);
    }
}
