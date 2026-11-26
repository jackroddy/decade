use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use decade_channel::Reply;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, Mode as CalloopMode, PostAction};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::lines::{Lines, Queue};
use crate::server::channel::Out;
use crate::server::mcp;
use crate::server::scratch::{self, Launch, Started, Waiting};

use super::State;
use super::record::Requester;
use super::screenshot::Asker;

/// A connection to a socket client.
pub(super) struct Socket {
    pub(super) workspace: u32,
    stream: UnixStream,

    /// The lines the client has not taken yet.
    queue: Queue,

    /// The event source that writes `queue` as the client takes it, while
    /// the queue holds anything.
    pub(super) flushing: Option<RegistrationToken>,
}

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
        let Some(index) = self.workspace_index(workspace) else {
            return;
        };
        let id = self.next_client;
        self.next_client += 1;
        if let Ok(peer) = rustix::net::sockopt::get_socket_peercred(&stream) {
            let pid = peer.pid.as_raw_nonzero().get() as u32;
            self.workspaces[index].router.peers.insert(id, pid);
        }
        self.sockets.insert(
            id,
            Socket {
                workspace,
                stream: writer,
                queue: Queue::default(),
                flushing: None,
            },
        );
        let mut lines = Lines::default();
        let inserted = self.loop_handle.insert_source(
            Generic::new(stream, Interest::READ, CalloopMode::Level),
            move |_, stream, state| {
                let open = lines.read(&**stream, |line| {
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
        let Some(workspace) = self.workspace_index(request.workspace) else {
            let closed = format!("workspace {} is closed", request.workspace);
            let _ = request.reply.send(Err(closed));
            return;
        };
        let caller = request.pane.as_deref();
        match request.call {
            mcp::Call::ScratchOpen { pane, launch } => {
                self.scratch_open(workspace, pane.as_deref(), launch, request.reply)
            }
            mcp::Call::Screenshot { pane } => {
                self.screenshot(workspace, &pane, caller, Asker::Mcp(request.reply))
            }
            mcp::Call::Record(record) => {
                self.record(workspace, &record, caller, Requester::Mcp(request.reply))
            }
            call => {
                let answer = self.mcp_call(workspace, call, caller);
                let _ = request.reply.send(answer.map(mcp::Answer::Text));
            }
        }
    }

    fn mcp_call(
        &mut self,
        workspace: usize,
        call: mcp::Call,
        caller: Option<&str>,
    ) -> Result<String, String> {
        let shown = &self.workspaces[workspace];
        let (to, verb, payload) = match call {
            mcp::Call::Kind {
                verb,
                kind,
                pane,
                payload,
            } => (
                shown.showing(&kind, pane.as_deref())?,
                verb.to_string(),
                payload,
            ),
            mcp::Call::ScratchSend {
                pane,
                verb,
                payload,
            } => {
                let index = shown.find_scratch(pane.as_deref())?;
                (shown.scratch[index].pane.clone(), verb, payload)
            }
            mcp::Call::Point { pane, note } => {
                let shown = &mut self.workspaces[workspace];
                shown.point_at(&pane, note, Instant::now())?;
                self.start_pulse();
                self.request_redraw();
                return Ok(format!("pointing at pane {pane}"));
            }
            mcp::Call::Workspace => return Ok(shown.name.clone()),
            mcp::Call::Agents => {
                let agents = shown.router.agents.listing(Instant::now());
                return Ok(agents.to_string());
            }
            mcp::Call::ScratchStatus { pane } => {
                let index = shown.find_scratch(pane.as_deref())?;
                return Ok(shown.scratch[index].status());
            }
            mcp::Call::ScratchRename { pane, name } => {
                let shown = &mut self.workspaces[workspace];
                let index = shown.find_scratch(pane.as_deref())?;
                let current = shown.scratch[index].pane.clone();
                let (tab, id) = shown.by_name(&current).ok_or("the pane has gone")?;
                shown.rename_pane(tab, id, &name)?;
                self.schedule_save();
                self.request_redraw();
                return Ok(format!("renamed pane {current} to {name}"));
            }
            mcp::Call::Panes => {
                let panes = Reply::Panes(shown.pane_list(caller));
                return Ok(serde_json::to_string(&panes).unwrap());
            }
            mcp::Call::RecordStop { pane } => return self.stop_record(workspace, &pane),
            mcp::Call::ScratchOpen { .. } | mcp::Call::Screenshot { .. } | mcp::Call::Record(_) => {
                unreachable!("mcp_request answers scratch.open, screenshot and record")
            }
        };
        let sent = format!("sent {verb} to pane {to}");
        let (client, line) = shown.agent_send(&to, verb, payload)?;
        self.channel_send(client, &line);
        Ok(sent)
    }

    /// Start the program in the scratch pane `pane` names, or ask the
    /// program there to close and start it once it has, and answer once
    /// the program's start is known.
    fn scratch_open(
        &mut self,
        workspace: usize,
        pane: Option<&str>,
        launch: Launch,
        reply: mcp::Reply,
    ) {
        let (index, launch, wait, occupants) = match self.scratch_launch(workspace, pane, launch) {
            Ok(ready) => ready,
            Err(reason) => {
                let _ = reply.send(Err(reason));
                return;
            }
        };
        self.answer_scratch(workspace, index, None);
        let scratch = &mut self.workspaces[workspace].scratch[index];
        scratch.waiting = Some(Waiting::new(reply, launch.program.clone(), wait));
        self.time_scratch(workspace, index);
        if occupants.is_empty() {
            self.start_scratch(workspace, index, launch);
            return;
        }
        for occupant in &occupants {
            occupant.send_close();
        }
        self.workspaces[workspace].scratch[index].next = Some(launch);
    }

    /// The index of the scratch pane `pane` names, the launch with its
    /// program resolved, the time to wait for it, and the windows in the
    /// scratch pane that must close first.
    fn scratch_launch(
        &self,
        workspace: usize,
        pane: Option<&str>,
        mut launch: Launch,
    ) -> Result<(usize, Launch, Duration, Vec<ToplevelSurface>), String> {
        let shown = &self.workspaces[workspace];
        let index = shown.find_scratch(pane)?;
        let name = &shown.scratch[index].pane;
        let wait = scratch::wait_for(launch.wait)?;
        launch.program = scratch::resolve_program(
            &launch.program,
            &std::env::var_os("PATH").unwrap_or_default(),
            &shown.cwd,
        )?;
        let occupants = shown
            .by_name(name)
            .and_then(|(tab, id)| {
                let entry = &shown.tabs[tab];
                let pane = entry.layout.pane(id)?;
                let toplevels = entry.clients_in(pane).map(|client| client.toplevel.clone());
                Some(toplevels.collect())
            })
            .unwrap_or_default();
        Ok((index, launch, wait, occupants))
    }

    /// Start the program in scratch pane `index`. A call waiting on it hears
    /// at once that a terminal started, and otherwise waits on the program.
    pub(super) fn start_scratch(&mut self, workspace: usize, index: usize, launch: Launch) {
        let started = self.spawn_scratch(workspace, index, &launch);
        let scratch = &mut self.workspaces[workspace].scratch[index];
        scratch.started = (started.as_ref().ok())
            .map(|&pid| Started::new(launch.program.clone(), pid, launch.terminal));
        let name = scratch.pane.clone();
        let pid = match started {
            Ok(pid) if !launch.terminal => pid,
            Ok(_) => {
                let started = format!("started {} in a terminal in pane {name}", launch.program);
                self.answer_scratch(workspace, index, Some(Ok(started)));
                return;
            }
            Err(error) => {
                if scratch.waiting.is_none() {
                    eprintln!("decade: {error}");
                }
                self.answer_scratch(workspace, index, Some(Err(error)));
                return;
            }
        };
        scratch.expected = Some(pid);
        if let Some(waiting) = &mut scratch.waiting {
            waiting.pid = Some(pid);
            self.time_scratch(workspace, index);
        }
    }

    /// Spawn the program for scratch pane `index`, reading its stderr when
    /// it runs as its own window, and return its pid.
    fn spawn_scratch(
        &mut self,
        workspace: usize,
        index: usize,
        launch: &Launch,
    ) -> Result<u32, String> {
        let shown = &self.workspaces[workspace];
        let name = shown.scratch[index].pane.as_str();
        if launch.terminal {
            let mut ghostty = self.children.ghostty(shown, name, name, &shown.cwd);
            ghostty.arg("-e").arg(&launch.program).args(&launch.args);
            return self.children.spawn(shown, ghostty);
        }
        let mut command = self.children.command(shown, &launch.program, Some(name));
        command.args(&launch.args);
        let (pid, stderr) = self.children.spawn_with_stderr(shown, command)?;
        let mut lines = Lines::default();
        let inserted = self.loop_handle.insert_source(
            Generic::new(stderr, Interest::READ, CalloopMode::Level),
            move |_, stderr, state| {
                let open = lines.read(&**stderr, |line| state.scratch_stderr(pid, line));
                if open {
                    return Ok(PostAction::Continue);
                }
                state.scratch_stderr_closed(pid);
                Ok(PostAction::Remove)
            },
        );
        if let Err(error) = inserted {
            eprintln!(
                "decade: could not read the stderr of {}: {error}",
                launch.program
            );
        }
        Ok(pid)
    }

    /// (Re)start the clock on the `scratch.open` waiting on scratch pane
    /// `index`.
    fn time_scratch(&mut self, workspace: usize, index: usize) {
        let id = self.workspaces[workspace].id;
        let scratch = &mut self.workspaces[workspace].scratch[index];
        let pane = scratch.pane.clone();
        let Some(waiting) = &mut scratch.waiting else {
            return;
        };
        if let Some(timer) = waiting.timer.take() {
            self.loop_handle.remove(timer);
        }
        let timer = Timer::from_duration(waiting.wait);

        // the timer finds its scratch pane again by name, since the
        // index moves when another scratch pane is unmarked, and a
        // renamed pane still answers to the name
        let inserted = self.loop_handle.insert_source(timer, move |_, _, state| {
            if let Some(workspace) = state.workspace_index(id)
                && let Ok(index) = state.workspaces[workspace].find_scratch(Some(&pane))
                && let Some(waiting) = &mut state.workspaces[workspace].scratch[index].waiting
            {
                waiting.timer = None;
                state.answer_scratch(workspace, index, None);
            }
            TimeoutAction::Drop
        });
        waiting.timer = inserted.ok();
    }

    /// Answer the `scratch.open` waiting on scratch pane `index` with
    /// `answer`, or else with what is known of its program so far.
    pub(super) fn answer_scratch(
        &mut self,
        workspace: usize,
        index: usize,
        answer: Option<Result<String, String>>,
    ) {
        let scratch = &mut self.workspaces[workspace].scratch[index];
        let Some(waiting) = scratch.waiting.take() else {
            return;
        };
        if let Some(timer) = waiting.timer {
            self.loop_handle.remove(timer);
        }
        let answer =
            answer.unwrap_or_else(|| Ok(waiting.report(&scratch.pane, scratch.started.as_ref())));
        let _ = waiting.reply.send(answer.map(mcp::Answer::Text));
    }

    /// The workspace and scratch pane whose `scratch.open` waits on the
    /// program `pid`.
    fn waiting_on(&self, pid: u32) -> Option<(usize, usize)> {
        (self.workspaces.iter().enumerate()).find_map(|(workspace, shown)| {
            let index = shown.scratch.iter().position(|scratch| {
                (scratch.waiting.as_ref()).is_some_and(|waiting| waiting.pid == Some(pid))
            })?;
            Some((workspace, index))
        })
    }

    /// The workspace and scratch pane whose program last started is `pid`,
    /// and that program.
    fn started_in(&mut self, pid: u32) -> Option<(usize, usize, &mut Started)> {
        let mut workspaces = self.workspaces.iter_mut().enumerate();
        workspaces.find_map(|(workspace, shown)| {
            let mut scratches = shown.scratch.iter_mut().enumerate();
            scratches.find_map(|(index, scratch)| {
                let started = scratch.started_as(pid)?;
                Some((workspace, index, started))
            })
        })
    }

    fn scratch_stderr(&mut self, pid: u32, line: &str) {
        eprintln!("{line}");
        if let Some((_, _, started)) = self.started_in(pid) {
            started.stderr_line(line);
        }
    }

    fn scratch_stderr_closed(&mut self, pid: u32) {
        let Some((workspace, index, started)) = self.started_in(pid) else {
            return;
        };
        started.stderr_open = false;
        let exited = started.exited.is_some();
        if exited && self.waiting_on(pid) == Some((workspace, index)) {
            self.answer_scratch(workspace, index, None);
        }
    }

    /// Note that the child `pid` has exited, and answer a `scratch.open`
    /// waiting on it once its stderr has been read to the end.
    pub(super) fn child_exited(&mut self, pid: u32, status: ExitStatus) {
        let Some((workspace, index, started)) = self.started_in(pid) else {
            return;
        };

        // a record that already has an exit is for an earlier
        // process that had this pid
        if started.exited.is_some() {
            return;
        }
        started.exited = Some(status);
        let stderr_open = started.stderr_open;
        if !stderr_open && self.waiting_on(pid) == Some((workspace, index)) {
            self.answer_scratch(workspace, index, None);
        }
    }

    /// The workspace, tab and id of the empty scratch pane waiting for a
    /// window from a program under `pid`.
    pub(super) fn expecting(&mut self, pid: u32) -> Option<(usize, usize, usize)> {
        for (workspace, shown) in self.workspaces.iter_mut().enumerate() {
            for (index, scratch) in shown.scratch.iter_mut().enumerate() {
                let Some(expected) = scratch.expected else {
                    continue;
                };
                if !scratch::descends_from(pid, expected) {
                    continue;
                }
                let Some((tab, id)) = shown.tabs.iter().enumerate().find_map(|(tab, entry)| {
                    let id = entry.layout.by_name(&scratch.pane)?;
                    Some((tab, id))
                }) else {
                    continue;
                };
                if !shown.tabs[tab].layout.pane(id)?.tabs.is_empty() {
                    continue;
                }
                scratch.expected = None;
                let landed = (scratch.waiting.as_ref())
                    .filter(|waiting| waiting.pid == Some(expected))
                    .map(|waiting| {
                        format!(
                            "started {} in pane {}, and its window is there",
                            waiting.program, scratch.pane
                        )
                    });
                if let Some(landed) = landed {
                    self.answer_scratch(workspace, index, Some(Ok(landed)));
                }
                return Some((workspace, tab, id));
            }
        }
        None
    }

    fn channel_disconnected(&mut self, id: usize) {
        let Some(socket) = self.sockets.remove(&id) else {
            return;
        };
        if let Some(token) = socket.flushing {
            self.loop_handle.remove(token);
        }
        if let Some(index) = self.workspace_index(socket.workspace) {
            self.workspaces[index].router.disconnect(id, Instant::now());
        }
        self.abandon_screenshot(id);
    }

    fn channel_line(&mut self, id: usize, line: &str) {
        let Some(workspace) = self
            .sockets
            .get(&id)
            .and_then(|socket| self.workspace_index(socket.workspace))
        else {
            return;
        };
        for out in self.workspaces[workspace].channel_line(id, line) {
            match out {
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Agent { raised } => self.agent_changed(workspace, raised),
                Out::Screenshot {
                    client,
                    screenshot,
                    message,
                } => {
                    let asker = Asker::Save {
                        client,
                        file: screenshot.file,
                        message,
                    };
                    let caller = screenshot.caller.as_deref();
                    self.screenshot(workspace, &screenshot.pane, caller, asker);
                }
                Out::Record {
                    client,
                    record,
                    message,
                } => {
                    let to = Requester::Socket { client, message };
                    self.record(workspace, &record, record.caller.as_deref(), to);
                }
                Out::RecordStop {
                    client,
                    stop,
                    message,
                } => {
                    if let Err(reason) = self.stop_record(workspace, &stop.pane) {
                        let error = Reply::Error(decade_channel::Error { reason, message });
                        self.channel_send(client, &serde_json::to_string(&error).unwrap());
                    }
                }
            }
        }
    }

    fn agent_changed(&mut self, workspace: usize, raised: Option<String>) {
        let id = self.workspaces[workspace].id;
        let watched = self
            .viewers
            .values()
            .any(|viewer| viewer.welcomed && viewer.focused && viewer.workspace == id);
        if let Some(body) = raised
            && !watched
        {
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
        self.workspaces[workspace]
            .router
            .agents
            .clear(Instant::now());
        self.request_redraw();
    }

    /// Queue the line `text` for client `to` and write what its socket
    /// takes now. The rest goes as the socket takes it.
    pub(super) fn channel_send(&mut self, to: usize, text: &str) {
        let Some(socket) = self.sockets.get_mut(&to) else {
            return;
        };
        socket.queue.push(text);
        if socket.flushing.is_some() {
            return;
        }
        match socket.queue.write(&socket.stream) {
            Ok(true) => {}
            Ok(false) => self.watch_for_room(to),
            Err(_) => self.channel_disconnected(to),
        }
    }

    /// Write the rest of client `id`'s queue each time its socket has
    /// room, until the queue is empty or the socket fails.
    fn watch_for_room(&mut self, id: usize) {
        let Some(stream) = (self.sockets.get(&id)).and_then(|s| s.stream.try_clone().ok()) else {
            self.channel_disconnected(id);
            return;
        };
        let inserted = self.loop_handle.insert_source(
            Generic::new(stream, Interest::WRITE, CalloopMode::Level),
            move |_, _, state| {
                let Some(socket) = state.sockets.get_mut(&id) else {
                    return Ok(PostAction::Remove);
                };
                let written = socket.queue.write(&socket.stream);
                if let Ok(false) = written {
                    return Ok(PostAction::Continue);
                }

                // cleared before channel_disconnected, which
                // would otherwise remove this source from
                // inside its own callback
                socket.flushing = None;
                if written.is_err() {
                    state.channel_disconnected(id);
                }
                Ok(PostAction::Remove)
            },
        );
        match (inserted, self.sockets.get_mut(&id)) {
            (Ok(token), Some(socket)) => socket.flushing = Some(token),
            _ => self.channel_disconnected(id),
        }
    }
}
