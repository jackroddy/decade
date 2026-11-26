use std::time::{Duration, Instant};

use decade_ui::Driven;
use smithay::backend::input::KeyState;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle};

use crate::server::drive::{self, Event, Input, Script};
use crate::server::mcp;
use crate::server::workspace;

use super::State;
use super::screenshot::Asker;
use super::viewers::{KEY_CAPSLOCK, KEY_NUMLOCK, hold};

const KEY_ESC: u32 = 1;

/// The evdev Ctrl, Shift, Alt and Super keys, left and right.
const MODIFIERS: [u32; 8] = [29, 97, 42, 54, 56, 100, 125, 126];

/// The glyph of the driven tag, `md-robot` in the Nerd Font.
const ICON: char = '\u{f06a9}';

/// The longest decade goes between checks that the driven pane is still
/// on screen, during a wait.
const CHECK_EVERY: Duration = Duration::from_millis(100);

/// A script an agent sent, and where its answer goes.
pub(super) struct Played {
    pub(super) reply: mcp::Reply,
    pub(super) script: Script,

    /// The agent's name, for the driven tag.
    pub(super) agent: String,

    /// How long the script plays, in milliseconds.
    pub(super) ms: u64,
}

/// A script ready to play in a scratch pane.
struct Planned {
    /// The pane's own name.
    name: String,

    /// The pane's client area on screen.
    rect: Rectangle<i32, Physical>,

    /// Where the script's pointer starts, in pane pixels.
    start: (f64, f64),
    events: Vec<Event>,
}

/// An agent's script playing in a scratch pane.
pub(super) struct Driving {
    workspace: u32,
    tab: usize,
    pane_id: usize,

    /// The pane's name when the script started.
    pane: String,

    /// The window the script plays in.
    surface: WlSurface,
    events: Vec<Event>,
    next: usize,
    steps: usize,
    done: usize,
    started: Instant,
    reply: mcp::Reply,
    timer: Option<RegistrationToken>,

    /// The buttons and keys the script holds down.
    buttons: Vec<u32>,
    keys: Vec<u32>,

    /// The pane that had keyboard focus before the script.
    focused: usize,

    /// Where the person's own pointer is.
    pub(super) person: Point<f64, Logical>,

    /// The driven tag's text.
    label: String,

    /// The lock keys the person pressed an odd number of times, and the
    /// viewer's last report of its held keys, to apply when the script
    /// ends.
    locks: Vec<u32>,
    sync: Option<(usize, u32, Vec<u32>)>,
}

impl Driving {
    /// Keep viewer `id`'s report of its locks and held keys for the end
    /// of the script.
    pub(super) fn sync_later(&mut self, id: usize, locked: u32, keycodes: Vec<u32>) {
        self.sync = Some((id, locked, keycodes));
        self.locks.clear();
    }
}

impl State {
    /// Play `script` in the scratch pane `pane` names at once when the
    /// pane accepts input, or else once the person answers the dialog.
    pub(super) fn scratch_input(
        &mut self,
        workspace: usize,
        pane: Option<&str>,
        script: Script,
        caller: Option<&str>,
        reply: mcp::Reply,
    ) {
        let (name, accepts, ms) = match self.input_target(workspace, pane, &script) {
            Ok(target) => target,
            Err(reason) => {
                let _ = reply.send(Err(reason));
                return;
            }
        };
        let played = Played {
            reply,
            script,
            agent: self.agent_name(workspace, caller),
            ms,
        };
        if accepts {
            self.drive(workspace, &name, played);
        } else {
            self.ask(workspace, name, caller, Asker::Input(played));
        }
    }

