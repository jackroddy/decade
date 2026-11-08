use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;

use decade_channel::AgentState;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, Mode as CalloopMode, PostAction};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::channel::{AGENT, Out};
use crate::lines::Lines;
use crate::mcp;
use crate::scratch::{self, Launch};

use super::State;

const NO_SCRATCH: &str =
    "this workspace has no scratch pane; the person marks one from the Ctrl bar";

impl State {
    pub(super) fn listen(&mut self, workspace: usize) {
        let id = self.workspaces[workspace].id;
        let path = self.children.socket_path(id);
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("decade: {}: {error}", path.display());
                return;
            }
        };
        if let Err(error) = listener.set_nonblocking(true) {
            eprintln!("decade: {}: {error}", path.display());
            return;
        }
        let inserted = self.loop_handle.insert_source(
            Generic::new(listener, Interest::READ, CalloopMode::Level),
            move |_, listener, state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => state.channel_connected(stream, id),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            eprintln!("decade: accepting a channel client failed: {error}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        );
        match inserted {
            Ok(token) => self.workspaces[workspace].listener = Some(token),
            Err(error) => eprintln!("decade: {} is not watched: {error}", path.display()),
        }
    }

    fn channel_connected(&mut self, stream: UnixStream, workspace: u32) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(writer) = stream.try_clone() else {
            return;
        };
        if self.workspace_index(workspace).is_none() {
            return;
        }
        let id = self.next_client;
        self.next_client += 1;
        self.sockets.insert(id, (workspace, writer));
        let mut lines = Lines::default();
        let inserted = self.loop_handle.insert_source(
            Generic::new(stream, Interest::READ, CalloopMode::Level),
            move |_, stream, state| {
                let open = lines.read(stream, |line| {
                    if !line.trim().is_empty() {
                        state.channel_line(id, line);
                    }
                });
                if open {
                    return Ok(PostAction::Continue);
                }
                state.channel_disconnected(id);
                Ok(PostAction::Remove)
            },
        );
        if inserted.is_err() {
            self.channel_disconnected(id);
        }
    }

    pub(super) fn mcp_request(&mut self, request: mcp::Request) {
        let Some(workspace) = request.workspace.and_then(|id| self.workspace_index(id)) else {
            let reason = match request.workspace {
                Some(id) => format!("workspace {id} is closed"),
                None => "the MCP address names no workspace".to_string(),
            };
            let _ = request.reply.send(vec![error_line(&reason)]);
            return;
        };
        let line = match request.call {
            mcp::Call::Line(line) => line,
            mcp::Call::ScratchOpen(launch) => {
                let reply = self
                    .scratch_open(workspace, launch)
                    .unwrap_or_else(|reason| error_line(&reason));
                let _ = request.reply.send(vec![reply]);
                return;
            }
            mcp::Call::ScratchSend { verb, payload } => {
                let Some(pane) = self.workspaces[workspace].scratch_pane() else {
                    let _ = request.reply.send(vec![error_line(NO_SCRATCH)]);
                    return;
                };
                let mut message = serde_json::Map::new();
                message.insert("to".to_string(), pane.into());
                message.insert(verb, payload);
                serde_json::Value::Object(message).to_string()
            }
        };
        let mut reply = Vec::new();
        for out in self.workspaces[workspace].channel_line(AGENT, &line) {
            match out {
                Out::Line(to, text) if to == AGENT => reply.push(text),
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Focus(pane) => self.focus_named(workspace, &pane),
                Out::Status { .. } => {}
            }
        }
        let _ = request.reply.send(reply);
    }

    /// Start the program in the workspace's scratch pane, or ask the
    /// program there to close and start it once it has.
    fn scratch_open(&mut self, workspace: usize, launch: Launch) -> Result<String, String> {
        let shown = &self.workspaces[workspace];
        let name = shown.scratch_pane().ok_or(NO_SCRATCH)?;
        scratch::check_program(
            &launch.program,
            &std::env::var_os("PATH").unwrap_or_default(),
        )?;
        let occupants: Vec<ToplevelSurface> = shown
            .by_name(name)
            .and_then(|(tab, id)| shown.tabs[tab].layout.pane(id))
            .map(|pane| {
                pane.clients
                    .iter()
                    .map(|client| client.toplevel.clone())
                    .collect()
            })
            .unwrap_or_default();
        if occupants.is_empty() {
            return self.start_scratch(workspace, launch);
        }
        for occupant in &occupants {
            occupant.send_close();
        }
        let reply = format!(
            "asked the program in pane {name} to close; {} starts there once it has",
            launch.program
        );
        if let Some(scratch) = &mut self.workspaces[workspace].scratch {
            scratch.next = Some(launch);
        }
        Ok(reply)
    }

    pub(super) fn start_scratch(
        &mut self,
        workspace: usize,
        launch: Launch,
    ) -> Result<String, String> {
        let name = self.workspaces[workspace]
            .scratch_pane()
            .ok_or(NO_SCRATCH)?
            .to_string();
        let shown = &self.workspaces[workspace];
        let mut command = if launch.terminal {
            let mut ghostty = self.children.ghostty(shown, &name, &name, &shown.cwd);
            ghostty.arg("-e").arg(&launch.program);
            ghostty
        } else {
            self.children.command(shown, &launch.program, Some(&name))
        };
        command.args(&launch.args);
        let pid = self.children.spawn(command)?;
        if !launch.terminal
            && let Some(scratch) = &mut self.workspaces[workspace].scratch
        {
            scratch.expected = Some(pid);
        }
        let place = if launch.terminal {
            "in a terminal in"
        } else {
            "in"
        };
        Ok(format!("started {} {place} pane {name}", launch.program))
    }

    /// The workspace, tab and id of the empty scratch pane waiting for a
    /// window from a program under `pid`.
    pub(super) fn expecting(&mut self, pid: u32) -> Option<(usize, usize, usize)> {
        for (index, workspace) in self.workspaces.iter_mut().enumerate() {
            let Some(scratch) = &mut workspace.scratch else {
                continue;
            };
            let Some(expected) = scratch.expected else {
                continue;
            };
            if !scratch::descends_from(pid, expected) {
                continue;
            }
            let Some((tab, id)) = workspace.tabs.iter().enumerate().find_map(|(tab, entry)| {
                let id = entry.layout.by_name(&scratch.pane)?;
                Some((tab, id))
            }) else {
                continue;
            };
            if !workspace.tabs[tab].layout.pane(id)?.clients.is_empty() {
                continue;
            }
            scratch.expected = None;
            return Some((index, tab, id));
        }
        None
    }

    fn channel_disconnected(&mut self, id: usize) {
        let Some((workspace, _)) = self.sockets.remove(&id) else {
            return;
        };
        if let Some(index) = self.workspace_index(workspace) {
            let gone = self.workspaces[index].router.clients.remove(&id);
            if gone.is_some_and(|client| client.status.is_some()) {
                self.request_redraw();
            }
        }
    }

    fn channel_line(&mut self, id: usize, line: &str) {
        let Some(workspace) = self
            .sockets
            .get(&id)
            .and_then(|(workspace, _)| self.workspace_index(*workspace))
        else {
            return;
        };
        for out in self.workspaces[workspace].channel_line(id, line) {
            match out {
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Focus(pane) => self.focus_named(workspace, &pane),
                Out::Status {
                    pane,
                    message,
                    raised,
                } => self.agent_status(workspace, pane, message, raised),
            }
        }
    }

    fn agent_status(
        &mut self,
        workspace: usize,
        pane: String,
        message: Option<String>,
        raised: bool,
    ) {
        let body = message.unwrap_or_else(|| format!("the agent in {pane}"));
        let id = self.workspaces[workspace].id;
        let watched = self
            .viewers
            .values()
            .any(|viewer| viewer.welcomed && viewer.focused && viewer.workspace == id);
        if raised && !watched {
            let title = format!("{} needs you", self.workspaces[workspace].name);
            let notify = Command::new("notify-send")
                .args(["--app-name=decade", &title, &body])
                .spawn();
            if let Ok(mut child) = notify {
                std::thread::spawn(move || child.wait());
            }
        }
        self.request_redraw();
    }

    /// Return every agent in the workspace that is done or needs attention
    /// to idle, as the person at the screen.
    pub(super) fn clear_agents(&mut self, workspace: usize) {
        let clients = self.workspaces[workspace].router.clients.values_mut();
        for status in clients.filter_map(|client| client.status.as_mut()) {
            if status.state > AgentState::Working {
                status.state = AgentState::Idle;
            }
        }
        self.request_redraw();
    }

    fn channel_send(&mut self, to: usize, text: &str) {
        let Some((_, stream)) = self.sockets.get(&to) else {
            return;
        };
        let mut stream = stream;
        if stream
            .write_all(text.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .is_err()
        {
            self.channel_disconnected(to);
        }
    }
}

fn error_line(reason: &str) -> String {
    serde_json::json!({ "error": { "reason": reason } }).to_string()
}
