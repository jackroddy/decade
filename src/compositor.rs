use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::io::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::{
    AbsolutePositionEvent, Axis as InputAxis, AxisSource, ButtonState, Event, InputEvent, KeyState,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{
    Color32F, Frame, ImportAll, ImportDma, ImportMem, Renderer, RendererSuper,
};
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend, WinitInput};
use smithay::input::keyboard::{FilterResult, keysyms};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::channel::{self as calloop_channel, Event as ChannelEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopHandle, LoopSignal, Mode as CalloopMode, PostAction, RegistrationToken,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat};
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::reexports::winit::platform::wayland::WindowAttributesExtWayland;
use smithay::reexports::winit::window::Window;
use smithay::utils::{
    Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, TraversalAction,
    with_states, with_surface_tree_downward,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::fractional_scale::{
    FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_output, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_shell,
    render_elements,
};

use crate::channel::{Host, Out, Router};
use crate::chrome;
use crate::grid::Grid;
use crate::mcp;
use crate::panes::{
    BACKGROUND, BarButton, Boundary, Direction, FOREGROUND, Kind as PaneKind, Layout, Pane,
};
use crate::text::{FONT_PX, Font, Glyphs};

const REFRESH_MILLIHERTZ: i32 = 60_000;
const AGENT_CLIENT: usize = usize::MAX;
const BAR_HOLD: Duration = Duration::from_millis(50);
const BTN_LEFT: u32 = 0x110;
const GHOST_SCALE: f64 = 0.2;
const GHOST_ALPHA: f32 = 0.8;
const GRAB_ANIMATION: Duration = Duration::from_millis(100);

enum Drag {
    Boundary(Boundary),
    Pane {
        id: usize,
        grab: Point<i32, Physical>,
        start: Instant,
    },
}

render_elements! {
    Element<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Chrome = TextureRenderElement<<R as RendererSuper>::TextureId>,
    Ghost = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    GhostChrome = RescaleRenderElement<TextureRenderElement<<R as RendererSuper>::TextureId>>,
}

enum Key {
    Focus(Direction),
    NewPane,
    Palette,
    Escape,
    Swallow,
}

struct ChildEnv {
    wayland: OsString,
    channel: PathBuf,
    mcp: Option<String>,
}

pub struct State {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<State>,
    data_device_state: DataDeviceState,
    dmabuf_state: DmabufState,
    _fractional_scale_state: FractionalScaleManagerState,
    _viewporter_state: ViewporterState,
    seat: Seat<State>,
    output: Output,
    backend: WinitGraphicsBackend<GlesRenderer>,
    display_handle: DisplayHandle,
    loop_signal: LoopSignal,
    loop_handle: LoopHandle<'static, State>,
    router: Router,
    sockets: HashMap<usize, UnixStream>,
    next_client: usize,
    env: ChildEnv,
    children: Vec<Child>,
    font: Font,
    glyphs: Glyphs,
    layout: Layout,
    unplaced: Vec<ToplevelSurface>,
    chrome: Option<(GlesTexture, Grid, Vec<u8>)>,
    overlay: Option<GlesTexture>,
    bars: Option<(GlesTexture, Grid)>,
    ghost: Option<(GlesTexture, bool)>,
    pointer: Point<f64, Logical>,
    drag: Option<Drag>,
    ctrl_timer: Option<RegistrationToken>,
    scale: f64,
    start: Instant,
}

pub struct Commands {
    pub chat: Option<Vec<String>>,
    pub code: Option<Vec<String>>,
    pub class: String,
}

pub fn run(commands: Commands) {
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
    let display: Display<State> = Display::new().unwrap();
    let dh = display.handle();

    let attributes = Window::default_attributes()
        .with_title("decade")
        .with_name(&commands.class, &commands.class);
    let (mut backend, winit) = winit::init_from_attributes::<GlesRenderer>(attributes).unwrap();

    let mut dmabuf_state = DmabufState::new();
    let formats = backend.renderer().dmabuf_formats();
    let _dmabuf_global = dmabuf_state.create_global::<State>(&dh, formats);

    let size = backend.window_size();
    let scale = backend.window().scale_factor();
    let mode = Mode {
        size,
        refresh: REFRESH_MILLIHERTZ,
    };
    let output = Output::new(
        "decade".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "decade".into(),
            model: "nested".into(),
        },
    );
    let _output_global = output.create_global::<State>(&dh);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Fractional(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "decade");
    seat.add_keyboard(Default::default(), 200, 25).unwrap();
    seat.add_pointer();

    let font = Font::load(FONT_PX * scale as f32);
    let layout = Layout::new(
        size.w as u32 / font.cell_width,
        size.h as u32 / font.cell_height,
    );

    let handle = event_loop.handle();
    let socket = ListeningSocketSource::new_auto().unwrap();
    let socket_name = socket.socket_name().to_os_string();
    handle
        .insert_source(socket, |stream, _, state| {
            state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        })
        .unwrap();
    handle
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                unsafe { display.get_mut().dispatch_clients(state).unwrap() };
                Ok(PostAction::Continue)
            },
        )
        .unwrap();
    handle
        .insert_source(winit, |event, _, state| match event {
            WinitEvent::Resized { size, scale_factor } => {
                state.resized(size.w as u32, size.h as u32, scale_factor)
            }
            WinitEvent::Input(event) => state.input(event),
            WinitEvent::Redraw => state.redraw(),
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Focus(_) => {}
        })
        .unwrap();

    let channel_path = channel_path();
    let _ = std::fs::remove_file(&channel_path);
    let listener = UnixListener::bind(&channel_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, CalloopMode::Level),
            |_, listener, state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => state.channel_connected(stream),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            eprintln!("decade: accepting a channel client failed: {error}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .unwrap();

    let mut router = Router::new(layout.panes.iter().map(|pane| pane.name.clone()).collect());
    router.connected(AGENT_CLIENT);
    router.handle(AGENT_CLIENT, r#"{"hello":{"name":"agent"}}"#);
    let (mcp_requests, mcp_source) = calloop_channel::channel();
    handle
        .insert_source(mcp_source, |event, _, state| {
            if let ChannelEvent::Msg(request) = event {
                state.mcp_request(request);
            }
        })
        .unwrap();
    let mcp = match mcp::start(mcp_requests) {
        Ok(server) => Some(server.url),
        Err(error) => {
            eprintln!("decade: the MCP server did not start: {error}");
            None
        }
    };
    let mut state = State {
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        shm_state: ShmState::new::<State>(&dh, vec![]),
        seat_state,
        data_device_state: DataDeviceState::new::<State>(&dh),
        dmabuf_state,
        _fractional_scale_state: FractionalScaleManagerState::new::<State>(&dh),
        _viewporter_state: ViewporterState::new::<State>(&dh),
        seat,
        output,
        backend,
        display_handle: dh,
        loop_signal: event_loop.get_signal(),
        loop_handle: handle.clone(),
        router,
        sockets: HashMap::new(),
        next_client: 0,
        env: ChildEnv {
            wayland: socket_name,
            channel: channel_path,
            mcp,
        },
        children: Vec::new(),
        font,
        glyphs: Glyphs::new(),
        layout,
        unplaced: Vec::new(),
        chrome: None,
        overlay: None,
        bars: None,
        ghost: None,
        pointer: (0.0, 0.0).into(),
        drag: None,
        ctrl_timer: None,
        scale,
        start: Instant::now(),
    };

    state.spawn_terminal("terminal", &[]);
    if let Some(chat) = &commands.chat {
        state.spawn_terminal("chat", chat);
    }
    if let Some(code) = &commands.code {
        state.spawn_terminal("code", code);
    }
    state.backend.window().request_redraw();
    event_loop
        .run(None, &mut state, |state| {
            state.display_handle.flush_clients().unwrap();
        })
        .unwrap();

    for child in &mut state.children {
        let _ = child.kill();
    }
    let _ = std::fs::remove_file(&state.env.channel);
}

fn channel_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    remove_stale_sockets(&dir);
    dir.join(format!("decade-{}.sock", std::process::id()))
}

