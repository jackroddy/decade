use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesRenderbuffer;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Physical, Rectangle, Size};

use decade_channel::{Captured, Error, Reply};
use decade_ui::overlay::{Cursor, Dialog, MenuKey};
use serde_json::Value;

use crate::chrome::prompt::{self, Answer};
use crate::png;
use crate::server::mcp;

use super::record;
use crate::server::workspace::PaneInfo;

use super::driving::Played;
use super::record::{Plan, Requester};
use super::{State, popups_over, send_frames_surface_tree, waits_for_frame};

/// How long decade waits for the person to answer before it refuses.
const ASK_FOR: Duration = Duration::from_secs(30);

/// How long after the dialog appears its keys and clicks start to answer
/// it, so a key typed or a click made as it appears does not.
const ARMED_AFTER: Duration = Duration::from_millis(500);

/// How long a screenshot waits for the pane's window to draw the frame it
/// was waiting to draw.
const FRESH_FOR: Duration = Duration::from_millis(200);

/// Who asked for a capture, which decides where its answer goes and the
/// dialog.
pub(super) enum Asker {
    /// The MCP tool `screenshot`.
    Mcp(mcp::Reply),

    /// Socket client `client`, which saves the PNG to `file`. `message` is
    /// its line, for the error reply.
    Save {
        client: usize,
        file: String,
        message: Value,
    },

    /// The MCP tool `record` or the socket's `record`, which records the
    /// pane by `plan`.
    Record { to: Requester, plan: Plan },

    /// The MCP tool `scratch.input`.
    Input(Played),
}

impl Asker {
    /// Why decade turned the asker down when the person said no, or did
    /// not answer for `unanswered`.
    fn refusal(&self, pane: &str, unanswered: Option<Duration>) -> String {
        let noun = match self {
            Asker::Record { .. } => "recording",
            Asker::Input(_) => "input",
            _ => "screenshot",
        };
        match (self, unanswered) {
            (Asker::Input(_), Some(ask_for)) => format!(
                "the person did not answer within {} seconds, so decade played no input in pane {pane}",
                ask_for.as_secs()
            ),
            (Asker::Input(_), None) => format!("the person refused input to pane {pane}"),
            (_, Some(ask_for)) => format!(
                "the person did not answer within {} seconds, so decade took no {noun} of pane {pane}",
                ask_for.as_secs()
            ),
            (_, None) => format!("the person refused a {noun} of pane {pane}"),
        }
    }
}

/// A screenshot waiting for the pane's window to commit a new frame.
pub(super) struct Capturing {
    workspace: u32,
    pane: String,
    surface: WlSurface,
    asker: Asker,
    timer: Option<RegistrationToken>,
}

/// A screenshot waiting on the person's answer to the dialog.
pub(super) struct Asking {
    workspace: u32,
    pane: String,
    agent: String,
    since: Instant,
    asker: Asker,
    timer: Option<RegistrationToken>,

    /// The dialog's selected choice.
    cursor: Cursor,
}

impl Asking {
    pub(super) fn armed(&self, now: Instant) -> bool {
        armed(self.since, now)
    }
}

/// Whether a dialog put up at `since` takes answers at `now`.
fn armed(since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(since) >= ARMED_AFTER
}

impl State {
    /// Answer a screenshot of pane `pane` at once when the pane accepts
    /// captures, or else once the person answers the dialog.
    pub(super) fn screenshot(
        &mut self,
        workspace: usize,
        pane: &str,
        caller: Option<&str>,
        asker: Asker,
    ) {
        match self.screenshot_target(workspace, pane) {
            Ok((name, info)) if info.captures => self.allowed(workspace, name, asker),
            Ok((name, _)) => self.ask(workspace, name, caller, asker),
            Err(reason) => self.refuse(asker, reason),
        }
    }

