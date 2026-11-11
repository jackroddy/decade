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

/// How long after the prompt appears its keys start to answer it, so a
/// key typed into the pane as it appears does not.
const ARMED_AFTER: Duration = Duration::from_millis(500);

/// A screenshot waiting on the person's answer, with its prompt drawn over
/// the pane.
pub(super) struct Asking {
    id: u64,
    workspace: u32,
    pane: String,
    agent: String,
    since: Instant,
    reply: Reply,
    timer: Option<RegistrationToken>,
}

impl State {
    /// Answer a `screenshot` of pane `pane` at once when the pane accepts
    /// screenshots, or else once the person answers its prompt.
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

    /// Put up the prompt for a screenshot of the pane named `pane` and
    /// point at the pane.
    fn ask(&mut self, workspace: usize, pane: String, caller: Option<&str>, reply: Reply) {
        let shown = &self.workspaces[workspace];
        let id = shown.id;
        if (self.asking.iter()).any(|asking| asking.workspace == id && asking.pane == pane) {
            let reason =
                format!("a screenshot of pane {pane} already waits for the person's answer");
            let _ = reply.send(Err(reason));
            return;
        }
        let agent = (shown.router.agents.list.iter())
            .find(|agent| agent.connected && caller.is_some() && agent.pane.as_deref() == caller)
            .map_or_else(|| "An agent".to_string(), |agent| agent.name.clone());
        let asking = self.next_asking;
        self.next_asking += 1;
        let timer = Timer::from_duration(ASK_FOR);
        let timer = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.answer_screenshot(asking, None);
            TimeoutAction::Drop
        });
        let now = Instant::now();
        let note = Some("screenshot?".to_string());

        // point_at fails only for an unknown name, and
        // screenshot_target gave the pane's own
        let _ = self.workspaces[workspace].point_at(&pane, note, now);
        self.asking.push(Asking {
            id: asking,
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

    /// Answer the screenshot `id` waits on with the person's `answer`, or
    /// refuse it for `None`, when its time ran out.
    fn answer_screenshot(&mut self, id: u64, answer: Option<Answer>) {
        let Some(index) = self.asking.iter().position(|asking| asking.id == id) else {
            return;
        };
        let asking = self.asking.remove(index);
        if let Some(timer) = asking.timer.filter(|_| answer.is_some()) {
            self.loop_handle.remove(timer);
        }
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

    /// The prompts drawn over panes of the workspace, each with the id of
    /// the screenshot it asks about.
    pub(super) fn prompts(&self, workspace: usize) -> Vec<(u64, Prompt)> {
        let shown = &self.workspaces[workspace];
        let layout = shown.layout();
        let font = &self.text.chrome;
        let cell = (font.cell_width as i32, font.cell_height as i32);
        (self.asking.iter())
            .filter(|asking| asking.workspace == shown.id)
            .filter_map(|asking| {
                let pane = layout.pane(layout.by_name(&asking.pane)?)?;
                let rect = font.px_rect(pane.client_rect());
                let area = ((rect.loc.x, rect.loc.y), (rect.size.w, rect.size.h));
                let agent = asking.agent.clone();
                Some((asking.id, Prompt { agent, area, cell }))
            })
            .collect()
    }

    /// What a left press at the pointer does to a prompt on screen: the
    /// screenshot it asks about and the answer pressed, if any, or `None`
    /// off every prompt.
    pub(super) fn prompt_hit(&self, (x, y): (i32, i32)) -> Option<(u64, Option<Answer>)> {
        (self.prompts(self.current()).into_iter())
            .find_map(|(id, prompt)| prompt.hit(x, y).map(|answer| (id, answer)))
    }

    /// The screenshot whose prompt takes keys: the one over the focused
    /// pane on screen, once it has been up for `ARMED_AFTER`.
    pub(super) fn prompt_for_keys(&self) -> Option<u64> {
        if self.overlay.is_some() {
            return None;
        }
        let shown = &self.workspaces[self.current()];
        let layout = shown.layout();
        let focused = &layout.pane(layout.focused)?.name;
        (self.asking.iter())
            .find(|asking| {
                asking.workspace == shown.id
                    && asking.pane == *focused
                    && asking.since.elapsed() >= ARMED_AFTER
            })
            .map(|asking| asking.id)
    }

    pub(super) fn answer_prompt(&mut self, id: u64, answer: Answer) {
        self.answer_screenshot(id, Some(answer));
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