    /// The scratch pane's own name, whether it accepts input and how long
    /// `script` plays there, or why it cannot play.
    fn input_target(
        &self,
        workspace: usize,
        pane: Option<&str>,
        script: &Script,
    ) -> Result<(String, bool, u64), String> {
        if let Some(pane) = self.driven_pane() {
            return Err(format!(
                "an agent's input is already playing in pane {pane}"
            ));
        }
        if self.asking.is_some() {
            return Err("decade is already asking the person about another capture".to_string());
        }
        let shown = &self.workspaces[workspace];
        let index = shown.find_scratch(pane)?;
        let (name, info) = self.screenshot_target(workspace, &shown.scratch[index].pane)?;
        if !self.on_active_viewer(shown.id) {
            return Err(format!(
                "workspace {} is not on screen, so the person could not watch or stop the input",
                shown.name
            ));
        }
        let toplevel = (shown.by_name(&name)).and_then(|(tab, id)| shown.tabs[tab].toplevel(id));
        if toplevel.is_none() {
            return Err(format!("pane {name} has no window to take the input"));
        }
        let (_, rect) = self.client_px(workspace, &name)?;
        let size = (rect.size.w as f64, rect.size.h as f64);
        let events = drive::plan(script, size, (0.0, 0.0))?;
        let ms = events.last().map_or(0, |event| event.at);
        Ok((name, info.input, ms))
    }

    /// Start playing `played` in pane `pane`, which the person allowed.
    pub(super) fn drive(&mut self, workspace: usize, pane: &str, played: Played) {
        let Planned {
            name,
            rect,
            start,
            events,
        } = match self.drive_plan(workspace, pane, &played.script) {
            Ok(planned) => planned,
            Err(reason) => {
                let _ = played.reply.send(Err(reason));
                return;
            }
        };
        let shown = &self.workspaces[workspace];
        let window = (shown.by_name(&name)).and_then(|(tab, id)| {
            let surface = shown.tabs[tab].toplevel(id)?.wl_surface().clone();
            Some((tab, id, surface))
        });
        let Some((tab, pane_id, surface)) = window else {
            let _ = played
                .reply
                .send(Err(format!("pane {name} has no window to take the input")));
            return;
        };
        let workspace_id = shown.id;
        let focused = self.layout().focused;
        let person = self.pointer;

        // a button the person holds in a client is let go, so
        // the script's presses start from none held
        let time = self.now();
        for button in self.forwarded.clone() {
            self.forward_button(button, false, time);
        }
        if self.overlay.is_some() {
            self.close_overlay();
        }
        self.dismiss_popups();
        self.block_input();
        self.release_held_keys(time);
        self.focus(pane_id);
        let at = Point::<f64, Physical>::from((
            rect.loc.x as f64 + start.0,
            rect.loc.y as f64 + start.1,
        ));
        self.agent_motion(at.to_logical(self.scale), time);
        let timer = self
            .loop_handle
            .insert_source(Timer::immediate(), |_, _, state| match state.drive_tick() {
                Some(at) => TimeoutAction::ToInstant(at),
                None => TimeoutAction::Drop,
            });
        let agent = match played.agent.as_str() {
            "An agent" => "agent",
            agent => agent,
        };
        self.driving = Some(Driving {
            workspace: workspace_id,
            tab,
            pane_id,
            pane: name,
            surface,
            events,
            next: 0,
            steps: played.script.steps.len(),
            done: 0,
            started: Instant::now(),
            reply: played.reply,
            timer: timer.ok(),
            buttons: Vec::new(),
            keys: Vec::new(),
            focused,
            person,
            label: format!("{agent} · Esc stops"),
            locks: Vec::new(),
            sync: None,
        });
        self.start_pulse();
        self.request_redraw();
    }

    /// `script` ready to play in pane `pane`, or why it cannot.
    fn drive_plan(&self, workspace: usize, pane: &str, script: &Script) -> Result<Planned, String> {
        let (name, _, _) = self.input_target(workspace, Some(pane), script)?;
        let (_, rect) = self.client_px(workspace, &name)?;
        let at = self.pointer.to_physical(self.scale);
        let start = (
            (at.x - rect.loc.x as f64).clamp(0.0, rect.size.w as f64 - 1.0),
            (at.y - rect.loc.y as f64).clamp(0.0, rect.size.h as f64 - 1.0),
        );
        let size = (rect.size.w as f64, rect.size.h as f64);
        let events = drive::plan(script, size, start)?;
        Ok(Planned {
            name,
            rect,
            start,
            events,
        })
    }

