use std::time::{Duration, Instant};

use smithay::backend::input::{
    Axis as InputAxis, AxisRelativeDirection, AxisSource, ButtonState, KeyState,
};
use smithay::desktop::WindowSurfaceType;
use smithay::desktop::utils::under_from_surface_tree;
use smithay::input::keyboard::{FilterResult, Keycode, Keysym, ModifiersState, keysyms};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER};

use crate::server::chrome::context_menu::Place;
use crate::server::chrome::frame::Hit;
use crate::server::chrome::menu::{MenuKey, Outcome};
use crate::server::chrome::rename::Named;
use crate::server::panes;
use crate::server::panes::{BarButton, Boundary, Direction, Drop};
use crate::wire::{self, BTN_LEFT, BTN_RIGHT, KEY_LEFTCTRL, KEY_LEFTMETA, KEY_LEFTSHIFT};

use super::{Overlay, State, popups_over};

const KEY_C: u32 = 46;
const KEY_V: u32 = 47;
const KEY_X: u32 = 45;
const KEY_RIGHTMETA: u32 = 126;
const RESIZE_STEP: u32 = 2;

/// The longest time between two presses on a name that opens it for renaming.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

impl State {
    fn show_bar(&mut self, show: bool) {
        if self.bar != show {
            self.bar = show;
            self.hover = None;
            self.tab_hover = None;
            self.request_redraw();
        }
    }

