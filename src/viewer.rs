use std::collections::HashMap;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_registry, delegate_xdg_shell,
    delegate_xdg_window, registry_handlers,
};
use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_keyboard, wl_keyboard::WlKeyboard, wl_output, wl_pointer,
    wl_pointer::WlPointer, wl_seat, wl_seat::WlSeat, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::compositor::viewer_socket_path;
use crate::hypr::{self, Hyprland};
use crate::inhibit::Shortcuts;
use crate::wire::{Decoder, Format, Outbox, Plane, ServerMessage, ViewerMessage};

const PROTOCOL_VERSION: u32 = 1;
const LOCAL: u32 = 0;
const DEFAULT_SIZE: (u32, u32) = (1280, 800);
const START_TIMEOUT: Duration = Duration::from_secs(10);
const FENCE_TIMEOUT_MS: i32 = 500;

// xkb's eight real modifiers have fixed indices, Shift
// first; Caps Lock is Lock, and the standard keymaps
// bind Num Lock to Mod2
const CAPS_LOCK: u32 = 1 << 1;
const NUM_LOCK: u32 = 1 << 4;

// the names of cursor-shape-v1's version 1 shapes, in the
// order of its enum, which starts at 1
const SHAPES: [&str; 34] = [
    "default",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "e-resize",
    "n-resize",
    "ne-resize",
    "nw-resize",
    "s-resize",
    "se-resize",
    "sw-resize",
    "w-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "col-resize",
    "row-resize",
    "all-scroll",
    "zoom-in",
    "zoom-out",
];

pub fn run(class: String, server_args: Vec<String>) {
    let server = match connect_or_start(&server_args) {
        Ok(server) => server,
        Err(error) => fail(&error),
    };
    if let Err(error) = server.set_nonblocking(true) {
        fail(&format!(
            "the server socket would not go nonblocking: {error}"
        ));
    }
    let connection = Connection::connect_to_env()
        .unwrap_or_else(|error| fail(&format!("no Wayland display to open a window on: {error}")));
    let (globals, queue) = registry_queue_init::<Viewer>(&connection)
        .unwrap_or_else(|error| fail(&format!("the Wayland registry did not load: {error}")));
    let qh = queue.handle();
    let mut event_loop: EventLoop<Viewer> =
        EventLoop::try_new().unwrap_or_else(|error| fail(&error.to_string()));
    WaylandSource::new(connection.clone(), queue)
        .insert(event_loop.handle())
        .unwrap_or_else(|error| fail(&error.to_string()));

    event_loop
        .handle()
        .insert_source(
            Generic::new(
                server
                    .try_clone()
                    .unwrap_or_else(|error| fail(&error.to_string())),
                Interest::READ,
                Mode::Level,
            ),
            |_, _, viewer: &mut Viewer| {
                viewer.server_readable();
                Ok(PostAction::Continue)
            },
        )
        .unwrap_or_else(|error| fail(&error.to_string()));

    let mut viewer = Viewer::open(&globals, qh, class, server);
    viewer.set_shortcuts(true);
    viewer.watch_hyprland(&event_loop);

    let exit = loop {
        let timeout = (!viewer.outbox.is_empty()).then_some(Duration::from_millis(5));
        if let Err(error) = event_loop.dispatch(timeout, &mut viewer) {
            eprintln!("decade: the Wayland connection failed: {error}");
            break 1;
        }
        if let Err(error) = viewer.outbox.send(&viewer.server) {
            eprintln!("decade: writing to the server failed: {error}");
            break 1;
        }
        let _ = connection.flush();
        if let Some(code) = viewer.exit {
            break code;
        }
    };
    std::process::exit(exit);
}

fn fail(message: &str) -> ! {
    eprintln!("decade: {message}");
    std::process::exit(1);
}

fn missing(name: &str) -> ! {
    fail(&format!("the host has no {name}"))
}

fn connect_or_start(server_args: &[String]) -> Result<UnixStream, String> {
    let path = viewer_socket_path();
    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(stream);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = Command::new(exe)
        .arg("server")
        .args(server_args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("decade server did not start: {error}"))?;
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(stream) = UnixStream::connect(&path) {
            return Ok(stream);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("decade server exited before it answered: {status}"));
        }
        if start.elapsed() > START_TIMEOUT {
            return Err(format!(
                "decade server did not answer at {}",
                path.display()
            ));
        }
    }
}