    /// Capture or start recording pane `pane`, which the person allowed.
    fn allowed(&mut self, workspace: usize, pane: String, asker: Asker) {
        match asker {
            Asker::Record { to, plan } => self.start_recording(workspace, pane, to, plan),
            Asker::Input(played) => self.drive(workspace, &pane, played),
            asker => self.capture_fresh(workspace, pane, asker),
        }
    }

    /// Answer `asker` with the error `reason`.
    pub(super) fn refuse(&mut self, asker: Asker, reason: String) {
        match asker {
            Asker::Record { to, .. } => self.answer_record(to, Err(reason)),
            Asker::Input(played) => {
                let _ = played.reply.send(Err(reason));
            }
            asker => self.answer_asker(asker, Err(reason)),
        }
    }

    /// Send `asker` the PNG, or the reason there is none.
    fn answer_asker(&mut self, asker: Asker, png: Result<Vec<u8>, String>) {
        match asker {
            Asker::Mcp(reply) => {
                let _ = reply.send(png.map(mcp::Answer::Png));
            }
            Asker::Save {
                client, message, ..
            } => {
                let reply = match png {
                    Ok(png) => Reply::Screenshot(Captured {
                        png: png::base64(&png),
                    }),
                    Err(reason) => Reply::Error(Error { reason, message }),
                };
                self.channel_send(client, &serde_json::to_string(&reply).unwrap());
            }
            Asker::Record { .. } | Asker::Input(_) => {
                unreachable!("refuse answers a recording and input")
            }
        }
    }

    /// The pane's own name and what decade keeps about it, or why it
    /// cannot be captured.
    pub(super) fn screenshot_target(
        &self,
        workspace: usize,
        pane: &str,
    ) -> Result<(String, &PaneInfo), String> {
        let shown = &self.workspaces[workspace];
        let (tab, id) = shown
            .by_name(pane)
            .ok_or_else(|| format!("no pane named {pane}"))?;
        if tab != shown.active_tab {
            return Err(format!(
                "pane {pane} is in tab {}, which is not shown",
                tab + 1
            ));
        }
        let pane = shown.tabs[tab]
            .layout
            .pane(id)
            .ok_or_else(|| format!("no pane named {pane}"))?;
        Ok((pane.name.clone(), shown.tabs[tab].info(id)))
    }

    /// Put up the dialog for a screenshot of the pane named `pane`, take
    /// every key and click from the panes and chrome, and point at the
    /// pane.
    pub(super) fn ask(
        &mut self,
        workspace: usize,
        pane: String,
        caller: Option<&str>,
        asker: Asker,
    ) {
        if self.asking.is_some() {
            let reason = "decade is already asking the person about another capture".to_string();
            self.refuse(asker, reason);
            return;
        }
        if let Some(pane) = self.driven_pane() {
            let reason = format!(
                "an agent's input is playing in pane {pane}, and decade asks the person nothing until it ends"
            );
            self.refuse(asker, reason);
            return;
        }
        let id = self.workspaces[workspace].id;
        let agent = self.agent_name(workspace, caller);
        let timer = Timer::from_duration(ASK_FOR);
        let timer = self.loop_handle.insert_source(timer, |_, _, state| {
            if let Some(asking) = &mut state.asking {
                asking.timer = None;
            }
            state.answer_screenshot(None);
            TimeoutAction::Drop
        });
        let armed = Timer::from_duration(ARMED_AFTER);
        let _ = self.loop_handle.insert_source(armed, |_, _, state| {
            state.request_redraw();
            TimeoutAction::Drop
        });
        let now = Instant::now();
        let note = Some(match asker {
            Asker::Record { .. } => "record?".to_string(),
            Asker::Input(_) => "input?".to_string(),
            _ => "screenshot?".to_string(),
        });

        // point_at fails only for an unknown name, and
        // screenshot_target gave the pane's own
        let _ = self.workspaces[workspace].point_at(&pane, note, now);
        if self.overlay.is_some() {
            self.close_overlay();
        }
        self.dismiss_popups();
        self.block_input();
        self.asking = Some(Asking {
            workspace: id,
            pane,
            agent,
            since: now,
            asker,
            timer: timer.ok(),
            cursor: Cursor::default(),
        });
        self.start_pulse();
        self.request_redraw();
    }

