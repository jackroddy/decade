use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use smithay::backend::input::KeyState;
use smithay::input::pointer::CursorImageStatus;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, Mode as CalloopMode, PostAction, RegistrationToken};
use smithay::utils::{Physical, Point};

use super::pool::Pool;
use crate::wire::{
    self, CAPS_LOCK, Decoder, NUM_LOCK, Outbox, SCALE, Selection, ServerMessage, ViewerMessage,
};

use super::State;
use super::input::Repeat;

const MAX_KEYCODE: u32 = 0xffff;
const MAX_LOGICAL: u32 = 16384;
const MAX_SCALE: u32 = SCALE * 8;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const KEY_CAPSLOCK: u32 = 58;
const KEY_NUMLOCK: u32 = 69;

impl State {
    pub(super) fn request_redraw(&mut self) {
        for viewer in self.viewers.values_mut() {
            viewer.wants_frame = true;
        }
    }

    pub(super) fn viewer_connected(&mut self, stream: UnixStream) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(reader) = stream.try_clone() else {
            return;
        };
        let id = self.next_viewer;
        self.next_viewer += 1;
        self.viewers.insert(
            id,
            Viewer {
                stream,
                source: None,
                outbox: Outbox::new(),
                welcomed: false,
                logical: (0, 0),
                scale: SCALE,
                pool: Pool::new(),
                ready: false,
                wants_frame: true,
                keys: Vec::new(),
                buttons: Vec::new(),
                workspace: self.workspaces[0].id,
                focused: false,
            },
        );
        let mut decoder = Decoder::new();
        let inserted = self.loop_handle.insert_source(
            Generic::new(reader, Interest::READ, CalloopMode::Level),
            move |_, reader, state| {
                let open = match decoder.receive(reader) {
                    Ok(open) => open,
                    Err(error) => {
                        state.viewer_failed(id, &error.to_string());
                        return Ok(PostAction::Remove);
                    }
                };
                while state.viewers.contains_key(&id) {
                    match decoder.next_viewer_message() {
                        Ok(Some(message)) => state.viewer_message(id, message),
                        Ok(None) => break,
                        Err(error) => {
                            state.viewer_failed(id, &error.to_string());
                            return Ok(PostAction::Remove);
                        }
                    }
                }
                if !open || !state.viewers.contains_key(&id) {
                    state.viewer_gone(id);
                    return Ok(PostAction::Remove);
                }
                Ok(PostAction::Continue)
            },
        );
        match inserted {
            Ok(token) => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.source = Some(token);
                }
            }
            Err(_) => {
                self.viewers.remove(&id);
            }
        }
    }

    fn viewer_failed(&mut self, id: usize, message: &str) {
        self.send(
            id,
            ServerMessage::Error {
                message: message.to_string(),
            },
        );
        self.flush_viewers();
        self.viewer_gone(id);
    }

    fn viewer_gone(&mut self, id: usize) {
        let Some(viewer) = self.viewers.remove(&id) else {
            return;
        };
        if let Some(token) = viewer.source {
            self.loop_handle.remove(token);
        }
        let now = self.now();
        for button in viewer.buttons {
            self.pointer_button(button, false, now);
        }
        for keycode in viewer.keys {
            self.keyboard(keycode, false, now);
        }
        for selection in [Selection::Clipboard, Selection::Primary] {
            if self.viewer_owns(selection, id) {
                self.clear_selection(selection);
            }
        }
        if self.active == Some(id) {
            self.close_overlay();
            let next = self.welcomed().next();
            self.set_active(next);
        }
    }

    pub(super) fn close_viewers(&mut self) {
        for viewer in self.viewers.values_mut().filter(|viewer| viewer.welcomed) {
            let _ = viewer.outbox.push_server_message(ServerMessage::Close {
                message: "the server is shutting down".to_string(),
            });
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            let _ = viewer.outbox.flush(&viewer.stream, Some(deadline));
        }
    }

    pub(super) fn send(&mut self, id: usize, message: ServerMessage) {
        if let Some(viewer) = self.viewers.get_mut(&id)
            && viewer.outbox.push_server_message(message).is_err()
        {
            eprintln!("decade: a message to a viewer could not be encoded");
        }
    }

    pub(super) fn flush_viewers(&mut self) {
        let mut failed = Vec::new();
        for (&id, viewer) in &mut self.viewers {
            if !viewer.outbox.is_empty() && viewer.outbox.send(&viewer.stream).is_err() {
                failed.push(id);
            }
        }
        for id in failed {
            self.viewer_gone(id);
        }
    }

    pub(super) fn welcomed(&self) -> impl Iterator<Item = usize> + '_ {
        self.viewers
            .iter()
            .filter(|(_, viewer)| viewer.welcomed)
            .map(|(&id, _)| id)
    }

    fn make_active(&mut self, id: usize) {
        if self.active != Some(id) {
            self.set_active(Some(id));
        }
    }

    fn set_active(&mut self, active: Option<usize>) {
        let before = self.current();
        self.active = active;
        self.apply_active_size();
        if self.current() != before {
            self.workspace_shown();
        }
    }

    pub(super) fn show_on_active(&mut self, workspace: u32) {
        if let Some(active) = self.active {
            self.show(active, workspace);
        }
    }

    /// Make the viewer show the workspace, and tell it so.
    pub(super) fn set_shown(&mut self, viewer: usize, workspace: u32) {
        if let Some(entry) = self.viewers.get_mut(&viewer) {
            entry.workspace = workspace;
        }
        self.send(viewer, ServerMessage::Showing { id: workspace });
    }

    fn show(&mut self, viewer: usize, workspace: u32) {
        let before = self.current();
        let Some(entry) = self.viewers.get(&viewer) else {
            return;
        };
        let shown = match self.workspace_index(workspace) {
            Some(_) => workspace,
            None => entry.workspace,
        };
        self.set_shown(viewer, shown);
        if self.current() != before {
            self.workspace_shown();
        }
        self.request_redraw();
    }

    fn apply_active_size(&mut self) {
        let Some(viewer) = self.active.and_then(|id| self.viewers.get(&id)) else {
            return;
        };
        let (width, height) = viewer.logical;
        let scale = viewer.scale;
        if width == 0 || height == 0 {
            return;
        }
        let factor = scale as f64 / SCALE as f64;
        let physical = |logical: u32| (logical as f64 * factor).round() as u32;
        self.resized(physical(width), physical(height), factor);
        let ids: Vec<usize> = self.welcomed().collect();
        for id in ids {
            self.send(
                id,
                ServerMessage::Output {
                    width,
                    height,
                    scale,
                },
            );
        }
    }

    fn viewer_message(&mut self, id: usize, message: ViewerMessage) {
        if let Some(problem) = out_of_range(&message) {
            self.viewer_failed(id, problem);
            return;
        }
        let now = self.now();
        match message {
            ViewerMessage::Hello {
                version,
                width,
                height,
                scale,
                ..
            } => self.welcome(id, version, (width, height), scale),
            ViewerMessage::Formats { formats } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.pool.set_formats(&formats);
                }
            }
            ViewerMessage::Resize {
                width,
                height,
                scale,
            } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.logical = (width, height);
                    viewer.scale = scale;
                }
                self.set_active(Some(id));
            }
            ViewerMessage::BufferFailed { id: buffer } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.pool.failed(buffer, &mut viewer.outbox);
                    viewer.ready = true;
                    viewer.wants_frame = true;
                }
            }
            ViewerMessage::Release { id: buffer, .. } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.pool.release(buffer, self.size, &mut viewer.outbox);
                }
            }
            ViewerMessage::Ready => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.ready = true;
                }
            }
            ViewerMessage::Key {
                time,
                keycode,
                pressed,
            } => {
                self.make_active(id);
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    hold(&mut viewer.keys, keycode, pressed);
                }
                self.keyboard(keycode, pressed, time);
            }
            ViewerMessage::Keys { locked, keycodes } => self.sync_keys(id, locked, keycodes, now),
            ViewerMessage::KeysReleased => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.focused = false;
                }
                let keys = self
                    .viewers
                    .get_mut(&id)
                    .map(|viewer| std::mem::take(&mut viewer.keys))
                    .unwrap_or_default();
                for keycode in keys {
                    self.keyboard(keycode, false, now);
                }
                self.stop_repeat();
            }
            ViewerMessage::Pointer { time, x, y } => {
                let location = Point::<f64, Physical>::from((x, y)).to_logical(self.scale);
                self.pointer_moved(location, time);
            }
            ViewerMessage::Button {
                time,
                button,
                pressed,
            } => {
                self.make_active(id);
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    hold(&mut viewer.buttons, button, pressed);
                }
                self.pointer_button(button, pressed, time);
            }
            ViewerMessage::Scroll(scroll) => self.pointer_scroll(scroll),
            ViewerMessage::Offer {
                selection,
                serial,
                seen,
                mimes,
            } => self.viewer_offer(id, selection, serial, seen, mimes),
            ViewerMessage::Cleared { selection, .. } => {
                if self.viewer_owns(selection, id) {
                    self.clear_selection(selection);
                }
            }
            ViewerMessage::Request {
                selection,
                serial,
                mime,
                fd,
            } => self.viewer_request(selection, serial, mime, fd),
            ViewerMessage::List => {
                let workspaces = self.listing();
                self.send(id, ServerMessage::Listing { workspaces });
                self.flush_viewers();
                self.viewer_gone(id);
            }
            ViewerMessage::Repeat { rate, delay } => {
                self.repeat = Repeat {
                    rate: rate.min(1000) as i32,
                    delay: delay.clamp(1, 5000) as i32,
                };
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.change_repeat_info(self.repeat.rate, self.repeat.delay);
            }
            ViewerMessage::Error { message, .. } => {
                eprintln!("decade: a viewer reported an error: {message}");
                self.viewer_gone(id);
            }
        }
    }

    /// Every workspace, as `listing` describes it.
    fn listing(&self) -> Vec<wire::Listed> {
        (self.workspaces.iter())
            .map(|workspace| wire::Listed {
                id: workspace.id,
                name: workspace.name.clone(),
                cwd: workspace.cwd.display().to_string(),
                panes: workspace.panes().count() as u32,
            })
            .collect()
    }

    /// Answer a viewer's hello and send it what a new viewer needs.
    fn welcome(&mut self, id: usize, version: u32, logical: (u32, u32), scale: u32) {
        if version == 0 {
            self.viewer_failed(id, "decade speaks protocol version 1");
            return;
        }
        if let Some(viewer) = self.viewers.get_mut(&id) {
            viewer.welcomed = true;
            viewer.logical = logical;
            viewer.scale = scale;
            viewer.ready = true;
        }
        let version = version.min(wire::VERSION);
        self.send(id, ServerMessage::Welcome { version });
        self.send(id, self.workspace_list());
        self.set_shown(id, self.workspaces[0].id);
        self.make_active(id);
        self.send_cursor(id);
        for selection in [Selection::Clipboard, Selection::Primary] {
            if self.offered[selection as usize].mimes.is_some() {
                self.tell_selection(id, selection);
            }
        }
        self.request_redraw();
    }

    /// Match the seat's lock state and held keys to the viewer's host,
    /// whose window has just gained keyboard focus.
    fn sync_keys(&mut self, id: usize, locked: u32, keycodes: Vec<u32>, time: u32) {
        if let Some(viewer) = self.viewers.get_mut(&id) {
            viewer.focused = true;
        }
        let modifiers = self.seat.get_keyboard().unwrap().modifier_state();
        let locks = [
            (modifiers.caps_lock, locked & CAPS_LOCK != 0, KEY_CAPSLOCK),
            (modifiers.num_lock, locked & NUM_LOCK != 0, KEY_NUMLOCK),
        ];
        for (on, wanted, keycode) in locks {
            if on != wanted {
                self.forward_key(keycode, KeyState::Pressed, time);
                self.forward_key(keycode, KeyState::Released, time);
            }
        }
        for keycode in keycodes {
            let Some(viewer) = self.viewers.get_mut(&id) else {
                return;
            };
            if viewer.keys.contains(&keycode) {
                continue;
            }
            viewer.keys.push(keycode);
            self.forward_key(keycode, KeyState::Pressed, time);
        }
    }

    pub(super) fn render_viewers(&mut self) {
        let ids: Vec<usize> = self
            .viewers
            .iter()
            .filter(|(_, viewer)| {
                viewer.welcomed && viewer.ready && viewer.wants_frame && viewer.pool.has_formats()
            })
            .map(|(&id, _)| id)
            .collect();
        for id in ids {
            self.render_to(id);
        }
    }

    fn render_to(&mut self, id: usize) {
        let Some(viewer) = self.viewers.get_mut(&id) else {
            return;
        };
        let Some((buffer, mut dmabuf)) =
            viewer
                .pool
                .slot(self.size, &mut self.gpu, &mut viewer.outbox)
        else {
            return;
        };
        let shown = viewer.workspace;
        let workspace = self.workspace_index(shown).unwrap_or(0);
        let (fence, animating) = match self.render(&mut dmabuf, workspace) {
            Ok(rendered) => rendered,
            Err(error) => {
                if !self.render_failing {
                    eprintln!("decade: rendering a frame failed: {error}");
                    self.render_failing = true;
                }
                return;
            }
        };
        self.render_failing = false;
        let Some(viewer) = self.viewers.get_mut(&id) else {
            return;
        };
        viewer.pool.lend(buffer);
        viewer.ready = false;
        viewer.wants_frame = animating;
        let _ = viewer
            .outbox
            .push_server_message(ServerMessage::Frame { id: buffer, fence });
    }

    pub(super) fn send_cursor(&mut self, id: usize) {
        let message = match &self.cursor {
            CursorImageStatus::Hidden => ServerMessage::CursorHidden,
            CursorImageStatus::Named(icon) => ServerMessage::CursorShape {
                shape: icon.name().to_string(),
            },
            CursorImageStatus::Surface(_) => ServerMessage::CursorShape {
                shape: "default".to_string(),
            },
        };
        self.send(id, message);
    }
}

