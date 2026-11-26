use std::time::{Duration, Instant};

use decade_ui::Driven;
use smithay::backend::input::KeyState;
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER};

use crate::server::drive::{self, Event, Input, Script};
use crate::server::mcp;
use crate::server::workspace;

use super::State;
use super::screenshot::Asker;
use super::viewers::hold;

const KEY_ESC: u32 = 1;

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

/// An agent's script playing in a scratch pane.
pub(super) struct Driving {
    workspace: u32,
    tab: usize,
    pane_id: usize,
    pane: String,
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
        let started = (self.input_target(workspace, Some(pane), &played.script))
            .and_then(|(name, _, _)| Ok((self.client_px(workspace, &name)?.1, name)))
            .and_then(|(rect, name)| {
                let at = self.pointer.to_physical(self.scale);
                let start = (
                    (at.x - rect.loc.x as f64).clamp(0.0, rect.size.w as f64 - 1.0),
                    (at.y - rect.loc.y as f64).clamp(0.0, rect.size.h as f64 - 1.0),
                );
                let size = (rect.size.w as f64, rect.size.h as f64);
                Ok((drive::plan(&played.script, size, start)?, name))
            });
        let (events, name) = match started {
            Ok(started) => started,
            Err(reason) => {
                let _ = played.reply.send(Err(reason));
                return;
            }
        };
        let shown = &self.workspaces[workspace];
        let Some((tab, pane_id)) = shown.by_name(&name) else {
            let _ = played.reply.send(Err(format!("no pane named {name}")));
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
        self.focus(pane_id);
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
        });
        self.start_pulse();
        self.request_redraw();
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
                let (pane, workspace) = (driving.pane.clone(), driving.workspace);
                let Some(workspace) = self.workspace_index(workspace) else {
                    return;
                };
                let Ok((_, rect)) = self.client_px(workspace, &pane) else {
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
        let pane = &driving.pane;
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
        } else if shown.tabs[driving.tab].toplevel(driving.pane_id).is_none() {
            format!("the window in pane {pane} closed")
        } else {
            return None;
        };
        Some(lost)
    }

    /// End the script, letting go of what it holds and giving the person
    /// back keyboard focus and the pointer, and answer the agent.
    fn stop_drive(&mut self, reason: &str, finished: bool) {
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
        self.pointer_moved(driving.person, time);
        self.request_redraw();
        let text = format!(
            "played {} of {} steps in pane {} over {:.1} s; {reason}",
            driving.done,
            driving.steps,
            driving.pane,
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
    /// script, and every other press is dropped. A release reaches
    /// decade's keyboard state and no client.
    pub(super) fn driven_key(&mut self, evdev: u32, pressed: bool, time: u32) {
        if pressed {
            if evdev == KEY_ESC {
                self.stop_drive("the person pressed Escape", false);
            }
            return;
        }
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.input::<(), _>(
            self,
            Keycode::new(evdev + 8),
            KeyState::Released,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Intercept(()),
        );
    }

    /// The name of the pane a script plays in.
    pub(super) fn driven_pane(&self) -> Option<&str> {
        self.driving.as_ref().map(|driving| driving.pane.as_str())
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
