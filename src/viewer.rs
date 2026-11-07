use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::MemfdFlags;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{
    delegate_compositor, delegate_dmabuf, delegate_output, delegate_registry,
    delegate_subcompositor, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};
use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_keyboard, wl_keyboard::WlKeyboard, wl_output, wl_pointer,
    wl_pointer::WlPointer, wl_region::WlRegion, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_subsurface::WlSubsurface, wl_surface::WlSurface,
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
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::compositor::{ANOTHER_SERVER, viewer_socket_path};
use crate::hypr::{self, Hyprland};
use crate::inhibit::Shortcuts;
use crate::palette::BACKGROUND;
use crate::wire::{Decoder, Format, Outbox, Plane, ServerMessage, ViewerMessage};

const PROTOCOL_VERSION: u32 = 1;
const LOCAL: u32 = 0;
const DEFAULT_SIZE: (u32, u32) = (1280, 800);
const START_TIMEOUT: Duration = Duration::from_secs(10);

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
    let mut event_loop: EventLoop<'static, Viewer> =
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

    let mut viewer = Viewer::open(&globals, qh, event_loop.handle(), class, server);
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
    let log_path = path.with_file_name("server.log");
    let log = log_path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| OpenOptions::new().create(true).append(true).open(&log_path))
        .map_err(|error| format!("{}: {error}", log_path.display()))?;
    let log_out = log.try_clone().map_err(|error| error.to_string())?;
    let mut child = Command::new(exe)
        .arg("server")
        .args(server_args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log_out)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("decade server did not start: {error}"))?;
    let start = Instant::now();
    let mut lost = false;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(stream) = UnixStream::connect(&path) {
            std::thread::spawn(move || child.wait());
            return Ok(stream);
        }
        if !lost && let Ok(Some(status)) = child.try_wait() {
            if status.code() != Some(ANOTHER_SERVER) {
                return Err(format!("decade server exited before it answered: {status}"));
            }
            // the server holding the lock answers at the same path
            lost = true;
        }
        if start.elapsed() > START_TIMEOUT {
            return Err(format!(
                "decade server did not answer at {}",
                path.display()
            ));
        }
    }
}