    /// Answer the screenshot the dialog asks about with the person's
    /// `answer`, or refuse it for `None`, when its time ran out.
    pub(super) fn answer_screenshot(&mut self, answer: Option<Answer>) {
        let Some(asking) = self.close_dialog() else {
            return;
        };
        let pane = &asking.pane;
        let workspace = self.workspace_index(asking.workspace);
        let refusal = match (workspace, answer) {
            (None, _) => format!("workspace {} is closed", asking.workspace),
            (Some(_), None) => asking.asker.refusal(pane, Some(ASK_FOR)),
            (Some(_), Some(Answer::Deny)) => asking.asker.refusal(pane, None),
            (Some(workspace), Some(answer)) => {
                match (answer, &asking.asker) {
                    (Answer::Always, Asker::Input(_)) => self.accept_input(workspace, pane),
                    (Answer::Always, _) => self.accept_captures(workspace, pane),
                    _ => {}
                }
                self.allowed(workspace, asking.pane, asking.asker);
                return;
            }
        };
        self.refuse(asking.asker, refusal);
    }

    /// Take down the dialog, stop its clock, give keys and clicks back and
    /// stop pointing at its pane, and return what it asked about.
    fn close_dialog(&mut self) -> Option<Asking> {
        let asking = self.asking.take()?;
        if let Some(timer) = asking.timer {
            self.loop_handle.remove(timer);
        }
        self.unblock_input();
        if let Some(index) = self.workspace_index(asking.workspace) {
            self.workspaces[index].end_point_at(&asking.pane);
        }
        self.request_redraw();
        Some(asking)
    }

    /// Take down the dialog when it asks for socket client `client`, which
    /// has gone, turning on no mode.
    pub(super) fn abandon_screenshot(&mut self, client: usize) {
        let asks_for_client = (self.asking.as_ref()).is_some_and(|asking| {
            matches!(
                asking.asker,
                Asker::Save { client: c, .. }
                    | Asker::Record { to: Requester::Socket { client: c, .. }, .. }
                    if c == client
            )
        });
        if asks_for_client {
            self.close_dialog();
        }
    }

    /// Answer every screenshot of workspace `workspace` that is still
    /// pending with the error `reason`.
    pub(super) fn refuse_screenshots(&mut self, workspace: u32, reason: String) {
        if self
            .asking
            .as_ref()
            .is_some_and(|asking| asking.workspace == workspace)
            && let Some(asking) = self.close_dialog()
        {
            self.refuse(asking.asker, reason.clone());
        }
        let (refused, waiting) = std::mem::take(&mut self.capturing)
            .into_iter()
            .partition(|capturing| capturing.workspace == workspace);
        self.capturing = waiting;
        for capturing in refused {
            if let Some(timer) = capturing.timer {
                self.loop_handle.remove(timer);
            }
            self.answer_asker(capturing.asker, Err(reason.clone()));
        }
    }