fn wait_for(fence: &OwnedFd) {
    let mut fds = [PollFd::new(fence, PollFlags::IN)];
    while let Err(rustix::io::Errno::INTR) = rustix::event::poll(&mut fds, FENCE_TIMEOUT_MS) {}
}

fn shape(name: &str) -> Shape {
    SHAPES
        .iter()
        .position(|&shape| shape == name)
        .and_then(|index| Shape::try_from(index as u32 + 1).ok())
        .unwrap_or(Shape::Default)
}

struct Viewer {
    registry_state: RegistryState,
    output_state: OutputState,
    dmabuf_state: DmabufState,
    qh: QueueHandle<Viewer>,
    window: Window,
    viewport: WpViewport,

    server: UnixStream,
    decoder: Decoder,
    outbox: Outbox,

    /// The window's logical size.
    size: (u32, u32),

    /// The window's scale, in 120ths.
    scale: u32,
    configured: bool,

    /// The host's main device and the formats it imports for the window.
    formats: Option<(u64, Vec<Format>)>,

    /// The size and scale the server last heard, once `hello` is sent.
    sent: Option<(u32, u32, u32)>,
    welcomed: bool,
    buffers: HashMap<u32, Imported>,

    /// The size of the frame on screen.
    shown: Option<(u32, u32)>,

    seat: WlSeat,
    pointer: Option<WlPointer>,
    cursor_shape: Option<WpCursorShapeManagerV1>,
    shape_device: Option<WpCursorShapeDeviceV1>,
    enter_serial: Option<u32>,

    /// The time of the latest timed pointer event, for `pointer` on enter.
    pointer_time: u32,

    /// The cursor the server asked for, or None when hidden.
    cursor: Option<Shape>,

    /// The scroll on each axis in the current pointer frame, vertical first.
    scroll: [Scroll; 2],
    scroll_source: u32,

    keyboard: Option<WlKeyboard>,
    keys: Keys,

    shortcuts: Option<Shortcuts>,
    hyprland: Option<Hyprland>,
    exit: Option<i32>,
}

struct Imported {
    buffer: WlBuffer,
    params: ZwpLinuxBufferParamsV1,
    width: u32,
    height: u32,
}

impl Imported {
    fn destroy(self) {
        self.buffer.destroy();
        self.params.destroy();
    }
}

#[derive(Default)]
struct Scroll {
    time: u32,
    moved: bool,

    /// The distance in surface-local logical pixels.
    value: f64,
    steps: i32,
    stop: bool,
    inverted: bool,
}

#[derive(Default)]
struct Keys {
    focused: bool,

    /// Whether the server has had `keys` since the window gained focus.
    sent: bool,

    /// The evdev codes of the keys held down.
    held: Vec<u32>,
    locked: u32,
}

impl Viewer {
    fn open(
        globals: &GlobalList,
        qh: QueueHandle<Viewer>,
        class: String,
        server: UnixStream,
    ) -> Viewer {
        let compositor =
            CompositorState::bind(globals, &qh).unwrap_or_else(|_| missing("wl_compositor"));
        let xdg_shell = XdgShell::bind(globals, &qh).unwrap_or_else(|_| missing("xdg_wm_base"));
        let viewporter: WpViewporter = globals
            .bind(&qh, 1..=1, ())
            .unwrap_or_else(|_| missing("wp_viewporter"));
        let seat: WlSeat = globals
            .bind(&qh, 1..=9, ())
            .unwrap_or_else(|_| missing("wl_seat"));
        let dmabuf_state = DmabufState::new(globals, &qh);
        let fractional_scale: Option<WpFractionalScaleManagerV1> =
            globals.bind(&qh, 1..=1, ()).ok();
        let cursor_shape: Option<WpCursorShapeManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let shortcuts = globals
            .bind::<ZwpKeyboardShortcutsInhibitManagerV1, _, _>(&qh, 1..=1, ())
            .ok()
            .map(Shortcuts::new);

        let surface = compositor.create_surface(&qh);
        let viewport = viewporter.get_viewport(&surface, &qh, ());
        let _fractional_scale = fractional_scale
            .as_ref()
            .map(|manager| manager.get_fractional_scale(&surface, &qh, ()));
        let _feedback = dmabuf_state
            .get_surface_feedback(&surface, &qh)
            .unwrap_or_else(|_| missing("linux-dmabuf version 4"));
        let window = xdg_shell.create_window(surface, WindowDecorations::None, &qh);
        window.set_title("decade");
        window.set_app_id(class);
        window.commit();

        Viewer {
            registry_state: RegistryState::new(globals),
            output_state: OutputState::new(globals, &qh),
            dmabuf_state,
            qh,
            window,
            viewport,
            server,
            decoder: Decoder::new(),
            outbox: Outbox::new(),
            size: DEFAULT_SIZE,
            scale: 120,
            configured: false,
            formats: None,
            sent: None,
            welcomed: false,
            buffers: HashMap::new(),
            shown: None,
            seat,
            pointer: None,
            cursor_shape,
            shape_device: None,
            enter_serial: None,
            pointer_time: 0,
            cursor: Some(Shape::Default),
            scroll: [Scroll::default(), Scroll::default()],
            scroll_source: 0,
            keyboard: None,
            keys: Keys::default(),
            shortcuts,
            hyprland: Hyprland::find(),
            exit: None,
        }
    }