pub(super) struct Viewer {
    pub(super) stream: UnixStream,
    pub(super) source: Option<RegistrationToken>,
    pub(super) outbox: Outbox,
    pub(super) welcomed: bool,
    pub(super) logical: (u32, u32),
    pub(super) scale: u32,

    pub(super) pool: Pool,
    pub(super) ready: bool,
    pub(super) wants_frame: bool,
    pub(super) keys: Vec<u32>,
    pub(super) buttons: Vec<u32>,
    pub(super) workspace: u32,

    /// Whether the viewer's window has keyboard focus on its host.
    pub(super) focused: bool,
}

/// Record a key or button as held or let go.
fn hold(held: &mut Vec<u32>, code: u32, pressed: bool) {
    held.retain(|&other| other != code);
    if pressed {
        held.push(code);
    }
}

fn out_of_range(message: &ViewerMessage) -> Option<&'static str> {
    let size_fits = |width: u32, height: u32, scale: u32| {
        (1..=MAX_LOGICAL).contains(&width)
            && (1..=MAX_LOGICAL).contains(&height)
            && (1..=MAX_SCALE).contains(&scale)
    };
    let fits = match message {
        ViewerMessage::Hello {
            width,
            height,
            scale,
            ..
        }
        | ViewerMessage::Resize {
            width,
            height,
            scale,
        } => size_fits(*width, *height, *scale),
        ViewerMessage::Key { keycode, .. } => *keycode <= MAX_KEYCODE,
        ViewerMessage::Keys { keycodes, .. } => {
            keycodes.iter().all(|&keycode| keycode <= MAX_KEYCODE)
        }
        _ => true,
    };
    (!fits).then_some("a size, scale or keycode is out of range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_scales_and_keycodes_past_their_limits_are_out_of_range() {
        let resize = |width, height, scale| ViewerMessage::Resize {
            width,
            height,
            scale,
        };
        assert!(out_of_range(&resize(1280, 800, 120)).is_none());
        assert!(out_of_range(&resize(MAX_LOGICAL, MAX_LOGICAL, MAX_SCALE)).is_none());
        assert!(out_of_range(&resize(0, 800, 120)).is_some());
        assert!(out_of_range(&resize(1280, MAX_LOGICAL + 1, 120)).is_some());
        assert!(out_of_range(&resize(1280, 800, 0)).is_some());
        assert!(out_of_range(&resize(1280, 800, MAX_SCALE + 1)).is_some());
        let key = |keycode| ViewerMessage::Key {
            time: 0,
            keycode,
            pressed: true,
        };
        assert!(out_of_range(&key(30)).is_none());
        assert!(out_of_range(&key(u32::MAX)).is_some());
        let keys = ViewerMessage::Keys {
            locked: 0,
            keycodes: vec![30, u32::MAX],
        };
        assert!(out_of_range(&keys).is_some());
    }
}
