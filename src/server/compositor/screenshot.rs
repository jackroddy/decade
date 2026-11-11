use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesRenderbuffer;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Buffer, Physical, Rectangle, Size};

use crate::png;
use crate::server::chrome::prompt::{Answer, Prompt};
use crate::server::mcp::{self, Reply};

use super::State;

/// How long decade waits for the person to answer before it refuses.
const ASK_FOR: Duration = Duration::from_secs(30);

/// How long after the dialog appears its keys and clicks start to answer
/// it, so a key typed or a click made as it appears does not.
const ARMED_AFTER: Duration = Duration::from_millis(500);

/// A screenshot waiting on the person's answer to the dialog.
pub(super) struct Asking {
    workspace: u32,
    pane: String,
    agent: String,
    since: Instant,
    reply: Reply,
    timer: Option<RegistrationToken>,
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
    /// Answer a `screenshot` of pane `pane` at once when the pane accepts
    /// screenshots, or else once the person answers the dialog.
    pub(super) fn screenshot(
        &mut self,
        workspace: usize,
        pane: &str,
        caller: Option<&str>,
        reply: Reply,
    ) {
        match self.screenshot_target(workspace, pane) {
            Ok((name, true)) => {
                let _ = reply.send(self.capture(workspace, &name).map(mcp::Answer::Png));
            }
            Ok((name, false)) => self.ask(workspace, name, caller, reply),
            Err(reason) => {
                let _ = reply.send(Err(reason));
            }
        }
    }

    /// The pane's own name and whether it accepts screenshots, or why it
    /// cannot be captured.
    fn screenshot_target(&self, workspace: usize, pane: &str) -> Result<(String, bool), String> {
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
        Ok((pane.name.clone(), pane.screenshots))
    }

    /// Put up the dialog for a screenshot of the pane named `pane`, take
    /// every key and click from the panes and chrome, and point at the
    /// pane.
    fn ask(&mut self, workspace: usize, pane: String, caller: Option<&str>, reply: Reply) {
        if self.asking.is_some() {
            let reason = "decade is already asking the person about another screenshot".to_string();
            let _ = reply.send(Err(reason));
            return;
        }
        let shown = &self.workspaces[workspace];
        let id = shown.id;
        let agent = (shown.router.agents.list.iter())
            .find(|agent| agent.connected && caller.is_some() && agent.pane.as_deref() == caller)
            .map_or_else(|| "An agent".to_string(), |agent| agent.name.clone());
        let timer = Timer::from_duration(ASK_FOR);
        let timer = self.loop_handle.insert_source(timer, |_, _, state| {
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
            reply,
            timer: timer.ok(),
        });
        self.start_pulse();
        self.request_redraw();
    }

    /// Answer the screenshot the dialog asks about with the person's
    /// `answer`, or refuse it for `None`, when its time ran out.
    pub(super) fn answer_screenshot(&mut self, answer: Option<Answer>) {
        let Some(asking) = self.asking.take() else {
            return;
        };
        if let Some(timer) = asking.timer.filter(|_| answer.is_some()) {
            self.loop_handle.remove(timer);
        }
        self.unblock_input();
        let pane = &asking.pane;
        let workspace = self.workspace_index(asking.workspace);
        if let Some(workspace) = workspace {
            self.workspaces[workspace].end_point_at(pane);
        }
        let result = match (workspace, answer) {
            (None, _) => Err(format!("workspace {} is closed", asking.workspace)),
            (Some(_), None) => Err(format!(
                "the person did not answer within {} seconds, so decade took no screenshot of pane {pane}",
                ASK_FOR.as_secs()
            )),
            (Some(_), Some(Answer::Deny)) => {
                Err(format!("the person refused a screenshot of pane {pane}"))
            }
            (Some(workspace), Some(answer)) => {
                if answer == Answer::Always {
                    self.accept_screenshots(workspace, pane);
                }
                self.capture(workspace, pane)
            }
        };
        let _ = asking.reply.send(result.map(mcp::Answer::Png));
        self.request_redraw();
    }

    fn accept_screenshots(&mut self, workspace: usize, pane: &str) {
        let shown = &mut self.workspaces[workspace];
        if let Some((tab, id)) = shown.by_name(pane)
            && let Some(pane) = shown.tabs[tab].layout.pane_mut(id)
        {
            pane.screenshots = true;
        }
    }

    /// Turn the accept screenshots mode of pane `id` on screen on or off.
    pub(super) fn toggle_screenshots(&mut self, id: usize) {
        if let Some(pane) = self.layout_mut().pane_mut(id) {
            pane.screenshots = !pane.screenshots;
            self.request_redraw();
        }
    }

    /// The dialog every viewer shows while a screenshot waits on the
    /// person's answer.
    pub(super) fn prompt(&self) -> Option<Prompt> {
        let asking = self.asking.as_ref()?;
        let workspace = (self.workspace_index(asking.workspace))
            .map(|index| self.workspaces[index].name.clone())
            .unwrap_or_default();
        let font = &self.text.chrome;
        Some(Prompt {
            agent: asking.agent.clone(),
            workspace,
            pane: asking.pane.clone(),
            armed: asking.armed(Instant::now()),
            window: (self.size.w, self.size.h),
            cell: (font.cell_width as i32, font.cell_height as i32),
        })
    }

    /// A PNG of the client area of pane `pane`, drawn as the workspace
    /// would be without decade's menus, bars or pointer hover.
    fn capture(&mut self, workspace: usize, pane: &str) -> Result<Vec<u8>, String> {
        let (name, _) = self.screenshot_target(workspace, pane)?;
        let shown = &self.workspaces[workspace];
        let layout = shown.layout();
        let rect = (layout.by_name(&name))
            .and_then(|id| layout.pane(id))
            .map(|pane| self.text.chrome.px_rect(pane.client_rect()))
            .ok_or_else(|| format!("no pane named {name}"))?;
        let frame = Rectangle::from_size(self.size);
        let rect = rect
            .intersection(frame)
            .filter(|rect| !rect.is_empty())
            .ok_or_else(|| format!("pane {name} has no room to draw"))?;
        let rgba = self.render_plain(workspace)?;
        let png = crop(&rgba, self.size.w, rect);
        self.workspaces[workspace].flash = Some((name, Instant::now()));
        self.start_pulse();
        self.request_redraw();
        Ok(png)
    }

    /// The workspace rendered off screen without the overlay, a drag, the
    /// Ctrl bar or hover, as RGBA rows, top first.
    fn render_plain(&mut self, workspace: usize) -> Result<Vec<u8>, String> {
        let overlay = self.overlay.take();
        let drag = self.drag.take();
        let bar = std::mem::replace(&mut self.bar, false);
        let hovers = (
            self.frame_hover.take(),
            self.hover.take(),
            self.tab_hover.take(),
        );
        let rendered = self.render_offscreen(workspace);
        self.overlay = overlay;
        self.drag = drag;
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