    fn send(&mut self, message: ViewerMessage) {
        if let Err(error) = self.outbox.push_viewer_message(message) {
            eprintln!("decade: a message to the server did not encode: {error}");
            self.exit = Some(1);
        }
    }

    fn input(&mut self, message: ViewerMessage) {
        if self.welcomed {
            self.send(message);
        }
    }

    fn hello(&mut self) {
        if self.sent.is_some() || !self.configured {
            return;
        }
        let Some((device, formats)) = self.formats.clone() else {
            return;
        };
        let (width, height) = self.size;
        self.send(ViewerMessage::Hello {
            version: PROTOCOL_VERSION,
            transport: LOCAL,
            width,
            height,
            scale: self.scale,
        });
        self.send(ViewerMessage::Formats { device, formats });
        self.sent = Some((width, height, self.scale));
    }

    fn sync_size(&mut self) {
        let now = (self.size.0, self.size.1, self.scale);
        if self.welcomed && self.sent != Some(now) {
            self.send(ViewerMessage::Resize {
                width: now.0,
                height: now.1,
                scale: now.2,
            });
            self.sent = Some(now);
        }
    }

    fn server_readable(&mut self) {
        match self.decoder.receive(&self.server) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("decade: the server closed the connection");
                self.exit = Some(1);
                return;
            }
            Err(error) => {
                eprintln!("decade: reading from the server failed: {error}");
                self.exit = Some(1);
                return;
            }
        }
        while self.exit.is_none() {
            match self.decoder.next_server_message() {
                Ok(Some(message)) => self.handle(message),
                Ok(None) => break,
                Err(error) => {
                    eprintln!("decade: the server sent a bad message: {error}");
                    self.send(ViewerMessage::Error {
                        code: 1,
                        message: error.to_string(),
                    });
                    self.exit = Some(1);
                }
            }
        }
    }

    fn handle(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Error { message, .. } => {
                eprintln!("decade: the server reported an error: {message}");
                self.exit = Some(1);
            }
            ServerMessage::Close { message, .. } => {
                eprintln!("decade: the server closed this viewer: {message}");
                self.exit = Some(0);
            }
            ServerMessage::Welcome { .. } => {
                self.welcomed = true;
                self.sync_size();
                self.send_keys();
            }
            ServerMessage::Buffer {
                id,
                width,
                height,
                format,
                modifier,
                planes,
                ..
            } => self.import(id, width, height, format, modifier, &planes),
            ServerMessage::Forget { id } => {
                if let Some(imported) = self.buffers.remove(&id) {
                    imported.destroy();
                }
            }
            ServerMessage::Frame { id, fence, .. } => self.show(id, fence),
            ServerMessage::CursorShape { shape: name } => self.set_cursor(Some(shape(&name))),
            ServerMessage::CursorImage { .. } | ServerMessage::Cursor { .. } => {
                self.set_cursor(Some(Shape::Default))
            }
            ServerMessage::CursorHidden => self.set_cursor(None),
            ServerMessage::Output { .. }
            | ServerMessage::Offer { .. }
            | ServerMessage::Cleared { .. }
            | ServerMessage::Request { .. }
            | ServerMessage::Workspaces { .. }
            | ServerMessage::Showing { .. } => {}
        }
    }

    fn import(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        format: u32,
        modifier: u64,
        planes: &[Plane],
    ) {
        let Ok(params) = self.dmabuf_state.create_params(&self.qh) else {
            self.send(ViewerMessage::BufferFailed { id });
            return;
        };
        for (index, plane) in planes.iter().enumerate() {
            params.add(
                plane.fd.as_fd(),
                index as u32,
                plane.offset,
                plane.stride,
                modifier,
            );
        }
        let (buffer, params) = params.create_immed(
            width as i32,
            height as i32,
            format,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.qh,
        );
        let imported = Imported {
            buffer,
            params,
            width,
            height,
        };
        if let Some(old) = self.buffers.insert(id, imported) {
            old.destroy();
        }
    }

    fn show(&mut self, id: u32, fence: Option<OwnedFd>) {
        let Some(imported) = self.buffers.get(&id) else {
            self.send(ViewerMessage::Release { id, fence: None });
            return;
        };
        if let Some(fence) = fence {
            wait_for(&fence);
        }
        let surface = self.window.wl_surface();
        surface.attach(Some(&imported.buffer), 0, 0);
        surface.damage_buffer(0, 0, imported.width as i32, imported.height as i32);
        self.viewport
            .set_destination(self.size.0 as i32, self.size.1 as i32);
        surface.frame(&self.qh, surface.clone());
        surface.commit();
        self.shown = Some((imported.width, imported.height));
    }

    fn set_cursor(&mut self, cursor: Option<Shape>) {
        self.cursor = cursor;
        self.apply_cursor();
    }

    fn apply_cursor(&self) {
        let (Some(pointer), Some(serial)) = (&self.pointer, self.enter_serial) else {
            return;
        };
        match (self.cursor, &self.shape_device) {
            (None, _) => pointer.set_cursor(serial, None, 0, 0),
            (Some(shape), Some(device)) => device.set_shape(serial, shape),
            (Some(_), None) => {}
        }
    }

    /// Convert a surface-local position to physical pixels of the frame on screen.
    fn to_frame(&self, x: f64, y: f64) -> (f64, f64) {
        let (width, height) = self.size;
        let physical = |logical: u32| (logical as u64 * self.scale as u64 + 60) / 120;
        let (frame_width, frame_height) = self
            .shown
            .map(|(w, h)| (w as u64, h as u64))
            .unwrap_or((physical(width), physical(height)));
        (
            x * frame_width as f64 / width.max(1) as f64,
            y * frame_height as f64 / height.max(1) as f64,
        )
    }

    fn send_keys(&mut self) {
        if !self.welcomed || !self.keys.focused || self.keys.sent {
            return;
        }
        self.keys.sent = true;
        self.send(ViewerMessage::Keys {
            locked: self.keys.locked,
            keycodes: self.keys.held.clone(),
        });
    }

    fn set_shortcuts(&mut self, on: bool) {
        if let Some(shortcuts) = &mut self.shortcuts {
            shortcuts.set(on, self.window.wl_surface(), &self.seat, &self.qh);
        }
    }

    fn recount_alone(&mut self) {
        if let Some(alone) = self.hyprland.as_ref().and_then(Hyprland::alone) {
            self.set_shortcuts(alone);
        }
    }

    fn watch_hyprland(&mut self, event_loop: &EventLoop<Viewer>) {
        let Some(hyprland) = &self.hyprland else {
            return;
        };
        let stream = match hyprland.events() {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("decade: Hyprland's event socket did not open: {error}");
                return;
            }
        };
        let mut buffer: Vec<u8> = Vec::new();
        let inserted = event_loop.handle().insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, viewer: &mut Viewer| {
                let mut chunk = [0u8; 4096];
                loop {
                    match (&**stream).read(&mut chunk) {
                        Ok(0) => return Ok(PostAction::Remove),
                        Ok(n) => {
                            buffer.extend_from_slice(&chunk[..n]);
                            let mut recount = false;
                            while let Some(end) = buffer.iter().position(|&b| b == b'\n') {
                                let line: Vec<u8> = buffer.drain(..=end).collect();
                                recount |= hypr::wants_recount(&String::from_utf8_lossy(&line));
                            }
                            if recount {
                                viewer.recount_alone();
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue);
                        }
                        Err(_) => return Ok(PostAction::Remove),
                    }
                }
            },
        );
        if inserted.is_ok() {
            self.recount_alone();
        }
    }

    fn pointer_event(&mut self, event: wl_pointer::Event) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                self.enter_serial = Some(serial);
                self.apply_cursor();
                let (x, y) = self.to_frame(surface_x, surface_y);
                let time = self.pointer_time;
                self.input(ViewerMessage::Pointer { time, x, y });
            }
            wl_pointer::Event::Leave { .. } => {
                self.enter_serial = None;
                self.input(ViewerMessage::PointerLeft);
            }
            wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            } => {
                self.pointer_time = time;
                let (x, y) = self.to_frame(surface_x, surface_y);
                self.input(ViewerMessage::Pointer { time, x, y });
            }
            wl_pointer::Event::Button {
                time,
                button,
                state,
                ..
            } => {
                self.pointer_time = time;
                let pressed = state == WEnum::Value(wl_pointer::ButtonState::Pressed);
                self.input(ViewerMessage::Button {
                    time,
                    button,
                    pressed,
                });
            }
            wl_pointer::Event::Axis { time, axis, value } => {
                if let Some(scroll) = self.axis(axis) {
                    scroll.time = time;
                    scroll.moved = true;
                    scroll.value += value;
                }
            }
            wl_pointer::Event::AxisSource {
                axis_source: WEnum::Value(source),
            } => self.scroll_source = source as u32,
            wl_pointer::Event::AxisStop { time, axis } => {
                if let Some(scroll) = self.axis(axis) {
                    scroll.time = time;
                    scroll.moved = true;
                    scroll.stop = true;
                }
            }
            wl_pointer::Event::AxisDiscrete { axis, discrete } => {
                if let Some(scroll) = self.axis(axis) {
                    scroll.steps += discrete * 120;
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                if let Some(scroll) = self.axis(axis) {
                    scroll.steps += value120;
                }
            }
            wl_pointer::Event::AxisRelativeDirection { axis, direction } => {
                if let Some(scroll) = self.axis(axis) {
                    scroll.inverted =
                        direction == WEnum::Value(wl_pointer::AxisRelativeDirection::Inverted);
                }
            }
            wl_pointer::Event::Frame => self.send_scroll(),
            _ => {}
        }
    }

    fn axis(&mut self, axis: WEnum<wl_pointer::Axis>) -> Option<&mut Scroll> {
        match axis {
            WEnum::Value(wl_pointer::Axis::VerticalScroll) => Some(&mut self.scroll[0]),
            WEnum::Value(wl_pointer::Axis::HorizontalScroll) => Some(&mut self.scroll[1]),
            _ => None,
        }
    }

    fn send_scroll(&mut self) {
        let (x_factor, y_factor) = self.to_frame(1.0, 1.0);
        let source = std::mem::take(&mut self.scroll_source);
        for axis in 0..2 {
            let scroll = std::mem::take(&mut self.scroll[axis]);
            if !scroll.moved {
                continue;
            }
            let factor = if axis == 0 { y_factor } else { x_factor };
            self.input(ViewerMessage::Scroll {
                time: scroll.time,
                source,
                axis: axis as u32,
                amount: scroll.value * factor,
                steps: scroll.steps,
                stop: scroll.stop,
                inverted: scroll.inverted,
            });
        }
    }

    fn keyboard_event(&mut self, event: wl_keyboard::Event) {
        match event {
            wl_keyboard::Event::Enter { keys, .. } => {
                self.keys.focused = true;
                self.keys.sent = false;
                self.keys.held = keys
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&key| u32::from_ne_bytes(key))
                    .collect();
            }
            wl_keyboard::Event::Leave { .. } => {
                if self.keys.sent {
                    self.input(ViewerMessage::KeysReleased);
                }
                self.keys = Keys {
                    locked: self.keys.locked,
                    ..Keys::default()
                };
            }
            wl_keyboard::Event::Modifiers { mods_locked, .. } => {
                self.keys.locked = (mods_locked & CAPS_LOCK != 0) as u32
                    | ((mods_locked & NUM_LOCK != 0) as u32) << 1;
                self.send_keys();
            }
            wl_keyboard::Event::Key {
                time, key, state, ..
            } => {
                self.send_keys();
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                if pressed {
                    self.keys.held.push(key);
                } else {
                    self.keys.held.retain(|&held| held != key);
                }
                self.input(ViewerMessage::Key {
                    time,
                    keycode: key,
                    pressed,
                });
            }
            _ => {}
        }
    }
}