    fn cancel_ctrl_timer(&mut self) {
        if let Some(token) = self.ctrl_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    fn ctrl_pressed(&mut self) {
        self.cancel_ctrl_timer();
        let timer = Timer::from_duration(Duration::from_millis(self.settings.bar_hold_ms));
        self.ctrl_timer = self
            .loop_handle
            .insert_source(timer, |_, _, state| {
                state.ctrl_timer = None;
                state.show_bar(true);
                TimeoutAction::Drop
            })
            .ok();
    }

    fn ctrl_released(&mut self) {
        self.cancel_ctrl_timer();
        self.show_bar(false);
    }

    pub(super) fn pointer_moved(&mut self, location: Point<f64, Logical>, time: u32) {
        self.pointer = location;
        if matches!(self.overlay, Some(Overlay::Picker { .. })) {
            let point = self.pointer.to_physical(self.scale).to_i32_round();
            let ids = self.workspace_ids();
            if let Some(Overlay::Picker { picker, .. }) = &mut self.overlay
                && picker.motion(point, &ids)
            {
                self.request_redraw();
            }
            return;
        }
        if matches!(self.overlay, Some(Overlay::Context(_))) {
            let (x, y) = self.pointer_px();
            if let Some(Overlay::Context(menu)) = &mut self.overlay
                && menu.hover(x, y)
            {
                self.request_redraw();
            }
            return;
        }
        let (column, row) = self.pointer_cell();
        let (x, y) = self.pointer_px();
        let threshold = self.text.chrome.cell_height as i32 / 2;
        let current = self.current();
        match &mut self.drag {
            Some(Drag::Boundary(boundary)) => {
                let layout = self.workspaces[current].layout_mut();
                layout.drag_boundary(boundary, column, row);
                self.relayout();
                return;
            }
            Some(Drag::Pane { cell, .. }) => {
                *cell = Some((column, row));
                self.request_redraw();
                return;
            }
            Some(Drag::Sidebar { from, moved, .. }) => {
                *moved |= (x - from.0).abs() >= threshold || (y - from.1).abs() >= threshold;
                if *moved {
                    self.request_redraw();
                }
                return;
            }
            None => {}
        }
        let area = self.pane_area();
        let in_panes = column >= area.column && row >= area.row;

        // the popover lies over the panes, and is shown
        // only while frame_hover holds a tile
        let hover = if !in_panes || self.frame_hover.is_some() {
            let (x, y) = self.pointer_px();
            self.frame(current).hit(x, y)
        } else {
            None
        };
        let on_popover = in_panes && hover.is_some();
        if hover != self.frame_hover {
            self.frame_hover = hover;
            self.request_redraw();
        }
        if self.bar {
            let hover = (self.layout().bar_hit(column, row))
                .filter(|&(_, button)| button != BarButton::Name && !on_popover);
            if hover != self.hover {
                self.hover = hover;
                self.request_redraw();
            }
        } else {
            let tab_hover = self.layout().tab_hit(column, row).filter(|_| !on_popover);
            if tab_hover != self.tab_hover {
                self.tab_hover = tab_hover;
                self.request_redraw();
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        let under = self.pane_under_pointer().filter(|_| !on_popover);
        let focus = under.and_then(|id| {
            let pane = self.layout().pane(id)?;
            let (origin, _) = self.client_geometry(pane);
            let toplevel = pane.toplevel()?.wl_surface();
            let popup = popups_over(toplevel).find_map(|(popup, at)| {
                let surface = popup.wl_surface();
                under_from_surface_tree(surface, self.pointer, origin + at, WindowSurfaceType::ALL)
            });
            Some(match popup {
                Some((surface, at)) => (surface, at.to_f64()),
                None => (toplevel.clone(), origin.to_f64()),
            })
        });
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location: self.pointer,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    pub(super) fn pointer_left(&mut self) {
        if self.frame_hover.is_some() || self.hover.is_some() || self.tab_hover.is_some() {
            self.frame_hover = None;
            self.hover = None;
            self.tab_hover = None;
            self.request_redraw();
        }
    }

    pub(super) fn pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        if matches!(self.overlay, Some(Overlay::Picker { .. })) {
            if button == BTN_LEFT {
                let point = self.pointer.to_physical(self.scale).to_i32_round();
                let (ids, grid) = (self.workspace_ids(), self.picker_grid());
                let Some(Overlay::Picker { picker, .. }) = &mut self.overlay else {
                    return;
                };
                let outcome = if pressed {
                    picker.press(point, &ids, &grid)
                } else {
                    picker.release(point, &ids, &grid)
                };
                self.act(outcome);
            }
            return;
        }
        if let Some(Overlay::Context(menu)) = &self.overlay {
            if pressed {
                let (x, y) = self.pointer_px();
                let outcome = match button {
                    BTN_LEFT => menu.click(x, y),
                    _ if menu.contains(x, y) => Outcome::Open,
                    _ => Outcome::Close,
                };
                self.act(outcome);
            }
            return;
        }
        if let Some(Overlay::Rename(field)) = &self.overlay
            && pressed
        {
            let (x, y) = self.pointer_px();
            if field.contains(x, y) {
                return;
            }
            self.close_overlay();
        }
        if pressed && !self.press_reaches_popup(button) {
            self.dismiss_popups();
        }
        if button == BTN_LEFT && pressed {
            let named = self.name_under_pointer();
            if self.double_click(named)
                && let Some(named) = named
            {
                self.open_rename(named);
                return;
            }
        }
        if button == BTN_RIGHT && pressed && self.right_press_on_chrome() {
            return;
        }
        if button == BTN_LEFT && pressed {
            let (x, y) = self.pointer_px();
            if let Some(hit) = self.frame(self.current()).hit(x, y) {
                // a card or header acts on release, unless it was
                // dragged, which moves it instead
                if self.sidebar_open && matches!(hit, Hit::Show(_) | Hit::Group(_)) {
                    let from = (x, y);
                    let moved = false;
                    self.drag = Some(Drag::Sidebar { hit, from, moved });
                    return;
                }
                self.frame_hit(hit);
                return;
            }
        }
        if button == BTN_LEFT {
            if pressed && self.press_on_chrome() {
                return;
            }
            if !pressed && let Some(drag) = self.drag.take() {
                self.drop(drag);
                return;
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        pointer.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                button,
                state,
            },
        );
        pointer.frame(self);
    }

    pub(super) fn pointer_scroll(&mut self, scroll: wire::Scroll) {
        let source = match scroll.source {
            1 => AxisSource::Finger,
            2 => AxisSource::Continuous,
            3 => AxisSource::WheelTilt,
            _ => AxisSource::Wheel,
        };
        let axis = if scroll.axis == 1 {
            InputAxis::Horizontal
        } else {
            InputAxis::Vertical
        };
        let mut frame = AxisFrame::new(scroll.time).source(source);
        if scroll.stop {
            frame = frame.stop(axis);
        } else {
            frame = frame.value(axis, scroll.amount / self.scale);
            if scroll.steps != 0 {
                frame = frame.v120(axis, scroll.steps);
            }
        }
        if scroll.inverted {
            frame = frame.relative_direction(axis, AxisRelativeDirection::Inverted);
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    pub(super) fn keyboard(&mut self, evdev: u32, pressed: bool, time: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let code = Keycode::new(evdev + 8);
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let palette = self.overlay.is_some();
        let restartable = self
            .layout()
            .pane(self.layout().focused)
            .is_some_and(|pane| pane.restartable());
        let mut control_key = false;
        let key = keyboard.input::<Option<Key>, _>(
            self,
            code,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, modifiers, handle| {
                let sym = handle.modified_sym().raw();
                control_key = sym == keysyms::KEY_Control_L || sym == keysyms::KEY_Control_R;
                if palette {
                    return FilterResult::Intercept(menu_key(
                        pressed,
                        modifiers,
                        handle.modified_sym(),
                    ));
                }
                let plain = !modifiers.logo && !modifiers.ctrl && !modifiers.alt;
                if restartable && pressed && plain && sym == keysyms::KEY_Return {
                    return FilterResult::Intercept(Some(Key::Restart));
                }
                if !pressed || !modifiers.logo {
                    return FilterResult::Forward;
                }
                match super_key(sym, modifiers.shift) {
                    Some(key) => FilterResult::Intercept(Some(key)),
                    None => FilterResult::Forward,
                }
            },
        );
        let key = key.flatten();
        match (control_key, pressed) {
            (true, true) if palette => {}
            (true, true) => self.ctrl_pressed(),
            (true, false) => self.ctrl_released(),
            (false, true) => self.cancel_ctrl_timer(),
            (false, false) => {}
        }
        let code = code.raw();
        if !pressed && self.repeating.is_some_and(|(held, _)| held == code) {
            self.stop_repeat();
        }
        if pressed {
            self.stop_repeat();
            if let Some(key) = key.filter(|&key| repeats(key)) {
                self.start_repeat(code, key);
            }
        }
        if let Some(key) = key {
            if dismisses_popups(key) {
                self.dismiss_popups();
            }
            self.run_key(key);
        }
    }

    fn send_chord(&mut self, chord: Chord) {
        let terminal = self
            .layout()
            .pane(self.layout().focused)
            .and_then(|pane| pane.toplevel())
            .and_then(|toplevel| panes::app_id(toplevel.wl_surface()))
            .is_some_and(|app_id| app_id.starts_with("decade."));
        let keys: &[u32] = match (terminal, chord) {
            (true, Chord::Copy | Chord::Cut) => &[KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_C],
            (true, Chord::Paste) => &[KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V],
            (false, Chord::Copy) => &[KEY_LEFTCTRL, KEY_C],
            (false, Chord::Paste) => &[KEY_LEFTCTRL, KEY_V],
            (false, Chord::Cut) => &[KEY_LEFTCTRL, KEY_X],
        };
        let held_super: Vec<u32> = self
            .active
            .and_then(|id| self.viewers.get(&id))
            .map(|viewer| {
                viewer
                    .keys
                    .iter()
                    .copied()
                    .filter(|&key| key == KEY_LEFTMETA || key == KEY_RIGHTMETA)
                    .collect()
            })
            .unwrap_or_default();
        let time = self.now();
        for &key in &held_super {
            self.forward_key(key, KeyState::Released, time);
        }
        for &key in keys {
            self.forward_key(key, KeyState::Pressed, time);
        }
        for &key in keys.iter().rev() {
            self.forward_key(key, KeyState::Released, time);
        }
        for &key in &held_super {
            self.forward_key(key, KeyState::Pressed, time);
        }
    }

    /// Pass an evdev key to the focused client, past decade's bindings.
    pub(super) fn forward_key(&mut self, evdev: u32, state: KeyState, time: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.input::<(), _>(
            self,
            Keycode::new(evdev + 8),
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    fn start_repeat(&mut self, code: u32, key: Key) {
        if self.repeat.rate == 0 {
            return;
        }
        let interval = Duration::from_millis(1000 / self.repeat.rate as u64);
        let timer = Timer::from_duration(Duration::from_millis(self.repeat.delay as u64));
        let token = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.run_key(key);
            TimeoutAction::ToDuration(interval)
        });
        self.repeating = token.ok().map(|token| (code, token));
    }

    pub(super) fn stop_repeat(&mut self) {
        if let Some((_, token)) = self.repeating.take() {
            self.loop_handle.remove(token);
        }
    }

    fn run_key(&mut self, key: Key) {
        match key {
            Key::Clipboard(chord) => self.send_chord(chord),
            Key::Zoom => self.toggle_zoom(self.layout().focused),
            Key::Focus(direction) => {
                if let Some(id) = self.layout().neighbour(direction) {
                    self.focus(id);
                }
            }
            Key::Move(direction) => {
                let focused = self.layout().focused;
                if let Some(id) = self.layout().neighbour(direction) {
                    self.layout_mut().apply_drop(focused, Drop::Swap(id));
                    self.relayout();
                }
            }
            Key::Resize(direction) => {
                let focused = self.layout().focused;
                self.layout_mut()
                    .resize_toward(focused, direction, RESIZE_STEP);
                self.relayout();
            }
            Key::NewPane => self.new_terminal_pane(),
            Key::NewTab => self.new_tab(),
            Key::PreviousTab | Key::NextTab => {
                let shown = &self.workspaces[self.current()];
                let count = shown.tabs.len();
                let step = if key == Key::NextTab { 1 } else { count - 1 };
                self.show_tab((shown.active_tab + step) % count);
            }
            Key::NewPaneTab => self.new_pane_tab(),
            Key::PreviousPaneTab | Key::NextPaneTab => {
                let focused = self.layout().focused;
                if let Some(pane) = self.layout_mut().pane_mut(focused)
                    && pane.clients.len() > 1
                {
                    pane.step_tab(key == Key::NextPaneTab);
                    self.focus(focused);
                }
            }
            Key::Palette => self.open_palette(),
            Key::Sidebar => self.toggle_sidebar(),
            Key::NextNeeding => {
                if let Some(id) = self.frame(self.current()).next_needing() {
                    self.show_on_active(id);
                }
            }
            Key::Picker => self.open_picker(),
            Key::Restart => {
                let focused = self.layout().focused;
                self.restart_pane(self.current(), focused);
            }
            Key::Menu(key) => {
                let (ids, grid) = (self.workspace_ids(), self.picker_grid());
                let outcome = match &mut self.overlay {
                    Some(Overlay::Menu(menu)) => menu.key(key),
                    Some(Overlay::Context(menu)) => menu.key(key),
                    Some(Overlay::Rename(field)) => field.key(key),
                    Some(Overlay::Panel(panel)) => panel.key(key, &self.settings),
                    Some(Overlay::Picker { picker, .. }) => picker.key(key, &ids, &grid),
                    None => Outcome::Open,
                };
                self.act(outcome);
            }
        }
    }

    fn press_on_chrome(&mut self) -> bool {
        let (column, row) = self.pointer_cell();
        let bar = self
            .bar
            .then(|| self.layout().bar_hit(column, row))
            .flatten();
        match bar {
            Some((id, BarButton::Close)) => {
                self.close_pane(id);
                return true;
            }
            Some((id, BarButton::Zoom)) => {
                self.toggle_zoom(id);
                return true;
            }
            Some((id, BarButton::Pin)) => {
                self.toggle_pin(id);
                return true;
            }
            Some((id, BarButton::Scratch)) => {
                self.toggle_scratch(id);
                return true;
            }
            Some(_) => {}
            None => {
                if let Some((id, index)) = self.layout().tab_hit(column, row) {
                    self.show_pane_tab(id, index);
                    return true;
                }
            }
        }
        if let Some(boundary) = self.layout().boundary_at(column, row) {
            self.drag = Some(Drag::Boundary(boundary));
            return true;
        }
        let Some(id) = self.pane_under_pointer() else {
            return false;
        };
        let ctrl = self.seat.get_keyboard().unwrap().modifier_state().ctrl;
        if ctrl || bar.is_some() {
            self.start_pane_drag(id);
            return true;
        }
        if self
            .layout()
            .pane(id)
            .is_some_and(|pane| pane.restartable())
        {
            self.focus(id);
            self.restart_pane(self.current(), id);
            return true;
        }
        self.end_point_at(id);
        if id != self.layout().focused {
            self.focus(id);
        }
        false
    }

    /// The workspace, tab or pane whose name is under the pointer: on a
    /// card or a tile's popover, a tab's pill, or a shown pane bar.
    fn name_under_pointer(&self) -> Option<Named> {
        let (x, y) = self.pointer_px();
        let frame = self.frame(self.current());
        match frame.hit(x, y) {
            Some(Hit::Show(id)) => {
                let ((left, top), room) = frame.workspace_name_at(id)?;
                let width = room * self.text.chrome.cell_width as i32;
                let height = self.text.chrome.cell_height as i32;
                let on_name = (left..left + width).contains(&x) && (top..top + height).contains(&y);
                on_name.then_some(Named::Workspace(id))
            }
            Some(Hit::Tab(index)) => Some(Named::Tab(index)),
            Some(_) => None,
            None if self.bar => {
                let (column, row) = self.pointer_cell();
                match self.layout().bar_hit(column, row)? {
                    (id, BarButton::Name) => Some(Named::Pane(id)),
                    _ => None,
                }
            }
            None => None,
        }
    }

    /// Record a left press on `named`, and return whether it is the second
    /// press on the same name within `DOUBLE_CLICK`.
    fn double_click(&mut self, named: Option<Named>) -> bool {
        let now = Instant::now();
        let double = named.is_some()
            && self.last_name_press.is_some_and(|(at, before)| {
                Some(before) == named && now.duration_since(at) < DOUBLE_CLICK
            });
        self.last_name_press = named.filter(|_| !double).map(|named| (now, named));
        double
    }

    /// Whether a press goes to the client of a pane with a popup open, which
    /// closes its popup itself, rather than to decade's chrome or another
    /// pane.
    fn press_reaches_popup(&self, button: u32) -> bool {
        let ctrl = self.seat.get_keyboard().unwrap().modifier_state().ctrl;
        if button == BTN_LEFT && ctrl {
            return false;
        }
        let (x, y) = self.pointer_px();
        if self.frame(self.current()).hit(x, y).is_some() {
            return false;
        }
        let (column, row) = self.pointer_cell();
        let layout = self.layout();
        if (self.bar && layout.bar_hit(column, row).is_some())
            || layout.chrome_at(column, row).is_some()
        {
            return false;
        }
        layout
            .pane_at(column, row)
            .and_then(|id| layout.pane(id))
            .and_then(|pane| pane.toplevel())
            .is_some_and(|toplevel| popups_over(toplevel.wl_surface()).next().is_some())
    }

    /// Open the menu for the chrome under the pointer, and return whether
    /// the pointer was on chrome rather than a client.
    fn right_press_on_chrome(&mut self) -> bool {
        let (x, y) = self.pointer_px();
        let frame = self.frame(self.current());
        let place = match frame.hit(x, y) {
            Some(Hit::Show(id) | Hit::Clear(id)) => Some(Place::Workspace {
                id,
                clearable: frame.clearable(id),
                group: (self.workspace_index(id)).and_then(|index| self.workspaces[index].group),
            }),
            Some(Hit::Group(id)) => Some(Place::Group(id)),
            Some(Hit::Tab(index)) => Some(Place::Tab(index)),
            Some(Hit::New) => self.sidebar_open.then_some(Place::Sidebar),
            Some(_) => None,
            None => {
                let (column, row) = self.pointer_cell();
                let area = self.pane_area();
                let bar = (self.bar)
                    .then(|| self.layout().bar_hit(column, row))
                    .flatten()
                    .map(|(id, _)| id);
                if row < area.row {
                    None
                } else if column < area.column {
                    self.sidebar_open.then_some(Place::Sidebar)
                } else if let Some(id) = bar.or_else(|| self.layout().chrome_at(column, row)) {
                    self.pane_place(id)
                } else {
                    return false;
                }
            }
        };
        if let Some(place) = place {
            self.open_context_menu(place);
        }
        true
    }

    fn pane_place(&self, id: usize) -> Option<Place> {
        let shown = &self.workspaces[self.current()];
        let layout = shown.layout();
        let pane = layout.pane(id)?;
        Some(Place::Pane {
            id,
            pinned: pane.pinned,
            zoomed: layout.zoom == Some(id),
            scratch: shown.scratch_pane() == Some(pane.name.as_str()),
        })
    }

    fn start_pane_drag(&mut self, id: usize) {
        self.hover = None;
        self.tab_hover = None;
        self.focus(id);
        let pointer = self.pointer.to_physical(self.scale).to_i32_round();
        let grab = pointer - self.pane_origin(id);
        self.drag = Some(Drag::Pane {
            id,
            grab,
            start: Instant::now(),
            cell: None,
        });
        self.request_redraw();
    }

    fn drop(&mut self, drag: Drag) {
        let (id, cell) = match drag {
            Drag::Pane { id, cell, .. } => (id, cell),
            Drag::Sidebar {
                hit, moved: false, ..
            } => return self.frame_hit(hit),
            Drag::Sidebar { hit, .. } => return self.drop_in_sidebar(hit),
            Drag::Boundary(_) => return,
        };
        let drop = cell.and_then(|(column, row)| self.layout().drop_at(id, column, row));
        if let Some(drop) = drop {
            self.layout_mut().apply_drop(id, drop);
            self.relayout();
        } else {
            self.request_redraw();
        }
    }

    /// Move the card or header `hit` to where it was dragged in the open
    /// sidebar, or leave it where it was when it was dropped elsewhere.
    fn drop_in_sidebar(&mut self, hit: Hit) {
        let (column, _) = self.pointer_cell();
        let (_, y) = self.pointer_px();
        if column < self.pane_area().column {
            let frame = self.frame(self.current());
            match hit {
                Hit::Show(id) => self.move_to_group(id, frame.section_at(y)),
                Hit::Group(id) => self.move_group(id, frame.group_slot(y)),
                _ => {}
            }
        }
        self.frame_hover = None;
        self.request_redraw();
    }

    fn pane_origin(&self, id: usize) -> Point<i32, Physical> {
        let Some(pane) = self.layout().pane(id) else {
            return Point::default();
        };
        self.text.chrome.px_rect(pane.rect).loc
    }

    fn pointer_px(&self) -> (i32, i32) {
        let physical = self.pointer.to_physical(self.scale);
        (physical.x.max(0.0) as i32, physical.y.max(0.0) as i32)
    }

    fn pointer_cell(&self) -> (u32, u32) {
        let physical = self.pointer.to_physical(self.scale);
        (
            (physical.x.max(0.0) / self.text.chrome.cell_width as f64) as u32,
            (physical.y.max(0.0) / self.text.chrome.cell_height as f64) as u32,
        )
    }

    fn pane_under_pointer(&self) -> Option<usize> {
        let (column, row) = self.pointer_cell();
        self.layout().pane_at(column, row)
    }
}

pub(super) enum Drag {
    Boundary(Boundary),

    /// A press on a card or a group's header in the open sidebar.
    Sidebar {
        hit: Hit,

        /// Where the press was, in pixels.
        from: (i32, i32),

        /// Whether the pointer has gone far enough from `from` to make the
        /// press a drag rather than a click.
        moved: bool,
    },
    Pane {
        id: usize,
        grab: Point<i32, Physical>,
        start: Instant,

        /// The cell under the pointer since the drag began.
        cell: Option<(u32, u32)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Focus(Direction),
    Zoom,
    Move(Direction),
    Resize(Direction),
    NewPane,
    NewTab,
    PreviousTab,
    NextTab,
    NewPaneTab,
    PreviousPaneTab,
    NextPaneTab,
    Palette,
    Picker,
    Restart,
    Sidebar,
    NextNeeding,
    Clipboard(Chord),
    Menu(MenuKey),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chord {
    Copy,
    Paste,
    Cut,
}

#[derive(Clone, Copy)]
pub(super) struct Repeat {
    pub(super) delay: i32,
    pub(super) rate: i32,
}

fn repeats(key: Key) -> bool {
    match key {
        Key::Focus(_) | Key::Move(_) | Key::Resize(_) => true,
        Key::Menu(key) => !matches!(key, MenuKey::Enter | MenuKey::Escape),
        Key::Zoom
        | Key::NewPane
        | Key::NewTab
        | Key::PreviousTab
        | Key::NextTab
        | Key::NewPaneTab
        | Key::PreviousPaneTab
        | Key::NextPaneTab
        | Key::Palette
        | Key::Picker
        | Key::Restart
        | Key::Sidebar
        | Key::NextNeeding
        | Key::Clipboard(_) => false,
    }
}

/// Whether `key` dismisses open popups before it acts.
fn dismisses_popups(key: Key) -> bool {
    // an overlay is open only after its key dismissed the popups, and a
    // clipboard chord goes to the popup holding the keyboard
    !matches!(key, Key::Menu(_) | Key::Clipboard(_))
}

fn super_key(sym: u32, shift: bool) -> Option<Key> {
    let direction = match sym {
        keysyms::KEY_h | keysyms::KEY_H => Some(Direction::Left),
        keysyms::KEY_j | keysyms::KEY_J => Some(Direction::Down),
        keysyms::KEY_k | keysyms::KEY_K => Some(Direction::Up),
        keysyms::KEY_l | keysyms::KEY_L => Some(Direction::Right),
        _ => None,
    };
    if let Some(direction) = direction {
        return Some(if shift {
            Key::Move(direction)
        } else {
            Key::Focus(direction)
        });
    }
    match sym {
        keysyms::KEY_Left => Some(Key::Resize(Direction::Left)),
        keysyms::KEY_Right => Some(Key::Resize(Direction::Right)),
        keysyms::KEY_Up => Some(Key::Resize(Direction::Up)),
        keysyms::KEY_Down => Some(Key::Resize(Direction::Down)),
        keysyms::KEY_m if !shift => Some(Key::Zoom),
        keysyms::KEY_c if !shift => Some(Key::Clipboard(Chord::Copy)),
        keysyms::KEY_v if !shift => Some(Key::Clipboard(Chord::Paste)),
        keysyms::KEY_x if !shift => Some(Key::Clipboard(Chord::Cut)),
        keysyms::KEY_Return if shift => Some(Key::NewPane),
        keysyms::KEY_space if shift => Some(Key::Palette),
        keysyms::KEY_Tab if !shift => Some(Key::Picker),
        keysyms::KEY_b if !shift => Some(Key::Sidebar),
        keysyms::KEY_a if !shift => Some(Key::NextNeeding),
        keysyms::KEY_t if !shift => Some(Key::NewTab),
        keysyms::KEY_t | keysyms::KEY_T => Some(Key::NewPaneTab),
        keysyms::KEY_bracketleft if !shift => Some(Key::PreviousTab),
        keysyms::KEY_bracketright if !shift => Some(Key::NextTab),
        keysyms::KEY_bracketleft | keysyms::KEY_braceleft => Some(Key::PreviousPaneTab),
        keysyms::KEY_bracketright | keysyms::KEY_braceright => Some(Key::NextPaneTab),
        _ => None,
    }
}

/// The key an overlay takes, or `None` for one it swallows.
fn menu_key(pressed: bool, modifiers: &ModifiersState, sym: Keysym) -> Option<Key> {
    if !pressed {
        return None;
    }
    let ctrl = modifiers.ctrl;
    let key = match sym.raw() {
        keysyms::KEY_Escape => MenuKey::Escape,
        keysyms::KEY_space if modifiers.logo => MenuKey::Escape,
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => MenuKey::Enter,
        keysyms::KEY_BackSpace => MenuKey::Backspace,
        keysyms::KEY_Tab => MenuKey::Tab,
        keysyms::KEY_Up => MenuKey::Up,
        keysyms::KEY_Down => MenuKey::Down,
        keysyms::KEY_Left => MenuKey::Left,
        keysyms::KEY_Right => MenuKey::Right,
        keysyms::KEY_h if ctrl => MenuKey::Left,
        keysyms::KEY_l if ctrl => MenuKey::Right,
        keysyms::KEY_k if ctrl => MenuKey::Up,
        keysyms::KEY_j if ctrl => MenuKey::Down,
        _ if ctrl || modifiers.logo => return None,
        _ => MenuKey::Char(sym.key_char().filter(|ch| !ch.is_control())?),
    };
    Some(Key::Menu(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decade_keybind_dismisses_popups_but_a_clipboard_chord_or_overlay_key_does_not() {
        let focus = super_key(keysyms::KEY_h, false).unwrap();
        assert!(dismisses_popups(focus));
        assert!(dismisses_popups(Key::Palette));
        let paste = super_key(keysyms::KEY_v, false).unwrap();
        assert!(!dismisses_popups(paste));
        assert!(!dismisses_popups(Key::Menu(MenuKey::Escape)));
    }
}