    /// Capture pane `pane` once its window commits the frame it was
    /// waiting to draw, or at once when it was waiting for none.
    fn capture_fresh(&mut self, workspace: usize, pane: String, asker: Asker) {
        let shown = &self.workspaces[workspace];
        let surface = (shown.by_name(&pane))
            .and_then(|(tab, id)| shown.tabs[tab].toplevel(id))
            .map(|toplevel| toplevel.wl_surface().clone())
            .filter(waits_for_frame);
        let Some(surface) = surface else {
            let png = self.capture(workspace, &pane);
            self.answer_asker(asker, png);
            return;
        };

        // while no viewer is on screen nothing renders, so the
        // window's last frame request is unanswered and its
        // buffer is older than its state
        let time = self.now();
        send_frames_surface_tree(&surface, time);
        for (popup, _) in popups_over(&surface) {
            send_frames_surface_tree(popup.wl_surface(), time);
        }
        let waited = surface.clone();
        let timer = Timer::from_duration(FRESH_FOR);
        let timer = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.finish_captures(&waited, true);
            TimeoutAction::Drop
        });
        self.capturing.push(Capturing {
            workspace: self.workspaces[workspace].id,
            pane,
            surface,
            asker,
            timer: timer.ok(),
        });
    }

    /// Answer the screenshots waiting on `surface`, which has committed or
    /// whose wait ran out.
    pub(super) fn finish_captures(&mut self, surface: &WlSurface, timed_out: bool) {
        let (done, waiting) = std::mem::take(&mut self.capturing)
            .into_iter()
            .partition(|capturing| capturing.surface == *surface);
        self.capturing = waiting;
        for capturing in done {
            if let Some(timer) = capturing.timer.filter(|_| !timed_out) {
                self.loop_handle.remove(timer);
            }
            let result = match self.workspace_index(capturing.workspace) {
                Some(workspace) => self.capture(workspace, &capturing.pane),
                None => Err(format!("workspace {} is closed", capturing.workspace)),
            };
            self.answer_asker(capturing.asker, result);
        }
    }

    fn accept_captures(&mut self, workspace: usize, pane: &str) {
        let shown = &mut self.workspaces[workspace];
        if let Some((tab, id)) = shown.by_name(pane)
            && let Some(info) = shown.tabs[tab].info.get_mut(&id)
        {
            info.captures = true;
        }
        shown.mark_panes();
    }

    /// The name decade gives the agent in workspace `workspace` that runs
    /// in pane `caller`.
    pub(super) fn agent_name(&self, workspace: usize, caller: Option<&str>) -> String {
        (self.workspaces[workspace].router.agents.list.iter())
            .find(|agent| agent.connected && caller.is_some() && agent.pane.as_deref() == caller)
            .map_or_else(|| "An agent".to_string(), |agent| agent.name.clone())
    }

    fn accept_input(&mut self, workspace: usize, pane: &str) {
        let shown = &mut self.workspaces[workspace];
        if let Some((tab, id)) = shown.by_name(pane)
            && let Some(info) = shown.tabs[tab].info.get_mut(&id)
        {
            info.input = true;
        }
        shown.mark_panes();
    }

    /// Turn the accept input mode of pane `id` on screen on or off.
    pub(super) fn toggle_input(&mut self, id: usize) {
        if let Some(info) = self.tab_mut().info.get_mut(&id) {
            info.input = !info.input;
            let current = self.current();
            self.workspaces[current].mark_panes();
            self.request_redraw();
        }
    }

    /// Turn the accept captures mode of pane `id` on screen on or off.
    pub(super) fn toggle_captures(&mut self, id: usize) {
        if let Some(info) = self.tab_mut().info.get_mut(&id) {
            info.captures = !info.captures;
            let current = self.current();
            self.workspaces[current].mark_panes();
            self.request_redraw();
        }
    }

    /// The dialog every viewer shows while a screenshot waits on the
    /// person's answer.
    pub(super) fn prompt(&self) -> Option<Dialog<'static, Answer>> {
        let asking = self.asking.as_ref()?;
        let workspace = (self.workspace_index(asking.workspace))
            .map(|index| self.workspaces[index].name.clone())
            .unwrap_or_default();
        let dialog = match &asking.asker {
            Asker::Mcp(_) => prompt::dialog(&asking.agent, &workspace, &asking.pane),
            Asker::Save { file, .. } => prompt::save_dialog(&asking.agent, &asking.pane, file),
            Asker::Record { plan, .. } => prompt::record_dialog(
                &asking.agent,
                &asking.pane,
                plan.duration.map(record::span).as_deref(),
                &record::span(record::limit(plan.format)),
                &plan.file.display().to_string(),
            ),
            Asker::Input(played) => prompt::input_dialog(
                &asking.agent,
                &asking.pane,
                played.script.steps.len(),
                &record::span(Duration::from_millis(played.ms.div_ceil(100) * 100)),
            ),
        };
        Some(Dialog {
            armed: asking.armed(Instant::now()),
            selected: asking.cursor.selected,
            window: self.window(),
            cell: self.menu_cell(),
            ..dialog
        })
    }

    /// Answer the dialog with what `key` gives, or move its selection.
    pub(super) fn dialog_key(&mut self, key: MenuKey) {
        let (Some(prompt), Some(asking)) = (self.prompt(), self.asking.as_mut()) else {
            return;
        };
        match prompt.key(&mut asking.cursor, key) {
            Some(answer) => self.answer_screenshot(Some(answer)),
            None => self.request_redraw(),
        }
    }

    /// Select the dialog's choice under pixel `x`, `y`.
    pub(super) fn dialog_hover(&mut self, x: i32, y: i32) {
        let (Some(prompt), Some(asking)) = (self.prompt(), self.asking.as_mut()) else {
            return;
        };
        if prompt.hover(&mut asking.cursor, x, y) {
            self.request_redraw();
        }
    }

    /// A PNG of the client area of pane `pane`, drawn as the workspace
    /// would be without decade's menus, bars or pointer hover, flashing
    /// the pane's camera tag.
    fn capture(&mut self, workspace: usize, pane: &str) -> Result<Vec<u8>, String> {
        let (name, rect) = self.client_px(workspace, pane)?;
        let rgba = self.render_plain(workspace)?;
        let png = png_of(&crop(&rgba, self.size.w, rect), rect.size.w, rect.size.h);
        self.workspaces[workspace].flash = Some((name, Instant::now()));
        self.start_pulse();
        self.request_redraw();
        Ok(png)
    }

    /// The pane's own name and its client area in pixels on screen, or why
    /// it cannot be captured.
    pub(super) fn client_px(
        &self,
        workspace: usize,
        pane: &str,
    ) -> Result<(String, Rectangle<i32, Physical>), String> {
        let (name, _) = self.screenshot_target(workspace, pane)?;
        let layout = self.workspaces[workspace].layout();
        let rect = (layout.by_name(&name))
            .and_then(|id| layout.pane(id))
            .map(|pane| self.text.px_rect(pane.client_rect()))
            .ok_or_else(|| format!("no pane named {name}"))?;
        let frame = Rectangle::from_size(self.size);
        let rect = rect
            .intersection(frame)
            .filter(|rect| !rect.is_empty())
            .ok_or_else(|| format!("pane {name} has no room to draw"))?;
        Ok((name, rect))
    }

    /// The workspace rendered off screen without the overlay, the
    /// screenshot dialog, a drag, the Ctrl bar, hover, the marks of a
    /// recording or of an agent's input, as RGBA rows, top first.
    pub(super) fn render_plain(&mut self, workspace: usize) -> Result<Vec<u8>, String> {
        let recordings = std::mem::take(&mut self.recordings);
        let driving = self.driving.take();
        let overlay = self.overlay.take();
        let asking = self.asking.take();
        let sidebar_drag = self.sidebar_drag.take();
        let drag = self.workspaces[workspace].layout_mut().drag.take();
        let bar = std::mem::replace(&mut self.bar, false);
        let flash = self.workspaces[workspace].flash.take();
        let hovers = (
            self.frame_hover.take(),
            self.hover.take(),
            self.tab_hover.take(),
        );
        let rendered = self.render_offscreen(workspace);
        self.recordings = recordings;
        self.driving = driving;
        self.workspaces[workspace].flash = flash;
        self.overlay = overlay;
        self.asking = asking;
        self.sidebar_drag = sidebar_drag;
        self.workspaces[workspace].layout_mut().drag = drag;
        self.bar = bar;
        (self.frame_hover, self.hover, self.tab_hover) = hovers;
        rendered
    }

    fn render_offscreen(&mut self, workspace: usize) -> Result<Vec<u8>, String> {
        // a renderbuffer, as the GPU cannot render into a linear
        // buffer that could be mapped instead
        let size: Size<i32, Buffer> = (self.size.w, self.size.h).into();
        let mut buffer: GlesRenderbuffer = self
            .gpu
            .renderer
            .create_buffer(Fourcc::Abgr8888, size)
            .map_err(|error| format!("making a buffer to capture into failed: {error}"))?;
        self.render(&mut buffer, workspace)?;
        let renderer = &mut self.gpu.renderer;
        let framebuffer = renderer
            .bind(&mut buffer)
            .map_err(|error| format!("binding the capture failed: {error}"))?;
        let mapping = renderer
            .copy_framebuffer(&framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
            .map_err(|error| format!("reading the capture failed: {error}"))?;
        drop(framebuffer);
        let bytes = renderer
            .map_texture(&mapping)
            .map_err(|error| format!("mapping the capture failed: {error}"))?;
        Ok(bytes.to_vec())
    }
}