impl CompositorHandler for Viewer {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {
        self.send(ViewerMessage::Ready);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for Viewer {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = Some(0);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        if let (Some(width), Some(height)) = configure.new_size {
            self.size = (width.get(), height.get());
        }
        self.configured = true;
        if self.shown.is_some() {
            self.viewport
                .set_destination(self.size.0 as i32, self.size.1 as i32);
        }
        window.commit();
        self.hello();
        self.sync_size();
    }
}

impl DmabufHandler for Viewer {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_feedback(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpLinuxDmabufFeedbackV1,
        feedback: DmabufFeedback,
    ) {
        let table = feedback.format_table();
        let mut indices: Vec<u16> = feedback
            .tranches()
            .iter()
            .flat_map(|tranche| tranche.formats.iter().copied())
            .collect();
        indices.sort_unstable();
        indices.dedup();
        let formats: Vec<Format> = indices
            .iter()
            .filter_map(|&index| table.get(index as usize))
            .map(|entry| Format {
                format: entry.format,
                modifier: entry.modifier,
            })
            .collect();
        let formats = Some((feedback.main_device(), formats));
        if formats == self.formats {
            return;
        }
        self.formats = formats;
        match (&self.sent, self.formats.clone()) {
            (Some(_), Some((device, formats))) => {
                self.send(ViewerMessage::Formats { device, formats })
            }
            _ => self.hello(),
        }
    }

