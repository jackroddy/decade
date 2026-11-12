use std::ffi::OsString;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use crate::server::workspace::{Workspace, app_id_prefix};

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
            let address = match pane {
                Some(pane) => format!("{mcp}?workspace={}&pane={pane}", workspace.id),
                None => format!("{mcp}?workspace={}", workspace.id),
            };
            command.env("DECADE_MCP", address);
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
            .arg(format!("--class={}{class}", app_id_prefix(workspace.id)));
        ghostty
    }

    /// Start the command, and return its pid.
    pub fn spawn(&mut self, command: Command) -> Result<u32, String> {
        self.start(command).map(|child| child.id())
    }

    /// Start the command with its stderr on a pipe, and return its pid and
    /// the pipe's nonblocking read end.
    pub fn spawn_with_stderr(&mut self, mut command: Command) -> Result<(u32, File), String> {
        command.stderr(Stdio::piped());
        let child = self.start(command)?;
        let pid = child.id();
        let stderr = File::from(OwnedFd::from(child.stderr.take().expect("a piped stderr")));
        rustix::io::ioctl_fionbio(&stderr, true)
            .map_err(|error| format!("could not read the stderr of pid {pid}: {error}"))?;
        Ok((pid, stderr))
    }

    fn start(&mut self, mut command: Command) -> Result<&mut Child, String> {
        let program = command.get_program().to_string_lossy().into_owned();
        let child = command
            .spawn()
            .map_err(|error| format!("could not start {program}: {error}"))?;
        self.running.push(child);
        Ok(self.running.last_mut().unwrap())
    }

    /// Whether the child with this pid has not exited.
    pub fn running(&mut self, pid: u32) -> bool {
        self.running
            .iter_mut()
            .find(|child| child.id() == pid)
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    /// Wait on every child that has exited, so none is left a zombie, and
    /// return the pid and exit status of each.
    pub fn reap(&mut self) -> Vec<(u32, ExitStatus)> {
        let mut exited = Vec::new();
        self.running.retain_mut(|child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                exited.push((child.id(), status));
                false
            }
            Err(_) => false,
        });
        exited
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
        let workspace = Workspace::new(
            3,
            PathBuf::from("/home/jack"),
            crate::server::panes::tests::AREA,
        );
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
            Some("http://127.0.0.1:4000/mcp?workspace=3&pane=logs")
        );
        let args: Vec<_> = ghostty.get_args().collect();
        assert_eq!(args.last().unwrap(), &"--class=decade.w3.logs.t2");
    }

    #[test]
    fn a_reaped_child_that_exited_leaves_no_zombie_and_gives_its_status() {
        let mut children = Children::new("wayland-9".into(), "/tmp".into(), None);
        let pid = children.spawn(Command::new("false")).unwrap();
        let mut sleep = Command::new("sleep");
        sleep.arg("5");
        let sleeper = children.spawn(sleep).unwrap();
        let mut exited = Vec::new();
        for _ in 0..100 {
            exited.extend(children.reap());
            if children.running.len() == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(exited.len(), 1);
        assert_eq!((exited[0].0, exited[0].1.code()), (pid, Some(1)));
        assert_eq!(children.running[0].id(), sleeper);
        children.kill_all();
    }

    #[test]
    fn a_program_started_with_its_stderr_on_a_pipe_writes_its_stderr_there() {
        let mut children = Children::new("wayland-9".into(), "/tmp".into(), None);
        let mut shell = Command::new("sh");
        shell.args(["-c", "echo out; echo broken >&2"]);
        shell.stdout(Stdio::null());
        let (_, stderr) = children.spawn_with_stderr(shell).unwrap();
        let mut seen = Vec::new();
        let mut lines = crate::lines::Lines::default();
        for _ in 0..100 {
            if !lines.read(&stderr, |line| seen.push(line.to_string())) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(seen, ["broken"]);
        children.kill_all();
    }

    #[test]
    fn a_program_outside_a_pane_runs_in_the_workspace_directory_with_no_pane() {
        let children = Children::new(OsString::from("wayland-9"), PathBuf::from("/run"), None);
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack"),
            crate::server::panes::tests::AREA,
        );
        let command = children.command(&workspace, "nautilus", None);
        assert_eq!(command.get_current_dir(), Some(Path::new("/home/jack")));
        assert_eq!(env(&command, "DECADE_PANE"), None);
        assert_eq!(env(&command, "DECADE_MCP"), None);
    }
}
