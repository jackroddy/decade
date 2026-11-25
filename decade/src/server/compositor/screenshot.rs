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
use crate::server::workspace::{PaneInfo, SAVED_SCREENSHOTS_ICON, SCREENSHOTS_ICON};

use super::{State, popups_over, send_frames_surface_tree, waits_for_frame};

/// How long decade waits for the person to answer before it refuses.
const ASK_FOR: Duration = Duration::from_secs(30);

/// How long after the dialog appears its keys and clicks start to answer
/// it, so a key typed or a click made as it appears does not.
const ARMED_AFTER: Duration = Duration::from_millis(500);

/// How long a screenshot waits for the pane's window to draw the frame it
/// was waiting to draw.
const FRESH_FOR: Duration = Duration::from_millis(200);

/// Who asked for a screenshot, which decides where its PNG goes, the
/// pane mode that lets it through without asking, and the dialog.
pub(super) enum Asker {
    /// The MCP tool `screenshot`, under the accept screenshots mode.
    Mcp(mcp::Reply),

    /// Socket client `client`, which saves the PNG to `file`, under the
    /// accept saved screenshots mode. `message` is its line, for the error
    /// reply.
    Save {
        client: usize,
        file: String,
        message: Value,
    },
}

impl Asker {
    fn accepted(&self, info: &PaneInfo) -> bool {
        match self {
            Asker::Mcp(_) => info.screenshots,
            Asker::Save { .. } => info.saved_screenshots,
        }
    }

    fn accept(&self, info: &mut PaneInfo) {
        match self {
            Asker::Mcp(_) => info.screenshots = true,
            Asker::Save { .. } => info.saved_screenshots = true,
        }
    }

    /// The left tag of the mode that lets this kind of screenshot through.
    fn tag(&self) -> char {
        match self {
            Asker::Mcp(_) => SCREENSHOTS_ICON,
            Asker::Save { .. } => SAVED_SCREENSHOTS_ICON,
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
    /// Answer a screenshot of pane `pane` at once when the pane's mode for
    /// `asker` is on, or else once the person answers the dialog.
    pub(super) fn screenshot(
        &mut self,
        workspace: usize,
        pane: &str,
        caller: Option<&str>,
        asker: Asker,
    ) {
        match self.screenshot_target(workspace, pane) {
            Ok((name, info)) if asker.accepted(info) => self.capture_fresh(workspace, name, asker),
            Ok((name, _)) => self.ask(workspace, name, caller, asker),
            Err(reason) => self.answer_asker(asker, Err(reason)),
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
        }
    }

    /// The pane's own name and what decade keeps about it, or why it
    /// cannot be captured.
    fn screenshot_target(
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
    fn ask(&mut self, workspace: usize, pane: String, caller: Option<&str>, asker: Asker) {
        if self.asking.is_some() {
            let reason = "decade is already asking the person about another screenshot".to_string();
            self.answer_asker(asker, Err(reason));
            return;
        }
        let shown = &self.workspaces[workspace];
        let id = shown.id;
        let agent = (shown.router.agents.list.iter())
            .find(|agent| agent.connected && caller.is_some() && agent.pane.as_deref() == caller)
            .map_or_else(|| "An agent".to_string(), |agent| agent.name.clone());
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
        let note = Some("screenshot?".to_string());

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
            (Some(_), None) => format!(
                "the person did not answer within {} seconds, so decade took no screenshot of pane {pane}",
                ASK_FOR.as_secs()
            ),
            (Some(_), Some(Answer::Deny)) => {
                format!("the person refused a screenshot of pane {pane}")
            }
            (Some(workspace), Some(answer)) => {
                if answer == Answer::Always {
                    self.accept_screenshots(workspace, pane, &asking.asker);
                }
                self.capture_fresh(workspace, asking.pane, asking.asker);
                return;
            }
        };
        self.answer_asker(asking.asker, Err(refusal));
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
        let asks_for_client = (self.asking.as_ref()).is_some_and(
            |asking| matches!(asking.asker, Asker::Save { client: c, .. } if c == client),
        );
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
            self.answer_asker(asking.asker, Err(reason.clone()));
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
            let png = self.capture(workspace, &pane, asker.tag());
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
                Some(workspace) => self.capture(workspace, &capturing.pane, capturing.asker.tag()),
                None => Err(format!("workspace {} is closed", capturing.workspace)),
            };
            self.answer_asker(capturing.asker, result);
        }
    }