    fn created(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpLinuxBufferParamsV1,
        _: WlBuffer,
    ) {
    }

    fn failed(&mut self, _: &Connection, _: &QueueHandle<Self>, params: &ZwpLinuxBufferParamsV1) {
        let failed = self
            .buffers
            .iter()
            .find(|(_, imported)| &imported.params == params)
            .map(|(&id, _)| id);
        if let Some(id) = failed {
            if let Some(imported) = self.buffers.remove(&id) {
                imported.destroy();
            }
            self.send(ViewerMessage::BufferFailed { id });
        }
    }

    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, buffer: &WlBuffer) {
        let released = self
            .buffers
            .iter()
            .find(|(_, imported)| &imported.buffer == buffer)
            .map(|(&id, _)| id);
        if let Some(id) = released {
            self.send(ViewerMessage::Release { id, fence: None });
        }
    }
}

impl OutputHandler for Viewer {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for Viewer {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

impl Dispatch<WlSeat, ()> for Viewer {
    fn event(
        viewer: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        let has_pointer = capabilities.contains(wl_seat::Capability::Pointer);
        if has_pointer && viewer.pointer.is_none() {
            let pointer = seat.get_pointer(qh, ());
            viewer.shape_device = viewer
                .cursor_shape
                .as_ref()
                .map(|manager| manager.get_pointer(&pointer, qh, ()));
            viewer.pointer = Some(pointer);
        } else if !has_pointer && let Some(pointer) = viewer.pointer.take() {
            if let Some(device) = viewer.shape_device.take() {
                device.destroy();
            }
            pointer.release();
        }
        let has_keyboard = capabilities.contains(wl_seat::Capability::Keyboard);
        if has_keyboard && viewer.keyboard.is_none() {
            viewer.keyboard = Some(seat.get_keyboard(qh, ()));
        } else if !has_keyboard && let Some(keyboard) = viewer.keyboard.take() {
            keyboard.release();
        }
    }
}

impl Dispatch<WlPointer, ()> for Viewer {
    fn event(
        viewer: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        viewer.pointer_event(event);
    }
}

impl Dispatch<WlKeyboard, ()> for Viewer {
    fn event(
        viewer: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        viewer.keyboard_event(event);
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for Viewer {
    fn event(
        viewer: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            viewer.scale = scale;
            viewer.sync_size();
        }
    }
}

macro_rules! no_events {
    ($($interface:ty),* $(,)?) => {
        $(
            impl Dispatch<$interface, ()> for Viewer {
                fn event(
                    _: &mut Self,
                    _: &$interface,
                    _: <$interface as Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

no_events!(
    WpViewporter,
    WpViewport,
    WpFractionalScaleManagerV1,
    WpCursorShapeManagerV1,
    WpCursorShapeDeviceV1,
    ZwpKeyboardShortcutsInhibitManagerV1,
    ZwpKeyboardShortcutsInhibitorV1,
);

delegate_compositor!(Viewer);
delegate_output!(Viewer);
delegate_registry!(Viewer);
delegate_xdg_shell!(Viewer);
delegate_xdg_window!(Viewer);
delegate_dmabuf!(Viewer);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_names_map_to_their_shapes_and_unknown_names_to_the_default() {
        assert_eq!(shape("default"), Shape::Default);
        assert_eq!(shape("text"), Shape::Text);
        assert_eq!(shape("zoom-out"), Shape::ZoomOut);
        assert_eq!(shape("no-such-cursor"), Shape::Default);
    }
}
