use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::io::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Instant;

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent,
};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{
    Color32F, Frame, ImportAll, ImportDma, ImportMem, Renderer, RendererSuper,
};
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend, WinitInput};
use smithay::input::keyboard::{FilterResult, keysyms};
use smithay::input::pointer::{ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::channel::{self as calloop_channel, Event as ChannelEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopHandle, LoopSignal, Mode as CalloopMode, PostAction,
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
use crate::panes::{BACKGROUND, Direction, FOREGROUND, Layout, Pane};
use crate::text::{FONT_PX, Font, Glyphs};

const REFRESH_MILLIHERTZ: i32 = 60_000;
const AGENT_CLIENT: usize = usize::MAX;

render_elements! {
    Element<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Chrome = TextureRenderElement<<R as RendererSuper>::TextureId>,
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
    channel_path: PathBuf,
    mcp_url: Option<String>,
    font: Font,
    glyphs: Glyphs,
    layout: Layout,
    unplaced: Vec<ToplevelSurface>,
    chrome: Option<GlesTexture>,
    pointer: Point<f64, Logical>,
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

    let mut router = Router::new(
        layout
            .panes
            .iter()
            .map(|pane| pane.title.to_string())
            .collect(),
    );
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
        Ok(server) => Some(server),
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
        channel_path,
        mcp_url: mcp.map(|server| server.url),
        font,
        glyphs: Glyphs::new(),
        layout,
        unplaced: Vec::new(),
        chrome: None,
        pointer: (0.0, 0.0).into(),
        scale,
        start: Instant::now(),
    };

    let env = ChildEnv {
        wayland: &socket_name,
        channel: &state.channel_path,
        mcp: state.mcp_url.as_deref(),
    };
    let mut children = vec![spawn_terminal(&env, "terminal", &[])];
    if let Some(chat) = &commands.chat {
        children.push(spawn_terminal(&env, "chat", chat));
    }
    if let Some(code) = &commands.code {
        children.push(spawn_terminal(&env, "code", code));
    }
    state.backend.window().request_redraw();
    event_loop
        .run(None, &mut state, |state| {
            state.display_handle.flush_clients().unwrap();
        })
        .unwrap();

    for child in children.iter_mut().flatten() {
        let _ = child.kill();
    }
    let _ = std::fs::remove_file(&state.channel_path);
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

struct ChildEnv<'a> {
    wayland: &'a std::ffi::OsStr,
    channel: &'a Path,
    mcp: Option<&'a str>,
}

fn spawn_terminal(env: &ChildEnv, pane: &str, command: &[String]) -> Option<Child> {
    let mut ghostty = Command::new("ghostty");
    ghostty
        .env("WAYLAND_DISPLAY", env.wayland)
        .env("DECADE_SOCKET", env.channel)
        .env("DECADE_PANE", pane);
    if let Some(mcp) = env.mcp {
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
        Ok(child) => Some(child),
        Err(error) => {
            eprintln!("decade: could not start ghostty: {error}");
            None
        }
    }
}

impl State {
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
                Out::Host(Host::Focus(pane)) => self.focus(pane),
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
                Out::Host(Host::Focus(pane)) => self.focus(pane),
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
        self.chrome = None;
        for pane in 0..self.layout.panes.len() {
            self.configure(pane);
        }
        self.backend.window().request_redraw();
    }

    fn configure(&mut self, pane: usize) {
        let pane = &self.layout.panes[pane];
        let Some(toplevel) = &pane.toplevel else {
            return;
        };
        let (_, size) = self.client_geometry(pane);
        let focused = self.layout.panes[self.layout.focused].toplevel.as_ref() == Some(toplevel);
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

    fn focus(&mut self, pane: usize) {
        self.layout.focused = pane;
        self.chrome = None;
        let surface = self.layout.panes[pane]
            .toplevel
            .as_ref()
            .map(|toplevel| toplevel.wl_surface().clone());
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        for pane in 0..self.layout.panes.len() {
            self.configure(pane);
        }
        self.backend.window().request_redraw();
    }

    fn input(&mut self, event: InputEvent<WinitInput>) {
        match event {
            InputEvent::Keyboard { event } => {
                let keyboard = self.seat.get_keyboard().unwrap();
                let pressed = event.state() == KeyState::Pressed;
                let direction = keyboard.input::<Direction, _>(
                    self,
                    event.key_code(),
                    event.state(),
                    SERIAL_COUNTER.next_serial(),
                    event.time_msec(),
                    |_, modifiers, handle| {
                        if !pressed || !modifiers.ctrl {
                            return FilterResult::Forward;
                        }
                        match handle.modified_sym().raw() {
                            keysyms::KEY_h => FilterResult::Intercept(Direction::Left),
                            keysyms::KEY_j => FilterResult::Intercept(Direction::Down),
                            keysyms::KEY_k => FilterResult::Intercept(Direction::Up),
                            keysyms::KEY_l => FilterResult::Intercept(Direction::Right),
                            _ => FilterResult::Forward,
                        }
                    },
                );
                if let Some(direction) = direction
                    && let Some(pane) = self.layout.neighbour(direction)
                {
                    self.focus(pane);
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let size = self.backend.window_size().to_f64().to_logical(self.scale);
                self.pointer = event.position_transformed(size.to_i32_round());
                let pointer = self.seat.get_pointer().unwrap();
                let focus = self.pane_under_pointer().and_then(|pane| {
                    let (origin, _) = self.client_geometry(&self.layout.panes[pane]);
                    self.layout.panes[pane]
                        .toplevel
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
                if event.state() == ButtonState::Pressed
                    && let Some(pane) = self.pane_under_pointer()
                    && pane != self.layout.focused
                {
                    self.focus(pane);
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
            _ => {}
        }
    }

    fn pane_under_pointer(&self) -> Option<usize> {
        let physical = self.pointer.to_physical(self.scale);
        let column = (physical.x / self.font.cell_width as f64) as u32;
        let row = (physical.y / self.font.cell_height as f64) as u32;
        self.layout.pane_at(column, row)
    }

    fn client_geometry(&self, pane: &Pane) -> (Point<i32, Logical>, Size<i32, Logical>) {
        let rect = pane.client_rect();
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
            font,
            glyphs,
            start,
            scale,
            ..
        } = self;
        let scale = *scale;
        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        let (renderer, mut framebuffer) = backend.bind().unwrap();

        if chrome.is_none() {
            let mut grid = Grid::new(layout.columns, layout.rows, FOREGROUND, BACKGROUND);
            layout.draw(&mut grid);
            let pixels = chrome::raster(&grid, font, glyphs);
            let texture_size = (
                (grid.columns * font.cell_width) as i32,
                (grid.rows * font.cell_height) as i32,
            );
            let texture = renderer
                .import_memory(&pixels, Fourcc::Abgr8888, texture_size.into(), false)
                .unwrap();
            *chrome = Some(texture);
        }

        let mut elements: Vec<Element<GlesRenderer>> = Vec::new();
        for pane in &layout.panes {
            let Some(toplevel) = &pane.toplevel else {
                continue;
            };
            let rect = pane.client_rect();
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
        elements.push(Element::Chrome(TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            (0.0, 0.0),
            chrome.clone().unwrap(),
            1,
            Transform::Normal,
            None,
            Some(Rectangle::from_size(size.to_f64().to_logical(1.0))),
            Some(size.to_f64().to_logical(scale).to_i32_round()),
            None,
            Kind::Unspecified,
        )));

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

        let time = start.elapsed().as_millis() as u32;
        for pane in &layout.panes {
            if let Some(toplevel) = &pane.toplevel {
                send_frames_surface_tree(toplevel.wl_surface(), time);
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
            match self.layout.place(app_id.as_deref()) {
                Some(pane) => {
                    let toplevel = self.unplaced.remove(i);
                    self.layout.panes[pane].toplevel = Some(toplevel);
                    self.configure(pane);
                }
                None => eprintln!(
                    "decade: no pane for a window with app id {}",
                    app_id.as_deref().unwrap_or("(none)")
                ),
            }
        }
        let pane = self.layout.panes.iter().position(|pane| {
            pane.toplevel
                .as_ref()
                .is_some_and(|toplevel| toplevel.wl_surface() == surface)
        });
        if let Some(pane) = pane
            && !self.layout.panes[pane]
                .toplevel
                .as_ref()
                .unwrap()
                .is_initial_configure_sent()
        {
            self.configure(pane);
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
        if let Some(pane) = self.layout.pane_of(&surface) {
            self.layout.panes[pane].toplevel = None;
        }
        self.unplaced.retain(|toplevel| toplevel != &surface);
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