/// A 1×1 buffer of the background colour, for the window behind the frame.
fn background(globals: &GlobalList, qh: &QueueHandle<Viewer>) -> WlBuffer {
    if let Ok(manager) = globals.bind::<WpSinglePixelBufferManagerV1, _, _>(qh, 1..=1, ()) {
        let [red, green, blue, alpha] = BACKGROUND.map(|c| (c as f64 * u32::MAX as f64) as u32);
        let buffer = manager.create_u32_rgba_buffer(red, green, blue, alpha, qh, ());
        manager.destroy();
        return buffer;
    }
    let shm: WlShm = globals
        .bind(qh, 1..=1, ())
        .unwrap_or_else(|_| missing("wl_shm"));
    let [red, green, blue, _] = BACKGROUND.map(|c| (c * 255.0).round() as u8);
    let mut file = rustix::fs::memfd_create("decade-background", MemfdFlags::CLOEXEC)
        .map(File::from)
        .unwrap_or_else(|error| fail(&format!("no memfd for the background: {error}")));
    // xrgb8888 is a little-endian u32, so blue comes first
    if let Err(error) = file.write_all(&[blue, green, red, 0xff]) {
        fail(&format!("the background did not write: {error}"));
    }
    let pool = shm.create_pool(file.as_fd(), 4, qh, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    buffer
}

/// Where a frame sits in the window.
struct Fit {
    /// The frame's size in physical pixels.
    frame: (u32, u32),

    /// The frame's place in the window, in logical pixels.
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Fit {
    /// Compute the fit of a frame in a window: the whole window when the frame
    /// is the window's physical size, else the frame scaled to fit and centred.
    fn new(frame: (u32, u32), window: (u32, u32), physical: (u32, u32)) -> Fit {
        let (width, height) = window;
        if frame == physical || frame.0 == 0 || frame.1 == 0 {
            return Fit {
                frame,
                x: 0,
                y: 0,
                width,
                height,
            };
        }
        let (frame_width, frame_height) = (frame.0 as u64, frame.1 as u64);
        let (window_width, window_height) = (width as u64, height as u64);
        let (fit_width, fit_height) = if frame_width * window_height >= frame_height * window_width
        {
            let fit_height = (frame_height * window_width + frame_width / 2) / frame_width;
            (window_width, fit_height)
        } else {
            let fit_width = (frame_width * window_height + frame_height / 2) / frame_height;
            (fit_width, window_height)
        };
        let (fit_width, fit_height) = (fit_width.max(1) as u32, fit_height.max(1) as u32);
        Fit {
            frame,
            x: (width as i32 - fit_width as i32) / 2,
            y: (height as i32 - fit_height as i32) / 2,
            width: fit_width,
            height: fit_height,
        }
    }

    /// The frame pixels per logical pixel, across and down.
    fn scale(&self) -> (f64, f64) {
        (
            self.frame.0 as f64 / self.width.max(1) as f64,
            self.frame.1 as f64 / self.height.max(1) as f64,
        )
    }

    /// Convert a window-local position to physical pixels of the frame.
    fn to_frame(&self, x: f64, y: f64) -> (f64, f64) {
        let (x_scale, y_scale) = self.scale();
        ((x - self.x as f64) * x_scale, (y - self.y as f64) * y_scale)
    }
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
    loop_handle: LoopHandle<'static, Viewer>,
    window: Window,

    /// The background buffer the window shows behind the frame.
    background: WlBuffer,
    window_viewport: WpViewport,

    /// The subsurface the frame is shown on, above the window.
    subsurface: WlSubsurface,
    frame_surface: WlSurface,
    frame_viewport: WpViewport,

    server: UnixStream,
    decoder: Decoder,
    outbox: Outbox,

    /// The window's logical size.
    size: (u32, u32),

    /// The window's scale, in 120ths.
    scale: u32,
    fractional: bool,
    configured: bool,

    /// The host's main device and the formats it imports for the window.
    formats: Option<(u64, Vec<Format>)>,

    /// The size and scale the server last heard, once `hello` is sent.
    sent: Option<(u32, u32, u32)>,
    welcomed: bool,
    buffers: HashMap<u32, Imported>,

    /// A frame not yet on screen.
    pending: Option<Pending>,

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
    /// The host's buffer, once it has created it.
    buffer: Option<WlBuffer>,
    params: ZwpLinuxBufferParamsV1,
    width: u32,
    height: u32,
}

struct Pending {
    id: u32,

    /// The source watching the frame's fence, until the fence signals.
    fence: Option<RegistrationToken>,
}

impl Imported {
    fn destroy(self) {
        if let Some(buffer) = self.buffer {
            buffer.destroy();
        }
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
        loop_handle: LoopHandle<'static, Viewer>,
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

        let subcompositor =
            SubcompositorState::bind(compositor.wl_compositor().clone(), globals, &qh)
                .unwrap_or_else(|_| missing("wl_subcompositor"));

        let surface = compositor.create_surface(&qh);
        let window_viewport = viewporter.get_viewport(&surface, &qh, ());
        let _fractional_scale = fractional_scale
            .as_ref()
            .map(|manager| manager.get_fractional_scale(&surface, &qh, ()));

        let (subsurface, frame_surface) = subcompositor.create_subsurface(surface.clone(), &qh);
        let frame_viewport = viewporter.get_viewport(&frame_surface, &qh, ());
        let _feedback = dmabuf_state
            .get_surface_feedback(&frame_surface, &qh)
            .unwrap_or_else(|_| missing("linux-dmabuf version 4"));

        // an empty input region passes the pointer through to
        // the window, so positions are always window-local
        let region = compositor.wl_compositor().create_region(&qh, ());
        frame_surface.set_input_region(Some(&region));
        region.destroy();

        let window = xdg_shell.create_window(surface, WindowDecorations::None, &qh);
        window.set_title("decade");
        window.set_app_id(class);
        window.commit();

        Viewer {
            registry_state: RegistryState::new(globals),
            output_state: OutputState::new(globals, &qh),
            dmabuf_state,
            background: background(globals, &qh),
            qh,
            loop_handle,
            window,
            window_viewport,
            subsurface,
            frame_surface,
            frame_viewport,
            server,
            decoder: Decoder::new(),
            outbox: Outbox::new(),
            size: DEFAULT_SIZE,
            scale: 120,
            fractional: fractional_scale.is_some(),
            configured: false,
            formats: None,
            sent: None,
            welcomed: false,
            buffers: HashMap::new(),
            pending: None,
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
            ServerMessage::Close { reason, message } => {
                if reason == 1 {
                    eprintln!("decade: the server is shutting down");
                } else {
                    eprintln!("decade: the server closed this viewer: {message}");
                }
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
            ServerMessage::Frame { id, fence, .. } => self.frame(id, fence),
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
        let params = params.create(
            width as i32,
            height as i32,
            format,
            zwp_linux_buffer_params_v1::Flags::empty(),
        );
        let imported = Imported {
            buffer: None,
            params,
            width,
            height,
        };
        if let Some(old) = self.buffers.insert(id, imported) {
            old.destroy();
        }
    }

    fn frame(&mut self, id: u32, fence: Option<OwnedFd>) {
        if !self.buffers.contains_key(&id) {
            self.send(ViewerMessage::Release { id, fence: None });
            return;
        }
        if let Some(old) = self.take_pending() {
            self.send(ViewerMessage::Release {
                id: old,
                fence: None,
            });
        }
        let fence = fence.and_then(|fence| self.watch(id, fence));
        self.pending = Some(Pending { id, fence });
        self.show_pending();
    }

    fn watch(&mut self, id: u32, fence: OwnedFd) -> Option<RegistrationToken> {
        let source = Generic::new(fence, Interest::READ, Mode::OneShot);
        let inserted = self
            .loop_handle
            .insert_source(source, move |_, _, viewer: &mut Viewer| {
                viewer.signalled(id);
                Ok(PostAction::Remove)
            });
        match inserted {
            Ok(token) => Some(token),
            Err(error) => {
                eprintln!("decade: a frame's fence could not be watched: {error}");
                None
            }
        }
    }

    fn signalled(&mut self, id: u32) {
        if let Some(pending) = &mut self.pending
            && pending.id == id
        {
            pending.fence = None;
            self.show_pending();
        }
    }

    /// Drop the pending frame, returning its buffer's id.
    fn take_pending(&mut self) -> Option<u32> {
        let pending = self.pending.take()?;
        if let Some(token) = pending.fence {
            self.loop_handle.remove(token);
        }
        Some(pending.id)
    }

    fn show_pending(&mut self) {
        let Some(Pending { id, fence: None }) = self.pending else {
            return;
        };
        let Some(imported) = self.buffers.get(&id) else {
            return;
        };
        let Some(buffer) = &imported.buffer else {
            return;
        };
        self.frame_surface.attach(Some(buffer), 0, 0);
        self.frame_surface
            .damage_buffer(0, 0, imported.width as i32, imported.height as i32);
        self.frame_surface
            .frame(&self.qh, self.frame_surface.clone());
        self.shown = Some((imported.width, imported.height));
        self.pending = None;
        self.place();
    }

    /// Place the frame in the window and commit both surfaces.
    fn place(&self) {
        let fit = self.fit();
        self.frame_viewport
            .set_destination(fit.width as i32, fit.height as i32);
        self.subsurface.set_position(fit.x, fit.y);
        self.frame_surface.commit();
        self.window.wl_surface().commit();
    }

    fn fit(&self) -> Fit {
        let physical = |logical: u32| ((logical as u64 * self.scale as u64 + 60) / 120) as u32;
        let physical = (physical(self.size.0), physical(self.size.1));
        Fit::new(self.shown.unwrap_or(physical), self.size, physical)
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
                let (x, y) = self.fit().to_frame(surface_x, surface_y);
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
                let (x, y) = self.fit().to_frame(surface_x, surface_y);
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
        let (x_factor, y_factor) = self.fit().scale();
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
        factor: i32,
    ) {
        if !self.fractional {
            self.scale = factor.max(1) as u32 * 120;
            self.sync_size();
        }
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
        if !self.configured {
            window.attach(Some(&self.background), 0, 0);
            self.configured = true;
        }
        self.window_viewport
            .set_destination(self.size.0 as i32, self.size.1 as i32);
        self.place();
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
        params: &ZwpLinuxBufferParamsV1,
        buffer: WlBuffer,
    ) {
        let Some((&id, imported)) = self
            .buffers
            .iter_mut()
            .find(|(_, imported)| &imported.params == params)
        else {
            buffer.destroy();
            return;
        };
        imported.buffer = Some(buffer);
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            self.show_pending();
        }
    }

    fn failed(&mut self, _: &Connection, _: &QueueHandle<Self>, params: &ZwpLinuxBufferParamsV1) {
        let failed = self
            .buffers
            .iter()
            .find(|(_, imported)| &imported.params == params)
            .map(|(&id, _)| id);
        let Some(id) = failed else {
            return;
        };
        if let Some(imported) = self.buffers.remove(&id) {
            imported.destroy();
        }
        self.send(ViewerMessage::BufferFailed { id });
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            self.take_pending();
            self.send(ViewerMessage::Ready);
        }
    }

    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, buffer: &WlBuffer) {
        let released = self
            .buffers
            .iter()
            .find(|(_, imported)| imported.buffer.as_ref() == Some(buffer))
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
    WlBuffer,
    WlRegion,
    WlShm,
    WlShmPool,
    WpSinglePixelBufferManagerV1,
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
delegate_subcompositor!(Viewer);

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

    #[test]
    fn a_frame_of_the_windows_physical_size_fills_the_window() {
        let fit = Fit::new((2400, 1500), (1601, 1000), (2400, 1500));
        assert_eq!((fit.x, fit.y, fit.width, fit.height), (0, 0, 1601, 1000));
        assert_eq!(fit.to_frame(1601.0, 1000.0).1, 1500.0);
    }

    #[test]
    fn a_wider_frame_is_letterboxed_above_and_below_and_centred() {
        let fit = Fit::new((2000, 500), (1000, 1000), (1000, 1000));
        assert_eq!((fit.x, fit.y, fit.width, fit.height), (0, 375, 1000, 250));
        assert_eq!(fit.to_frame(500.0, 500.0), (1000.0, 250.0));
        assert_eq!(fit.to_frame(0.0, 0.0), (0.0, -750.0));
    }

    #[test]
    fn a_taller_frame_is_pillarboxed_and_positions_map_through_the_same_fit() {
        let fit = Fit::new((500, 1000), (800, 400), (1600, 800));
        assert_eq!((fit.x, fit.y, fit.width, fit.height), (300, 0, 200, 400));
        assert_eq!(fit.to_frame(300.0, 400.0), (0.0, 1000.0));
        assert_eq!(fit.scale(), (2.5, 2.5));
    }
}
