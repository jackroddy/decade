use std::ffi::OsString;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use crate::server::save::Session;
use crate::server::workspace::{Workspace, app_id_prefix};

#[derive(Debug, PartialEq)]
pub struct Placement {
    pub workspace: u32,
    pub pane: Option<String>,
    pub into: Option<String>,
}

/// The programs decade started, and what each one is told about decade.
pub struct Children {
    wayland: OsString,
    runtime: PathBuf,

    /// The MCP server's address, without the workspace it serves.
    mcp: Option<String>,

    /// Each running child, with the id of the workspace it was started for.
    running: Vec<(u32, Child)>,
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

    /// The workspace id, pane name and `DECADE_INTO` pane name a process's
    /// environment holds, when its `DECADE_SOCKET` is one of this server's
    /// sockets.
    pub fn placement(&self, environ: &[u8]) -> Option<Placement> {
        let var = |name: &str| {
            environ
                .split(|&byte| byte == 0)
                .find_map(|entry| entry.strip_prefix(name.as_bytes())?.strip_prefix(b"="))
                .map(|value| String::from_utf8_lossy(value).into_owned())
        };
        let socket = var("DECADE_SOCKET")?;
        let (_, id) = socket.strip_suffix(".sock")?.rsplit_once("-w")?;
        let id = id.parse().ok()?;
        (Path::new(&socket) == self.socket_path(id)).then(|| Placement {
            workspace: id,
            pane: var("DECADE_PANE"),
            into: var("DECADE_INTO"),
        })
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

    /// A ghostty for the pane like `ghostty`'s, in the session's directory,
    /// whose shell starts coax on the session and is left at its prompt
    /// when coax exits.
    pub fn resuming(
        &self,
        workspace: &Workspace,
        pane: &str,
        class: &str,
        session: &Session,
    ) -> Command {
        let mut ghostty = self.ghostty(workspace, pane, class, &session.cwd);
        ghostty.arg(resume_input(session));
        ghostty
    }

    /// Start the command for the workspace, and return its pid. Nothing
    /// starts for a sleeping workspace.
    pub fn spawn(&mut self, workspace: &Workspace, command: Command) -> Result<u32, String> {
        self.start(workspace, command).map(|child| child.id())
    }

    /// Start the command with its stderr on a pipe, and return its pid and
    /// the pipe's nonblocking read end.
    pub fn spawn_with_stderr(
        &mut self,
        workspace: &Workspace,
        mut command: Command,
    ) -> Result<(u32, File), String> {
        command.stderr(Stdio::piped());
        let child = self.start(workspace, command)?;
        let pid = child.id();
        let stderr = File::from(OwnedFd::from(child.stderr.take().expect("a piped stderr")));
        rustix::io::ioctl_fionbio(&stderr, true)
            .map_err(|error| format!("could not read the stderr of pid {pid}: {error}"))?;
        Ok((pid, stderr))
    }

    fn start(&mut self, workspace: &Workspace, mut command: Command) -> Result<&mut Child, String> {
        let program = command.get_program().to_string_lossy().into_owned();
        if workspace.asleep.is_some() {
            return Err(format!(
                "{program} does not start in {}, which is asleep",
                workspace.name
            ));
        }
        let child = command
            .spawn()
            .map_err(|error| format!("could not start {program}: {error}"))?;
        self.running.push((workspace.id, child));
        Ok(&mut self.running.last_mut().unwrap().1)
    }

    /// Whether the child with this pid has not exited.
    pub fn running(&mut self, pid: u32) -> bool {
        self.running
            .iter_mut()
            .find(|(_, child)| child.id() == pid)
            .is_some_and(|(_, child)| matches!(child.try_wait(), Ok(None)))
    }

    /// Wait on every child that has exited, so none is left a zombie, and
    /// return the pid and exit status of each.
    pub fn reap(&mut self) -> Vec<(u32, ExitStatus)> {
        let mut exited = Vec::new();
        self.running
            .retain_mut(|(_, child)| match child.try_wait() {
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
        for (_, child) in &mut self.running {
            let _ = child.kill();
        }
    }

    /// Kill every child started for the workspace with the id `workspace`.
    pub fn kill_workspace(&mut self, workspace: u32) {
        for (_, child) in (self.running.iter_mut()).filter(|(started, _)| *started == workspace) {
            let _ = child.kill();
        }
    }
}

/// The ghostty option that types `coax <args> --session <id>` and a
/// return into the terminal's shell.
fn resume_input(session: &Session) -> String {
    let mut line = "coax".to_string();
    let session_flag = ["--session".to_string(), session.id.clone()];
    for word in session.args.iter().chain(&session_flag) {
        line.push(' ');
        line.push_str(&shell_quoted(word));
    }

    // ghostty reads the value as the body of a Zig string literal
    let mut input = "--input=raw:".to_string();
    for c in line.chars() {
        match c {
            '\\' => input.push_str("\\\\"),
            '"' => input.push_str("\\\""),
            c if c.is_ascii_control() => input.push_str(&format!("\\x{:02x}", c as u8)),
            c => input.push(c),
        }
    }
    input.push_str("\\n");
    input
}

/// `word` as one word to a POSIX shell.
fn shell_quoted(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
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

    fn workspace(id: u32) -> Workspace {
        Workspace::new(
            id,
            PathBuf::from("/tmp"),
            crate::server::workspace::tests::AREA,
        )
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
            crate::server::workspace::tests::AREA,
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
        let pid = children
            .spawn(&workspace(0), Command::new("false"))
            .unwrap();
        let mut sleep = Command::new("sleep");
        sleep.arg("5");
        let sleeper = children.spawn(&workspace(0), sleep).unwrap();
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
        assert_eq!(children.running[0].1.id(), sleeper);
        children.kill_all();
    }

    #[test]
    fn a_program_started_with_its_stderr_on_a_pipe_writes_its_stderr_there() {
        let mut children = Children::new("wayland-9".into(), "/tmp".into(), None);
        let mut shell = Command::new("sh");
        shell.args(["-c", "echo out; echo broken >&2"]);
        shell.stdout(Stdio::null());
        let (_, stderr) = children.spawn_with_stderr(&workspace(0), shell).unwrap();
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
    fn killing_a_workspaces_children_leaves_the_other_workspaces_running() {
        let mut children = Children::new("wayland-9".into(), "/tmp".into(), None);
        let sleeper = |children: &mut Children, id| {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            children.spawn(&workspace(id), sleep).unwrap()
        };
        let killed = [sleeper(&mut children, 1), sleeper(&mut children, 1)];
        let kept = sleeper(&mut children, 2);
        children.kill_workspace(1);
        let mut exited = Vec::new();
        for _ in 0..100 {
            exited.extend(children.reap().into_iter().map(|(pid, _)| pid));
            if exited.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        exited.sort();
        let mut expected = killed.to_vec();
        expected.sort();
        assert_eq!(exited, expected);
        assert!(children.running(kept));
        children.kill_all();
    }

    #[test]
    fn a_resuming_ghostty_starts_in_the_sessions_directory_and_types_coax_on_the_session() {
        let children = Children::new(OsString::from("wayland-9"), PathBuf::from("/run"), None);
        let session = Session {
            agent: "claude".to_string(),
            id: "7f3a".to_string(),
            args: ["--agent", "my agent", "--allow"]
                .map(str::to_string)
                .to_vec(),
            cwd: PathBuf::from("/home/jack/projects/coax"),
        };
        let ghostty = children.resuming(&workspace(1), "chat", "chat", &session);
        assert_eq!(ghostty.get_current_dir(), Some(session.cwd.as_path()));
        assert_eq!(
            ghostty.get_args().last().unwrap(),
            r"--input=raw:coax --agent 'my agent' --allow --session 7f3a\n"
        );

        // the shell reads 'it'\''s\' as it's\, and ghostty reads a
        // doubled backslash as one
        let session = Session {
            args: vec!["--agent".to_string(), "it's\\".to_string()],
            ..session
        };
        assert_eq!(
            resume_input(&session),
            r"--input=raw:coax --agent 'it'\\''s\\' --session 7f3a\n"
        );
    }

    #[test]
    fn nothing_starts_for_a_sleeping_workspace() {
        let mut children = Children::new("wayland-9".into(), "/tmp".into(), None);
        let mut asleep = workspace(1);
        asleep.asleep = Some(Vec::new());
        let ghostty = children.ghostty(&asleep, "terminal", "terminal", Path::new("/tmp"));
        assert!(children.spawn(&asleep, ghostty).is_err());
        assert!(
            children
                .spawn_with_stderr(&asleep, Command::new("true"))
                .is_err()
        );
        assert!(children.running.is_empty());
    }

    #[test]
    fn a_program_outside_a_pane_runs_in_the_workspace_directory_with_no_pane() {
        let children = Children::new(OsString::from("wayland-9"), PathBuf::from("/run"), None);
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack"),
            crate::server::workspace::tests::AREA,
        );
        let command = children.command(&workspace, "nautilus", None);
        assert_eq!(command.get_current_dir(), Some(Path::new("/home/jack")));
        assert_eq!(env(&command, "DECADE_PANE"), None);
        assert_eq!(env(&command, "DECADE_MCP"), None);
    }

    #[test]
    fn an_environment_naming_one_of_this_servers_sockets_gives_its_workspace_and_pane() {
        let children = Children::new("wayland-9".into(), "/run/user/1000".into(), None);
        let pid = std::process::id();
        let environ = format!(
            "HOME=/home/jack\0DECADE_SOCKET=/run/user/1000/decade-{pid}-w7.sock\0DECADE_PANE=terminal3\0"
        );
        assert_eq!(
            children.placement(environ.as_bytes()),
            Some(Placement {
                workspace: 7,
                pane: Some("terminal3".to_string()),
                into: None,
            })
        );
        let environ = format!("DECADE_SOCKET=/run/user/1000/decade-{pid}-w7.sock");
        assert_eq!(
            children.placement(environ.as_bytes()),
            Some(Placement {
                workspace: 7,
                pane: None,
                into: None,
            })
        );
    }

    #[test]
    fn an_environment_naming_decade_into_gives_that_pane_beside_the_launching_pane() {
        let children = Children::new("wayland-9".into(), "/run/user/1000".into(), None);
        let pid = std::process::id();
        let environ = format!(
            "DECADE_SOCKET=/run/user/1000/decade-{pid}-w7.sock\0DECADE_PANE=terminal3\0DECADE_INTO=scratch\0"
        );
        let placement = children.placement(environ.as_bytes()).unwrap();
        assert_eq!(placement.into.as_deref(), Some("scratch"));
        assert_eq!(placement.pane.as_deref(), Some("terminal3"));
    }

    #[test]
    fn an_environment_from_another_server_or_with_no_socket_gives_no_workspace() {
        let children = Children::new("wayland-9".into(), "/run/user/1000".into(), None);
        let pid = std::process::id();
        let other = format!("DECADE_SOCKET=/run/user/1000/decade-{}-w7.sock", pid + 1);
        let elsewhere = format!("DECADE_SOCKET=/tmp/decade-{pid}-w7.sock");
        let unset = "HOME=/home/jack\0DECADE_PANE=terminal3".to_string();
        let empty = "DECADE_SOCKET_OLD=x\0DECADE_SOCKET=".to_string();
        for environ in [other, elsewhere, unset, empty] {
            assert_eq!(children.placement(environ.as_bytes()), None, "{environ:?}");
        }
    }
}