    /// Let go of every key decade holds, while no client has keyboard
    /// focus, so the driven client starts with no keys or modifiers held.
    fn release_held_keys(&mut self, time: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for keycode in keyboard.pressed_keys() {
            self.forward_key(keycode.raw() - 8, KeyState::Released, time);
        }
    }

    /// Play the events that are due, and return when to play the next, or
    /// `None` once the script has ended.
    fn drive_tick(&mut self) -> Option<Instant> {
        if let Some(reason) = self.drive_lost() {
            if let Some(driving) = &mut self.driving {
                driving.timer = None;
            }
            self.stop_drive(&reason, false);
            return None;
        }
        let driving = self.driving.as_ref()?;
        let started = driving.started;
        let elapsed = started.elapsed().as_millis() as u64;
        while let Some(driving) = &mut self.driving
            && let Some(&event) = driving.events.get(driving.next)
            && event.at <= elapsed
        {
            driving.next += 1;
            self.play(event.input);
        }
        let driving = self.driving.as_mut()?;
        let Some(next) = driving.events.get(driving.next) else {
            driving.timer = None;
            self.stop_drive("it finished", true);
            return None;
        };
        let at = started + Duration::from_millis(next.at);
        Some(at.min(Instant::now() + CHECK_EVERY))
    }