/// The part `rect` of an RGBA frame `width` pixels wide, as RGBA rows.
pub(super) fn crop(rgba: &[u8], width: i32, rect: Rectangle<i32, Physical>) -> Vec<u8> {
    let stride = width as usize * 4;
    let (left, top) = (rect.loc.x as usize, rect.loc.y as usize);
    let (columns, rows) = (rect.size.w as usize, rect.size.h as usize);
    let mut cropped = Vec::with_capacity(columns * rows * 4);
    for row in rgba.chunks_exact(stride).skip(top).take(rows) {
        cropped.extend_from_slice(&row[left * 4..(left + columns) * 4]);
    }
    cropped
}

/// An RGBA frame `width` by `height` pixels as a PNG.
fn png_of(rgba: &[u8], width: i32, height: i32) -> Vec<u8> {
    let rgb: Vec<u8> = (rgba.as_chunks::<4>().0.iter())
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    png::encode(width as u32, height as u32, &rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_takes_answers_only_once_it_has_been_up_for_half_a_second() {
        let since = Instant::now();
        assert!(!armed(since, since));
        assert!(!armed(since, since + Duration::from_millis(499)));
        assert!(armed(since, since + ARMED_AFTER));
        assert!(!armed(since + Duration::from_secs(1), since));
    }

    #[test]
    fn a_refused_or_unanswered_script_says_no_input_was_played_and_a_capture_says_what_was_not_taken()
     {
        let (reply, _) = tokio::sync::oneshot::channel();
        let input = Asker::Input(Played {
            reply,
            script: crate::server::drive::Script {
                cell: None,
                steps: Vec::new(),
            },
            agent: "coax".to_string(),
            ms: 0,
        });
        assert_eq!(
            input.refusal("scratch", None),
            "the person refused input to pane scratch"
        );
        assert_eq!(
            input.refusal("scratch", Some(ASK_FOR)),
            "the person did not answer within 30 seconds, so decade played no input in pane scratch"
        );
        let (reply, _) = tokio::sync::oneshot::channel();
        let screenshot = Asker::Mcp(reply);
        assert_eq!(
            screenshot.refusal("code", None),
            "the person refused a screenshot of pane code"
        );
        assert_eq!(
            screenshot.refusal("code", Some(ASK_FOR)),
            "the person did not answer within 30 seconds, so decade took no screenshot of pane code"
        );
    }

    #[test]
    fn a_crop_keeps_the_rows_and_columns_of_its_rectangle() {
        // a 3 by 2 frame whose pixels hold their own column and row
        let rgba: Vec<u8> = (0..2u8)
            .flat_map(|row| (0..3u8).flat_map(move |column| [column, row, 9, 255]))
            .collect();
        let rect = Rectangle::new((1, 1).into(), (2, 1).into());
        let cropped = crop(&rgba, 3, rect);
        assert_eq!(cropped, [1, 1, 9, 255, 2, 1, 9, 255]);
        assert_eq!(
            png_of(&cropped, 2, 1),
            png::encode(2, 1, &[1, 1, 9, 2, 1, 9])
        );
    }
}