    fn accept_screenshots(&mut self, workspace: usize, pane: &str, asker: &Asker) {
        let shown = &mut self.workspaces[workspace];
        if let Some((tab, id)) = shown.by_name(pane)
            && let Some(info) = shown.tabs[tab].info.get_mut(&id)
        {
            asker.accept(info);
        }
        shown.mark_panes();
    }

    /// Turn the accept screenshots mode of pane `id` on screen on or off.
    pub(super) fn toggle_screenshots(&mut self, id: usize) {
        if let Some(info) = self.tab_mut().info.get_mut(&id) {
            info.screenshots = !info.screenshots;
            let current = self.current();
            self.workspaces[current].mark_panes();
            self.request_redraw();
        }
    }

    /// Turn the accept saved screenshots mode of pane `id` on screen on or
    /// off.
    pub(super) fn toggle_saved_screenshots(&mut self, id: usize) {
        if let Some(info) = self.tab_mut().info.get_mut(&id) {
            info.saved_screenshots = !info.saved_screenshots;
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
    /// the pane's left tag `tag`.
    fn capture(&mut self, workspace: usize, pane: &str, tag: char) -> Result<Vec<u8>, String> {
        let (name, _) = self.screenshot_target(workspace, pane)?;
        let shown = &self.workspaces[workspace];
        let layout = shown.layout();
        let rect = (layout.by_name(&name))
            .and_then(|id| layout.pane(id))
            .map(|pane| self.text.px_rect(pane.client_rect()))
            .ok_or_else(|| format!("no pane named {name}"))?;
        let frame = Rectangle::from_size(self.size);
        let rect = rect
            .intersection(frame)
            .filter(|rect| !rect.is_empty())
            .ok_or_else(|| format!("pane {name} has no room to draw"))?;
        let rgba = self.render_plain(workspace)?;
        let png = crop(&rgba, self.size.w, rect);
        self.workspaces[workspace].flash = Some((name, tag, Instant::now()));
        self.start_pulse();
        self.request_redraw();
        Ok(png)
    }

    /// The workspace rendered off screen without the overlay, the
    /// screenshot dialog, a drag, the Ctrl bar or hover, as RGBA rows, top
    /// first.
    fn render_plain(&mut self, workspace: usize) -> Result<Vec<u8>, String> {
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

/// The part `rect` of an RGBA frame `width` pixels wide, as a PNG.
fn crop(rgba: &[u8], width: i32, rect: Rectangle<i32, Physical>) -> Vec<u8> {
    let stride = width as usize * 4;
    let (left, top) = (rect.loc.x as usize, rect.loc.y as usize);
    let (columns, rows) = (rect.size.w as usize, rect.size.h as usize);
    let mut rgb = Vec::with_capacity(columns * rows * 3);
    for row in rgba.chunks_exact(stride).skip(top).take(rows) {
        let pixels = &row[left * 4..(left + columns) * 4];
        rgb.extend(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]]),
        );
    }
    png::encode(rect.size.w as u32, rect.size.h as u32, &rgb)
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
    fn a_saved_screenshot_goes_by_its_own_mode_and_a_viewed_one_by_the_other() {
        let (reply, _) = tokio::sync::oneshot::channel();
        let mcp = Asker::Mcp(reply);
        let save = Asker::Save {
            client: 1,
            file: "shot.png".to_string(),
            message: Value::Null,
        };
        let mut info = PaneInfo::default();
        save.accept(&mut info);
        assert!(save.accepted(&info));
        assert!(!mcp.accepted(&info));
        let mut info = PaneInfo::default();
        mcp.accept(&mut info);
        assert!(mcp.accepted(&info));
        assert!(!save.accepted(&info));
    }

    #[test]
    fn a_crop_keeps_the_rows_and_columns_of_its_rectangle() {
        // a 3 by 2 frame whose pixels hold their own column and row
        let rgba: Vec<u8> = (0..2u8)
            .flat_map(|row| (0..3u8).flat_map(move |column| [column, row, 9, 255]))
            .collect();
        let rect = Rectangle::new((1, 1).into(), (2, 1).into());
        let png = crop(&rgba, 3, rect);
        assert_eq!(png, png::encode(2, 1, &[1, 1, 9, 2, 1, 9]));
    }
}