fn remove_stale_sockets(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix("decade-"))
            .and_then(|rest| rest.strip_suffix(".sock"))
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl State {
    fn spawn_terminal(&mut self, pane: &str, command: &[String]) {
        let mut ghostty = Command::new("ghostty");
        ghostty
            .env("WAYLAND_DISPLAY", &self.env.wayland)
            .env("DECADE_SOCKET", &self.env.channel)
            .env("DECADE_PANE", pane);
        if let Some(mcp) = &self.env.mcp {
            ghostty.env("DECADE_MCP", mcp);
        }
        ghostty
            .env_remove("DISPLAY")
            .args(["--window-decoration=none", "--gtk-titlebar=false"])
            .arg(format!("--class=decade.{pane}"));
        if !command.is_empty() {
            ghostty.arg("-e").args(command);
        }
        match ghostty.spawn() {
            Ok(child) => self.children.push(child),
            Err(error) => eprintln!("decade: could not start ghostty: {error}"),
        }
    }

    fn channel_connected(&mut self, stream: UnixStream) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(writer) = stream.try_clone() else {
            return;
        };
        let id = self.next_client;
        self.next_client += 1;
        self.router.connected(id);
        self.sockets.insert(id, writer);
        let mut buffer: Vec<u8> = Vec::new();
        let inserted = self.loop_handle.insert_source(
            Generic::new(stream, Interest::READ, CalloopMode::Level),
            move |_, stream, state| {
                let mut chunk = [0u8; 4096];
                loop {
                    match (&**stream).read(&mut chunk) {
                        Ok(0) => {
                            state.channel_disconnected(id);
                            return Ok(PostAction::Remove);
                        }
                        Ok(n) => {
                            buffer.extend_from_slice(&chunk[..n]);
                            while let Some(end) = buffer.iter().position(|&b| b == b'\n') {
                                let line: Vec<u8> = buffer.drain(..=end).collect();
                                let line = String::from_utf8_lossy(&line[..end]);
                                if !line.trim().is_empty() {
                                    state.channel_line(id, &line);
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue);
                        }
                        Err(_) => {
                            state.channel_disconnected(id);
                            return Ok(PostAction::Remove);
                        }
                    }
                }
            },
        );
        if inserted.is_err() {
            self.channel_disconnected(id);
        }
    }

    fn mcp_request(&mut self, request: mcp::Request) {
        let mut reply = Vec::new();
        for out in self.router.handle(AGENT_CLIENT, &request.line) {
            match out {
                Out::Line(to, text) if to == AGENT_CLIENT => reply.push(text),
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Host(Host::Focus(pane)) => self.focus_named(&pane),
            }
        }
        let _ = request.reply.send(reply);
    }

    fn channel_disconnected(&mut self, id: usize) {
        self.router.disconnected(id);
        self.sockets.remove(&id);
    }

    fn channel_line(&mut self, id: usize, line: &str) {
        for out in self.router.handle(id, line) {
            match out {
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Host(Host::Focus(pane)) => self.focus_named(&pane),
            }
        }
    }

    fn channel_send(&mut self, to: usize, text: &str) {
        let Some(stream) = self.sockets.get(&to) else {
            return;
        };
        let mut stream = stream;
        if stream
            .write_all(text.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .is_err()
        {
            self.channel_disconnected(to);
        }
    }

    fn resized(&mut self, width: u32, height: u32, scale: f64) {
        let rescaled = scale != self.scale;
        if rescaled {
            self.scale = scale;
            self.font = Font::load(FONT_PX * scale as f32);
            self.glyphs = Glyphs::new();
            for pane in &self.layout.panes {
                if let Some(toplevel) = &pane.toplevel {
                    self.send_scale(toplevel.wl_surface());
                }
            }
        }
        self.layout
            .resize(width / self.font.cell_width, height / self.font.cell_height);
        self.output.change_current_state(
            Some(Mode {
                size: (width as i32, height as i32).into(),
                refresh: REFRESH_MILLIHERTZ,
            }),
            None,
            rescaled.then_some(Scale::Fractional(scale)),
            None,
        );
        self.relayout();
    }

    fn relayout(&mut self) {
        self.overlay = None;
        let ids: Vec<usize> = self.layout.panes.iter().map(|pane| pane.id).collect();
        for id in ids {
            self.configure(id);
        }
        self.backend.window().request_redraw();
    }

    fn configure(&mut self, id: usize) {
        let Some(pane) = self.layout.pane(id) else {
            return;
        };
        let Some(toplevel) = &pane.toplevel else {
            return;
        };
        let (_, size) = self.client_geometry(pane);
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let focused = self.layout.focused == id;
        toplevel.with_pending_state(|state| {
            state.size = Some(size);
            if focused {
                state.states.set(xdg_toplevel::State::Activated);
            } else {
                state.states.unset(xdg_toplevel::State::Activated);
            }
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        } else {
            toplevel.send_configure();
        }
    }

    fn focus(&mut self, id: usize) {
        if self.layout.pane(id).is_none() {
            return;
        }
        let previous = self.layout.focused;
        self.layout.focused = id;
        let surface = self
            .layout
            .pane(id)
            .and_then(|pane| pane.toplevel.as_ref())
            .map(|toplevel| toplevel.wl_surface().clone());
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        self.configure(previous);
        self.configure(id);
        self.chrome_changed();
    }

    fn chrome_changed(&mut self) {
        self.backend.window().request_redraw();
    }

    fn focus_named(&mut self, name: &str) {
        if let Some(id) = self.layout.by_name(name) {
            self.focus(id);
        }
    }

    fn new_terminal_pane(&mut self) {
        let name = self.layout.unique_name("terminal");
        let id = self.layout.add_transient(name.clone());
        self.router.add_pane(name.clone());
        self.spawn_terminal(&name, &[]);
        self.relayout();
        self.focus(id);
    }

    fn remove_pane(&mut self, id: usize) {
        let Some(pane) = self.layout.pane(id) else {
            return;
        };
        let name = pane.name.clone();
        self.layout.remove(id);
        self.router.remove_pane(&name);
        self.relayout();
        let focused = self.layout.focused;
        self.focus(focused);
    }

    fn close_pane(&mut self, id: usize) {
        let Some(pane) = self.layout.pane(id) else {
            return;
        };
        match (&pane.toplevel, &pane.kind) {
            (Some(toplevel), _) => toplevel.send_close(),
            (None, PaneKind::Transient) => self.remove_pane(id),
            (None, PaneKind::Anchor { .. }) => {}
        }
    }

    fn show_bar(&mut self, show: bool) {
        if self.layout.bar != show {
            self.layout.bar = show;
            self.backend.window().request_redraw();
        }
    }

    fn cancel_ctrl_timer(&mut self) {
        if let Some(token) = self.ctrl_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    fn ctrl_pressed(&mut self) {
        self.cancel_ctrl_timer();
        let timer = Timer::from_duration(BAR_HOLD);
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

    fn input(&mut self, event: InputEvent<WinitInput>) {
        match event {
            InputEvent::Keyboard { event } => self.keyboard(event),
            InputEvent::PointerMotionAbsolute { event } => {
                let size = self.backend.window_size().to_f64().to_logical(self.scale);
                self.pointer = event.position_transformed(size.to_i32_round());
                match self.drag.take() {
                    Some(Drag::Boundary(boundary)) => {
                        let (column, row) = self.pointer_cell();
                        self.layout.drag_boundary(&boundary, column, row);
                        self.drag = Some(Drag::Boundary(boundary));
                        self.relayout();
                        return;
                    }
                    Some(Drag::Pane { id, grab, start }) => {
                        let target = self.pane_under_pointer().filter(|&target| target != id);
                        self.layout.drop_target = target;
                        self.drag = Some(Drag::Pane { id, grab, start });
                        self.backend.window().request_redraw();
                        return;
                    }
                    None => {}
                }
                let pointer = self.seat.get_pointer().unwrap();
                let focus = self.pane_under_pointer().and_then(|id| {
                    let pane = self.layout.pane(id)?;
                    let (origin, _) = self.client_geometry(pane);
                    pane.toplevel
                        .as_ref()
                        .map(|toplevel| (toplevel.wl_surface().clone(), origin.to_f64()))
                });
                pointer.motion(
                    self,
                    focus,
                    &MotionEvent {
                        location: self.pointer,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event } => {
                let pressed = event.state() == ButtonState::Pressed;
                if event.button_code() == BTN_LEFT {
                    if pressed && self.press_on_chrome() {
                        return;
                    }
                    if !pressed && let Some(drag) = self.drag.take() {
                        self.drop(drag);
                        return;
                    }
                }
                let pointer = self.seat.get_pointer().unwrap();
                pointer.button(
                    self,
                    &ButtonEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        button: event.button_code(),
                        state: event.state(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let pointer = self.seat.get_pointer().unwrap();
                let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
                for axis in [InputAxis::Horizontal, InputAxis::Vertical] {
                    let amount = event
                        .amount(axis)
                        .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(v120) = event.amount_v120(axis) {
                            frame = frame.v120(axis, v120 as i32);
                        }
                    } else if event.source() == AxisSource::Finger {
                        frame = frame.stop(axis);
                    }
                }
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    fn keyboard(
        &mut self,
        event: <WinitInput as smithay::backend::input::InputBackend>::KeyboardKeyEvent,
    ) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let pressed = event.state() == KeyState::Pressed;
        let palette = self.layout.palette;
        let mut control_key = false;
        let key = keyboard.input::<Key, _>(
            self,
            event.key_code(),
            event.state(),
            SERIAL_COUNTER.next_serial(),
            event.time_msec(),
            |_, modifiers, handle| {
                let sym = handle.modified_sym().raw();
                control_key = sym == keysyms::KEY_Control_L || sym == keysyms::KEY_Control_R;
                if palette {
                    return if pressed && sym == keysyms::KEY_Escape {
                        FilterResult::Intercept(Key::Escape)
                    } else {
                        FilterResult::Intercept(Key::Swallow)
                    };
                }
                if !pressed || !modifiers.ctrl {
                    return FilterResult::Forward;
                }
                match sym {
                    keysyms::KEY_h => FilterResult::Intercept(Key::Focus(Direction::Left)),
                    keysyms::KEY_j => FilterResult::Intercept(Key::Focus(Direction::Down)),
                    keysyms::KEY_k => FilterResult::Intercept(Key::Focus(Direction::Up)),
                    keysyms::KEY_l => FilterResult::Intercept(Key::Focus(Direction::Right)),
                    keysyms::KEY_space => FilterResult::Intercept(Key::Palette),
                    keysyms::KEY_Return if modifiers.shift => FilterResult::Intercept(Key::NewPane),
                    _ => FilterResult::Forward,
                }
            },
        );
        match (control_key, pressed) {
            (true, true) => self.ctrl_pressed(),
            (true, false) => self.ctrl_released(),
            (false, true) => self.cancel_ctrl_timer(),
            (false, false) => {}
        }
        match key {
            Some(Key::Focus(direction)) => {
                if let Some(id) = self.layout.neighbour(direction) {
                    self.focus(id);
                }
            }
            Some(Key::NewPane) => self.new_terminal_pane(),
            Some(Key::Palette) => {
                self.layout.palette = true;
                self.overlay = None;
                self.backend.window().request_redraw();
            }
            Some(Key::Escape) => {
                self.layout.palette = false;
                self.overlay = None;
                self.backend.window().request_redraw();
            }
            Some(Key::Swallow) | None => {}
        }
    }

    fn press_on_chrome(&mut self) -> bool {
        let (column, row) = self.pointer_cell();
        let bar = self.layout.bar_hit(column, row);
        match bar {
            Some((id, BarButton::Close)) => {
                self.close_pane(id);
                return true;
            }
            Some((id, BarButton::Zoom)) => {
                self.layout.toggle_zoom(id);
                self.relayout();
                return true;
            }
            _ => {}
        }
        if let Some(boundary) = self.layout.boundary_at(column, row) {
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
        if id != self.layout.focused {
            self.focus(id);
        }
        false
    }

    fn start_pane_drag(&mut self, id: usize) {
        self.focus(id);
        let pointer = self.pointer.to_physical(self.scale).to_i32_round();
        let grab = pointer - self.pane_origin(id);
        self.drag = Some(Drag::Pane {
            id,
            grab,
            start: Instant::now(),
        });
        self.layout.dragging = Some(id);
        self.backend.window().request_redraw();
    }

    fn drop(&mut self, drag: Drag) {
        let Drag::Pane { id, .. } = drag else {
            return;
        };
        self.layout.dragging = None;
        self.ghost = None;
        if let Some(target) = self.layout.drop_target.take() {
            self.layout.swap(id, target);
            self.relayout();
        } else {
            self.backend.window().request_redraw();
        }
    }

    fn pane_origin(&self, id: usize) -> Point<i32, Physical> {
        let Some(pane) = self.layout.pane(id) else {
            return Point::default();
        };
        let rect = pane.rect;
        (
            (rect.column * self.font.cell_width) as i32,
            (rect.row * self.font.cell_height) as i32,
        )
            .into()
    }

    fn pointer_cell(&self) -> (u32, u32) {
        let physical = self.pointer.to_physical(self.scale);
        (
            (physical.x.max(0.0) / self.font.cell_width as f64) as u32,
            (physical.y.max(0.0) / self.font.cell_height as f64) as u32,
        )
    }

    fn pane_under_pointer(&self) -> Option<usize> {
        let (column, row) = self.pointer_cell();
        self.layout.pane_at(column, row)
    }

    fn client_geometry(&self, pane: &Pane) -> (Point<i32, Logical>, Size<i32, Logical>) {
        let rect = self.layout.client_rect(pane);
        let origin: Point<i32, Physical> = (
            (rect.column * self.font.cell_width) as i32,
            (rect.row * self.font.cell_height) as i32,
        )
            .into();
        let size: Size<i32, Physical> = (
            (rect.columns * self.font.cell_width) as i32,
            (rect.rows * self.font.cell_height) as i32,
        )
            .into();
        (
            origin.to_f64().to_logical(self.scale).to_i32_round(),
            size.to_f64().to_logical(self.scale).to_i32_round(),
        )
    }

    fn send_scale(&self, surface: &WlSurface) {
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(self.scale);
            });
        });
    }

    fn redraw(&mut self) {
        self.render();
        self.display_handle.flush_clients().unwrap();
        let size = self.backend.window_size();
        self.backend
            .submit(Some(&[Rectangle::from_size(size)]))
            .unwrap();
    }

    fn render(&mut self) {
        let State {
            backend,
            layout,
            chrome,
            overlay,
            bars,
            font,
            glyphs,
            start,
            scale,
            drag,
            pointer,
            ghost,
            ..
        } = self;
        let scale = *scale;
        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        let (renderer, mut framebuffer) = backend.bind().unwrap();

        let texture_size = (
            (layout.columns * font.cell_width) as i32,
            (layout.rows * font.cell_height) as i32,
        );
        let mut grid = Grid::new(layout.columns, layout.rows, FOREGROUND, BACKGROUND);
        layout.draw(&mut grid);
        match chrome {
            Some((texture, previous, pixels))
                if previous.columns == grid.columns && previous.rows == grid.rows =>
            {
                patch_chrome(renderer, texture, pixels, previous, &grid, font, glyphs);
                *previous = grid;
            }
            _ => {
                let pixels = chrome::raster(&grid, font, glyphs);
                *chrome = renderer
                    .import_memory(&pixels, Fourcc::Abgr8888, texture_size.into(), false)
                    .ok()
                    .map(|texture| (texture, grid, pixels));
            }
        }
        if overlay.is_none() && layout.palette {
            let mut grid = crate::panes::overlay_grid(layout.columns, layout.rows);
            layout.draw_overlay(&mut grid);
            let pixels = chrome::raster(&grid, font, glyphs);
            *overlay = renderer
                .import_memory(&pixels, Fourcc::Abgr8888, texture_size.into(), false)
                .ok();
        }
        if layout.bar {
            let mut grid = crate::panes::overlay_grid(layout.columns, layout.rows);
            layout.draw_bars(&mut grid);
            let unchanged = bars.as_ref().is_some_and(|(_, previous)| {
                previous.columns == grid.columns && previous.cells == grid.cells
            });
            if !unchanged {
                let pixels = chrome::raster(&grid, font, glyphs);
                *bars = renderer
                    .import_memory(&pixels, Fourcc::Abgr8888, texture_size.into(), false)
                    .ok()
                    .map(|texture| (texture, grid));
            }
        } else {
            *bars = None;
        }
        let texture_element = |renderer: &GlesRenderer, texture: &GlesTexture| {
            Element::Chrome(TextureRenderElement::from_static_texture(
                Id::new(),
                renderer.context_id(),
                (0.0, 0.0),
                texture.clone(),
                1,
                Transform::Normal,
                None,
                Some(Rectangle::from_size(size.to_f64().to_logical(1.0))),
                Some(size.to_f64().to_logical(scale).to_i32_round()),
                None,
                Kind::Unspecified,
            ))
        };

        let mut elements: Vec<Element<GlesRenderer>> = Vec::new();
        let mut animating = false;
        let dragged = match drag {
            Some(Drag::Pane { id, grab, start }) => {
                layout.pane(*id).map(|pane| (pane, *grab, *start))
            }
            _ => None,
        };
        if let Some((pane, grab, start)) = dragged {
            let t = (start.elapsed().as_secs_f64() / GRAB_ANIMATION.as_secs_f64()).min(1.0);
            animating = t < 1.0;
            let eased = 1.0 - (1.0 - t).powi(3);
            let ghost_scale = 1.0 + (GHOST_SCALE - 1.0) * eased;
            let alpha = 1.0 + (GHOST_ALPHA - 1.0) * eased as f32;
            let pointer = pointer.to_physical(scale).to_i32_round();
            let origin = pointer - grab;
            let pixels = (
                (pane.rect.columns * font.cell_width) as i32,
                (pane.rect.rows * font.cell_height) as i32,
            );
            if ghost.as_ref().is_none_or(|(_, bar)| *bar != layout.bar)
                && let Some(grid) = layout.draw_ghost(pane.id)
            {
                let raster = chrome::raster(&grid, font, glyphs);
                *ghost = renderer
                    .import_memory(&raster, Fourcc::Abgr8888, pixels.into(), false)
                    .ok()
                    .map(|texture| (texture, layout.bar));
            }
            if let Some((texture, _)) = ghost.as_ref() {
                let size: Size<i32, Physical> = pixels.into();
                let element = TextureRenderElement::from_static_texture(
                    Id::new(),
                    renderer.context_id(),
                    origin.to_f64(),
                    texture.clone(),
                    1,
                    Transform::Normal,
                    Some(alpha),
                    None,
                    Some(size.to_f64().to_logical(scale).to_i32_round()),
                    None,
                    Kind::Unspecified,
                );
                elements.push(Element::GhostChrome(RescaleRenderElement::from_element(
                    element,
                    pointer,
                    ghost_scale,
                )));
            }
            if let Some(toplevel) = &pane.toplevel {
                let client = layout.client_rect(pane);
                let offset: Point<i32, Physical> = (
                    ((client.column - pane.rect.column) * font.cell_width) as i32,
                    ((client.row - pane.rect.row) * font.cell_height) as i32,
                )
                    .into();
                let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(
                        renderer,
                        toplevel.wl_surface(),
                        origin + offset,
                        scale,
                        alpha,
                        Kind::Unspecified,
                    );
                elements.extend(surfaces.into_iter().map(|surface| {
                    Element::Ghost(RescaleRenderElement::from_element(
                        surface,
                        pointer,
                        ghost_scale,
                    ))
                }));
            }
        }
        if layout.palette
            && let Some(texture) = overlay.as_ref()
        {
            elements.push(texture_element(renderer, texture));
        }
        if let Some((texture, _)) = bars.as_ref() {
            elements.push(texture_element(renderer, texture));
        }
        for pane in layout.visible() {
            let Some(toplevel) = &pane.toplevel else {
                continue;
            };
            if layout.dragging == Some(pane.id) {
                continue;
            }
            let rect = layout.client_rect(pane);
            if rect.columns == 0 || rect.rows == 0 {
                continue;
            }
            let origin = (
                (rect.column * font.cell_width) as i32,
                (rect.row * font.cell_height) as i32,
            );
            let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    toplevel.wl_surface(),
                    origin,
                    scale,
                    1.0,
                    Kind::Unspecified,
                );
            elements.extend(surfaces.into_iter().map(Element::Surface));
        }
        if let Some((texture, _, _)) = chrome.as_ref() {
            elements.push(texture_element(renderer, texture));
        }

        let mut frame = renderer
            .render(&mut framebuffer, size, Transform::Flipped180)
            .unwrap();
        frame
            .clear(
                Color32F::new(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 1.0),
                &[damage],
            )
            .unwrap();
        draw_render_elements::<GlesRenderer, _, _>(&mut frame, scale, &elements, &[damage])
            .unwrap();
        let _ = frame.finish().unwrap();
        drop(framebuffer);
        if animating {
            backend.window().request_redraw();
        }

        let time = start.elapsed().as_millis() as u32;
        for pane in layout.visible() {
            if let Some(toplevel) = &pane.toplevel {
                send_frames_surface_tree(toplevel.wl_surface(), time);
            }
        }
    }
}

fn patch_chrome(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    pixels: &mut [u8],
    previous: &Grid,
    current: &Grid,
    font: &Font,
    glyphs: &mut Glyphs,
) {
    let columns = current.columns as usize;
    let cell_width = font.cell_width as usize;
    let cell_height = font.cell_height as usize;
    let stride = columns * cell_width * 4;
    for row in 0..current.rows as usize {
        let old = &previous.cells[row * columns..(row + 1) * columns];
        let new = &current.cells[row * columns..(row + 1) * columns];
        let mut column = 0;
        while column < columns {
            if old[column] == new[column] {
                column += 1;
                continue;
            }
            let start = column;
            while column < columns && old[column] != new[column] {
                column += 1;
            }
            let run = chrome::raster_run(&new[start..column], font, glyphs);
            let run_stride = (column - start) * cell_width * 4;
            for y in 0..cell_height {
                let from = y * run_stride;
                let to = (row * cell_height + y) * stride + start * cell_width * 4;
                pixels[to..to + run_stride].copy_from_slice(&run[from..from + run_stride]);
            }
            let region = Rectangle::new(
                (
                    (start as u32 * font.cell_width) as i32,
                    (row as u32 * font.cell_height) as i32,
                )
                    .into(),
                (
                    ((column - start) as u32 * font.cell_width) as i32,
                    font.cell_height as i32,
                )
                    .into(),
            );
            if renderer.update_memory(texture, pixels, region).is_err() {
                return;
            }
        }
    }
}

fn send_frames_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<State>(surface);
        self.backend.window().request_redraw();
        if let Some(i) = self
            .unplaced
            .iter()
            .position(|toplevel| toplevel.wl_surface() == surface)
        {
            let app_id = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().unwrap().app_id.clone())
            });
            let toplevel = self.unplaced.remove(i);
            let id = match self.layout.place(app_id.as_deref()) {
                Some(id) => id,
                None => {
                    let name = self
                        .layout
                        .unique_name(app_id.as_deref().unwrap_or("window"));
                    let id = self.layout.add_transient(name.clone());
                    self.router.add_pane(name);
                    self.relayout();
                    id
                }
            };
            if let Some(pane) = self.layout.pane_mut(id) {
                pane.toplevel = Some(toplevel);
            }
            self.focus(id);
        }
        let pane = self.layout.panes.iter().find(|pane| {
            pane.toplevel
                .as_ref()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface)
        });
        if let Some(pane) = pane
            && !pane.toplevel.as_ref().unwrap().is_initial_configure_sent()
        {
            let id = pane.id;
            self.configure(id);
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.unplaced.push(surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.unplaced.retain(|toplevel| toplevel != &surface);
        let Some(id) = self.layout.pane_of(&surface) else {
            return;
        };
        let transient = matches!(
            self.layout.pane(id).map(|pane| &pane.kind),
            Some(PaneKind::Transient)
        );
        if let Some(pane) = self.layout.pane_mut(id) {
            pane.toplevel = None;
        }
        if transient {
            self.remove_pane(id);
        } else {
            let focused = self.layout.focused;
            self.focus(focused);
        }
    }

    fn new_popup(&mut self, _: PopupSurface, _: PositionerState) {}
    fn grab(&mut self, _: PopupSurface, _: wl_seat::WlSeat, _: Serial) {}
    fn reposition_request(&mut self, _: PopupSurface, _: PositionerState, _: u32) {}
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.backend.renderer().import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _: &Seat<State>, _: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _: &Seat<State>, _: CursorImageStatus) {}
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}

impl ServerDndGrabHandler for State {
    fn send(&mut self, _: String, _: OwnedFd, _: Seat<State>) {}
}

impl OutputHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.send_scale(&surface);
    }
}

delegate_compositor!(State);
delegate_xdg_shell!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);