    fn play(&mut self, input: Input) {
        let time = self.now();
        let Some(driving) = &mut self.driving else {
            return;
        };
        match input {
            Input::Motion(x, y) => {
                let (workspace, tab, id) = (driving.workspace, driving.tab, driving.pane_id);
                let Some(workspace) = self.workspace_index(workspace) else {
                    return;
                };
                let Some(rect) = self.pane_px(workspace, tab, id) else {
                    return;
                };
                let x = rect.loc.x as f64 + x.min(rect.size.w as f64 - 1.0);
                let y = rect.loc.y as f64 + y.min(rect.size.h as f64 - 1.0);
                let location = Point::<f64, Physical>::from((x, y)).to_logical(self.scale);
                self.agent_motion(location, time);
            }
            Input::Button(button, pressed) => {
                hold(&mut driving.buttons, button, pressed);
                self.forward_button(button, pressed, time);
            }
            Input::Key(key, pressed) => {
                hold(&mut driving.keys, key, pressed);
                let state = if pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                };
                self.forward_key(key, state, time);
            }
            Input::Done => driving.done += 1,
        }
    }

    /// Why the driven pane can no longer take the script, or `None` while
    /// it can.
    fn drive_lost(&self) -> Option<String> {
        let driving = self.driving.as_ref()?;
        let pane = self.driven_pane()?;
        let Some(index) = self.workspace_index(driving.workspace) else {
            return Some(format!("workspace {} closed", driving.workspace));
        };
        let shown = &self.workspaces[index];
        let lost = if shown.asleep.is_some() {
            format!("workspace {} went to sleep", shown.name)
        } else if !self.on_active_viewer(driving.workspace) {
            format!("workspace {} left the screen", shown.name)
        } else if shown.active_tab != driving.tab {
            format!("the tab of pane {pane} stopped being shown")
        } else if shown.tabs[driving.tab]
            .layout
            .pane(driving.pane_id)
            .is_none()
        {
            format!("pane {pane} closed")
        } else if !(shown.tabs[driving.tab].toplevel(driving.pane_id))
            .is_some_and(|toplevel| *toplevel.wl_surface() == driving.surface)
        {
            format!("the window the script played in left pane {pane}")
        } else {
            return None;
        };
        Some(lost)
    }

    /// End the script, letting go of what it holds and giving the person
    /// back keyboard focus and the pointer, and answer the agent.
    fn stop_drive(&mut self, reason: &str, finished: bool) {
        let name = self.driven_pane().map(str::to_string);
        let Some(driving) = self.driving.take() else {
            return;
        };
        if let Some(timer) = driving.timer {
            self.loop_handle.remove(timer);
        }
        let time = self.now();
        for &key in driving.keys.iter().rev() {
            self.forward_key(key, KeyState::Released, time);
        }
        for &button in driving.buttons.iter().rev() {
            self.forward_button(button, false, time);
        }
        let same_tab = (self.workspace_index(driving.workspace)).is_some_and(|index| {
            index == self.current() && self.workspaces[index].active_tab == driving.tab
        });
        let focused = if same_tab && self.layout().pane(driving.focused).is_some() {
            driving.focused
        } else {
            self.layout().focused
        };
        self.focus(focused);
        self.sync_after_drive(driving.sync, &driving.locks, time);
        self.pointer_moved(driving.person, time);
        self.request_redraw();
        let text = format!(
            "played {} of {} steps in pane {} over {:.1} s; {reason}",
            driving.done,
            driving.steps,
            name.unwrap_or(driving.pane),
            driving.started.elapsed().as_secs_f64()
        );
        let answer = if finished {
            Ok(mcp::Answer::Text(text))
        } else {
            Err(text)
        };
        let _ = driving.reply.send(answer);
    }

    /// Take a key from the person while a script plays: Escape stops the
    /// script, a lock key is kept to apply when it ends, and every other
    /// press and every release is dropped.
    pub(super) fn driven_key(&mut self, evdev: u32, pressed: bool) {
        if !pressed {
            return;
        }
        if evdev == KEY_ESC {
            self.stop_drive("the person pressed Escape", false);
        } else if let Some(driving) = &mut self.driving
            && (evdev == KEY_CAPSLOCK || evdev == KEY_NUMLOCK)
        {
            let pressed_twice = driving.locks.contains(&evdev);
            hold(&mut driving.locks, evdev, !pressed_twice);
        }
    }

    /// Bring decade's keys into line with the person's after a script:
    /// apply viewer `sync`'s report and the lock keys in `locks`, and press
    /// again the modifiers the active viewer still holds.
    fn sync_after_drive(&mut self, sync: Option<(usize, u32, Vec<u32>)>, locks: &[u32], time: u32) {
        if let Some((id, locked, keycodes)) = sync {
            self.sync_keys(id, locked, keycodes, time);
        }
        for &lock in locks {
            self.forward_key(lock, KeyState::Pressed, time);
            self.forward_key(lock, KeyState::Released, time);
        }
        let pressed = self.seat.get_keyboard().unwrap().pressed_keys();
        let held: Vec<u32> = (self.active.and_then(|id| self.viewers.get(&id)))
            .map(|viewer| viewer.keys.clone())
            .unwrap_or_default();
        for key in held {
            if MODIFIERS.contains(&key) && !pressed.iter().any(|code| code.raw() == key + 8) {
                self.forward_key(key, KeyState::Pressed, time);
            }
        }
    }

    /// The current name of the pane a script plays in.
    pub(super) fn driven_pane(&self) -> Option<&str> {
        let driving = self.driving.as_ref()?;
        let current = (self.workspace_index(driving.workspace))
            .and_then(|index| self.workspaces[index].tabs.get(driving.tab))
            .and_then(|tab| tab.layout.pane(driving.pane_id))
            .map(|pane| pane.name.as_str());
        Some(current.unwrap_or(&driving.pane))
    }

    /// The driven pane of workspace `workspace`'s shown tab, how long the
    /// script has played, and the driven tag's text.
    pub(super) fn driven_mark(&self, workspace: usize) -> Option<(usize, Duration, String)> {
        let driving = self.driving.as_ref()?;
        let shown = &self.workspaces[workspace];
        (driving.workspace == shown.id && driving.tab == shown.active_tab).then(|| {
            (
                driving.pane_id,
                driving.started.elapsed(),
                driving.label.clone(),
            )
        })
    }

    /// Whether the active viewer shows workspace `id`.
    fn on_active_viewer(&self, id: u32) -> bool {
        (self.active.and_then(|viewer| self.viewers.get(&viewer)))
            .is_some_and(|viewer| viewer.workspace == id)
    }
}

/// The driven tag and outline of a pane `elapsed` into its script.
pub(super) fn driven(pane: usize, elapsed: Duration, label: &str) -> Driven<'_> {
    Driven {
        pane,
        strength: workspace::pulse(elapsed),
        icon: ICON,
        label,
    }
}
