use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::io::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::{
    AbsolutePositionEvent, Axis as InputAxis, AxisSource, ButtonState, Event, InputEvent, KeyState,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::utils::{
    CommitCounter, draw_render_elements, on_commit_buffer_handler,
};
use smithay::backend::renderer::{Color32F, Frame, ImportDma, Renderer};
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend, WinitInput};
use smithay::input::keyboard::{FilterResult, Keysym, ModifiersState, keysyms};
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

use crate::apps;
use crate::channel::{Host, Out, Router};
use crate::draw::{Face, Scene, Shape, TextCache};
use crate::hypr::{self, Hyprland};
use crate::inhibit::{self, Shortcuts};
use crate::mcp;
use crate::menu::{Action, Menu, MenuKey, Outcome};
use crate::palette::ACCENT;
use crate::panel::{Panel, PanelOutcome};
use crate::panes::{
    BACKGROUND, BarButton, Boundary, Direction, Drop, Kind as PaneKind, Layout, Pane,
};
use crate::settings::{self, Settings};
use crate::text::Font;

const REFRESH_MILLIHERTZ: i32 = 60_000;
const AGENT_CLIENT: usize = usize::MAX;
const BTN_LEFT: u32 = 0x110;
const RESIZE_STEP: u32 = 2;

enum Drag {
    Boundary(Boundary),
    Pane {
        id: usize,
        grab: Point<i32, Physical>,
        start: Instant,
    },
}

render_elements! {
    Element<=GlesRenderer>;
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Text = TextureRenderElement<GlesTexture>,
    Rounded = PixelShaderElement,
    Solid = SolidColorRenderElement,
    Ghost = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    GhostSolid = RescaleRenderElement<SolidColorRenderElement>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Focus(Direction),
    Zoom,
    Move(Direction),
    Resize(Direction),
    NewPane,
    Palette,
    Menu(MenuKey),
    Swallow,
}

struct ChildEnv {
    wayland: OsString,
    channel: PathBuf,
    mcp: Option<String>,
}

pub struct State {
    shortcuts: Option<Shortcuts>,
    hyprland: Option<Hyprland>,
    repeat: Repeat,
    repeating: Option<(u32, RegistrationToken)>,
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
    texts: TextCache,
    layout: Layout,
    unplaced: Vec<ToplevelSurface>,
    menu_font: Font,
    rounded: GlesPixelProgram,
    menu: Option<Menu>,
    panel: Option<Panel>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    pointer: Point<f64, Logical>,
    drag: Option<Drag>,
    ctrl_timer: Option<RegistrationToken>,
    scale: f64,
    start: Instant,
}

#[derive(Clone, Copy)]
struct Repeat {
    delay: i32,
    rate: i32,
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
    let rounded = backend
        .renderer()
        .compile_custom_pixel_shader(
            include_str!("rounded.frag"),
            &[
                UniformName::new("fill", UniformType::_4f),
                UniformName::new("border", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("border_width", UniformType::_1f),
            ],
        )
        .expect("the rounded rect shader compiles");
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
    let hyprland = Hyprland::find();
    let option = |name: &str| hyprland.as_ref().and_then(|hyprland| hyprland.option(name));
    let repeat = Repeat {
        delay: option("input:repeat_delay").unwrap_or(200).clamp(1, 5000) as i32,
        rate: option("input:repeat_rate").unwrap_or(25).clamp(1, 1000) as i32,
    };
    seat.add_keyboard(Default::default(), repeat.delay, repeat.rate)
        .unwrap();
    seat.add_pointer();

    let mut settings_path = settings::path();
    let settings = match settings_path.as_deref().map(settings::load) {
        Some(Ok(settings)) => settings,
        Some(Err(error)) => {
            let path = settings_path.take().unwrap_or_default();
            eprintln!(
                "decade: {} could not be read, so decade uses the defaults and will not write it: {error}",
                path.display()
            );
            Settings::default()
        }
        None => Settings::default(),
    };
    let font = Font::load(settings.font_size * scale as f32);
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
            WinitEvent::Redraw => {
                if let Some(shortcuts) = &mut state.shortcuts {
                    shortcuts.dispatch();
                }
                state.redraw();
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Focus(false) => state.stop_repeat(),
            WinitEvent::Focus(true) => {}
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
    let shortcuts = match inhibit::inhibit(backend.window()) {
        Ok(shortcuts) => Some(shortcuts),
        Err(error) => {
            eprintln!("decade: Hyprland keeps its shortcuts while decade has focus: {error}");
            None
        }
    };
    let mut state = State {
        shortcuts,
        hyprland,
        repeat,
        repeating: None,
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
        texts: TextCache::new(),
        menu_font: Font::load(settings.palette_size * scale as f32),
        rounded,
        layout,
        unplaced: Vec::new(),
        menu: None,
        panel: None,
        settings,
        settings_path,
        pointer: (0.0, 0.0).into(),
        drag: None,
        ctrl_timer: None,
        scale,
        start: Instant::now(),
    };

    state.watch_hyprland();
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
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("WAYLAND_DISPLAY", &self.env.wayland)
            .env("DECADE_SOCKET", &self.env.channel)
            .env_remove("DISPLAY");
        if let Some(mcp) = &self.env.mcp {
            command.env("DECADE_MCP", mcp);
        }
        command
    }

    fn launch(&mut self, argv: &[String]) {
        let Some((program, args)) = argv.split_first() else {
            return;
        };
        match self.command(program).args(args).spawn() {
            Ok(child) => self.children.push(child),
            Err(error) => eprintln!("decade: could not start {program}: {error}"),
        }
    }

    fn spawn_terminal(&mut self, pane: &str, command: &[String]) {
        let mut ghostty = self.command("ghostty");
        ghostty
            .env("DECADE_PANE", pane)
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

    fn watch_hyprland(&mut self) {
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
        let inserted = self.loop_handle.insert_source(
            Generic::new(stream, Interest::READ, CalloopMode::Level),
            move |_, stream, state| {
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
                                state.recount_alone();
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

    fn recount_alone(&mut self) {
        let alone = self.hyprland.as_ref().and_then(Hyprland::alone);
        if let (Some(alone), Some(shortcuts)) = (alone, &mut self.shortcuts) {
            shortcuts.set(alone);
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
            self.font = Font::load(self.settings.font_size * scale as f32);
            self.menu_font = Font::load(self.settings.palette_size * scale as f32);
            self.texts.clear();
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
            self.layout.hover = None;
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
                        let (column, row) = self.pointer_cell();
                        self.layout.drag_cell = Some((column, row));
                        self.drag = Some(Drag::Pane { id, grab, start });
                        self.backend.window().request_redraw();
                        return;
                    }
                    None => {}
                }
                if self.layout.bar {
                    let (column, row) = self.pointer_cell();
                    let hover = self
                        .layout
                        .bar_hit(column, row)
                        .filter(|&(_, button)| button != BarButton::Name);
                    if hover != self.layout.hover {
                        self.layout.hover = hover;
                        self.chrome_changed();
                    }
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
        let palette = self.menu.is_some() || self.panel.is_some();
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
                    return FilterResult::Intercept(menu_key(
                        pressed,
                        modifiers,
                        handle.modified_sym(),
                    ));
                }
                if !pressed || !modifiers.logo {
                    return FilterResult::Forward;
                }
                match super_key(sym, modifiers.shift) {
                    Some(key) => FilterResult::Intercept(key),
                    None => FilterResult::Forward,
                }
            },
        );
        match (control_key, pressed) {
            (true, true) if palette => {}
            (true, true) => self.ctrl_pressed(),
            (true, false) => self.ctrl_released(),
            (false, true) => self.cancel_ctrl_timer(),
            (false, false) => {}
        }
        let code = event.key_code().raw();
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
            self.run_key(key);
        }
    }

    fn start_repeat(&mut self, code: u32, key: Key) {
        let interval = Duration::from_millis(1000 / self.repeat.rate as u64);
        let timer = Timer::from_duration(Duration::from_millis(self.repeat.delay as u64));
        let token = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.run_key(key);
            TimeoutAction::ToDuration(interval)
        });
        self.repeating = token.ok().map(|token| (code, token));
    }

    fn stop_repeat(&mut self) {
        if let Some((_, token)) = self.repeating.take() {
            self.loop_handle.remove(token);
        }
    }

    fn run_key(&mut self, key: Key) {
        match key {
            Key::Zoom => {
                let focused = self.layout.focused;
                self.layout.toggle_zoom(focused);
                self.relayout();
            }
            Key::Focus(direction) => {
                if let Some(id) = self.layout.neighbour(direction) {
                    self.focus(id);
                }
            }
            Key::Move(direction) => {
                let focused = self.layout.focused;
                if let Some(id) = self.layout.neighbour(direction) {
                    self.layout.apply_drop(focused, Drop::Swap(id));
                    self.relayout();
                }
            }
            Key::Resize(direction) => {
                let focused = self.layout.focused;
                self.layout.resize_toward(focused, direction, RESIZE_STEP);
                self.relayout();
            }
            Key::NewPane => self.new_terminal_pane(),
            Key::Palette => {
                self.menu = Some(Menu::new(apps::installed()));
                self.backend.window().request_redraw();
            }
            Key::Menu(key) => {
                if let Some(panel) = &mut self.panel {
                    let before = self.settings;
                    match panel.key(key, &mut self.settings) {
                        PanelOutcome::Open => {}
                        PanelOutcome::Close => self.panel = None,
                        PanelOutcome::Changed => self.settings_changed(before),
                    }
                } else {
                    let outcome = self.menu.as_mut().map(|menu| menu.key(key));
                    match outcome {
                        Some(Outcome::Open) | None => {}
                        Some(Outcome::Close) => self.menu = None,
                        Some(Outcome::Run(Action::Launch(argv))) => {
                            self.menu = None;
                            self.launch(&argv);
                        }
                        Some(Outcome::Run(Action::Settings)) => {
                            self.menu = None;
                            self.panel = Some(Panel::new());
                        }
                    }
                }
                self.backend.window().request_redraw();
            }
            Key::Swallow => {}
        }
    }

    fn settings_changed(&mut self, before: Settings) {
        if let Some(path) = &self.settings_path
            && let Err(error) = settings::save(path, &self.settings)
        {
            eprintln!("decade: could not write {}: {error}", path.display());
        }
        let fonts = (self.settings.font_size, self.settings.palette_size);
        if fonts != (before.font_size, before.palette_size) {
            self.font = Font::load(self.settings.font_size * self.scale as f32);
            self.menu_font = Font::load(self.settings.palette_size * self.scale as f32);
            self.texts.clear();
            let size = self.backend.window_size();
            self.layout.resize(
                size.w as u32 / self.font.cell_width,
                size.h as u32 / self.font.cell_height,
            );
            self.relayout();
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
            Some((id, BarButton::Pin)) => {
                self.toggle_pin(id);
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

    fn toggle_pin(&mut self, id: usize) {
        let Some(pane) = self.layout.pane(id) else {
            return;
        };
        let client = pane
            .toplevel
            .as_ref()
            .map(|toplevel| toplevel.wl_surface().clone());
        match &pane.kind {
            PaneKind::Anchor { .. } => {
                self.layout.unpin(id);
                if client.is_none() {
                    self.remove_pane(id);
                }
            }
            PaneKind::Transient => {
                if let Some(app_id) = client.as_ref().and_then(app_id) {
                    self.layout.pin(id, app_id);
                }
            }
        }
        self.chrome_changed();
    }

    fn start_pane_drag(&mut self, id: usize) {
        self.layout.hover = None;
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
        let drop = self
            .layout
            .drag_cell
            .take()
            .and_then(|(column, row)| self.layout.drop_at(id, column, row));
        if let Some(drop) = drop {
            self.layout.apply_drop(id, drop);
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
            font,
            menu_font,
            texts,
            start,
            scale,
            drag,
            pointer,
            menu,
            panel,
            settings,
            rounded,
            ..
        } = self;
        let scale = *scale;
        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        let (renderer, mut framebuffer) = backend.bind().unwrap();
        let mut paint = Paint {
            renderer,
            texts,
            font,
            menu_font,
            rounded,
            scale,
        };
        let mut elements: Vec<Element> = Vec::new();

        if let Some(menu) = menu {
            let mut scene = Scene::new(paint.menu_font);
            let box_size = menu.draw(&mut scene, paint.menu_font);
            let origin = ((size.w - box_size.w) / 2, (size.h - box_size.h) / 3).into();
            paint.scene(&mut elements, &scene, origin, None);
        }
        if let Some(panel) = panel {
            let mut scene = Scene::new(paint.menu_font);
            let box_size = panel.draw(&mut scene, paint.menu_font, settings);
            let origin = ((size.w - box_size.w) / 2, (size.h - box_size.h) / 3).into();
            paint.scene(&mut elements, &scene, origin, None);
        }

        let mut animating = false;
        let dragged = match drag {
            Some(Drag::Pane { id, grab, start }) => {
                layout.pane(*id).map(|pane| (pane, *grab, *start))
            }
            _ => None,
        };
        if let Some((pane, grab, start)) = dragged {
            let animation = Duration::from_millis(settings.grab_ms).as_secs_f64();
            let t = if animation > 0.0 {
                (start.elapsed().as_secs_f64() / animation).min(1.0)
            } else {
                1.0
            };
            animating = t < 1.0;
            let eased = 1.0 - (1.0 - t).powi(3);
            let shrink = Shrink {
                about: pointer.to_physical(scale).to_i32_round(),
                scale: 1.0 + (settings.ghost_scale - 1.0) * eased,
                alpha: 1.0 + (settings.ghost_opacity - 1.0) * eased as f32,
            };
            let origin = shrink.about - grab;
            let cell: Point<i32, Physical> =
                (font.cell_width as i32, font.cell_height as i32).into();
            let outer: Point<i32, Physical> = (
                (pane.rect.columns * font.cell_width) as i32,
                (pane.rect.rows * font.cell_height) as i32,
            )
                .into();
            let start = shrink.point(origin + cell.downscale(2));
            let end = shrink.point(origin + outer - cell.downscale(2));
            let mut outline = Scene::new(font);
            let line = outline.line_width();
            outline.box_lines(
                start.x,
                start.y,
                end.x - start.x + line,
                end.y - start.y + line,
                ACCENT,
            );
            paint.scene(&mut elements, &outline, Point::default(), None);
            let mut ghost = Scene::new(font);
            layout.draw_ghost(pane.id, &mut ghost);
            paint.scene(&mut elements, &ghost, origin + cell, Some(shrink));
            if let Some(toplevel) = &pane.toplevel {
                let client = layout.client_rect(pane);
                let offset: Point<i32, Physical> = (
                    ((client.column - pane.rect.column) * font.cell_width) as i32,
                    ((client.row - pane.rect.row) * font.cell_height) as i32,
                )
                    .into();
                let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(
                        paint.renderer,
                        toplevel.wl_surface(),
                        origin + offset,
                        scale,
                        shrink.alpha,
                        Kind::Unspecified,
                    );
                elements.extend(surfaces.into_iter().map(|surface| {
                    Element::Ghost(RescaleRenderElement::from_element(
                        surface,
                        shrink.about,
                        shrink.scale,
                    ))
                }));
            }
        }

        let mut above = Scene::new(font);
        layout.draw_bars(&mut above);
        layout.draw_drop(&mut above);
        paint.scene(&mut elements, &above, Point::default(), None);

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
                    paint.renderer,
                    toplevel.wl_surface(),
                    origin,
                    scale,
                    1.0,
                    Kind::Unspecified,
                );
            elements.extend(surfaces.into_iter().map(Element::Surface));
        }

        let mut below = Scene::new(font);
        layout.draw(&mut below);
        paint.scene(&mut elements, &below, Point::default(), None);

        let mut frame = paint
            .renderer
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
        texts.end_frame();
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

#[derive(Clone, Copy)]
struct Shrink {
    about: Point<i32, Physical>,
    scale: f64,
    alpha: f32,
}

impl Shrink {
    fn point(&self, point: Point<i32, Physical>) -> Point<i32, Physical> {
        let offset = (point - self.about).to_f64().upscale(self.scale);
        (self.about.to_f64() + offset).to_i32_round()
    }
}

struct Paint<'a> {
    renderer: &'a mut GlesRenderer,
    texts: &'a mut TextCache,
    font: &'a Font,
    menu_font: &'a Font,
    rounded: &'a GlesPixelProgram,
    scale: f64,
}

impl Paint<'_> {
    fn scene(
        &mut self,
        elements: &mut Vec<Element>,
        scene: &Scene,
        origin: Point<i32, Physical>,
        shrink: Option<Shrink>,
    ) {
        let alpha = shrink.map_or(1.0, |shrink| shrink.alpha);
        for shape in scene.shapes.iter().rev() {
            match shape {
                Shape::Rect { at, size, color } => {
                    let a = color[3] * alpha;
                    let solid = SolidColorRenderElement::new(
                        Id::new(),
                        Rectangle::new(origin + *at, *size),
                        CommitCounter::default(),
                        Color32F::new(color[0] * a, color[1] * a, color[2] * a, a),
                        Kind::Unspecified,
                    );
                    elements.push(match shrink {
                        Some(shrink) => Element::GhostSolid(RescaleRenderElement::from_element(
                            solid,
                            shrink.about,
                            shrink.scale,
                        )),
                        None => Element::Solid(solid),
                    });
                }
                Shape::Rounded {
                    at,
                    size,
                    radius,
                    fill,
                    border,
                } => {
                    let premultiply = |c: [f32; 4]| {
                        let a = c[3] * alpha;
                        (c[0] * a, c[1] * a, c[2] * a, a)
                    };
                    let area = Rectangle::new(origin + *at, *size)
                        .to_f64()
                        .to_logical(self.scale)
                        .to_i32_round();
                    elements.push(Element::Rounded(PixelShaderElement::new(
                        self.rounded.clone(),
                        area,
                        None,
                        1.0,
                        vec![
                            Uniform::new("fill", premultiply(*fill)),
                            Uniform::new("border", premultiply(*border)),
                            Uniform::new("radius", *radius as f32),
                            Uniform::new("border_width", self.font.cell_width as f32 / 8.0),
                        ],
                        Kind::Unspecified,
                    )));
                }
                Shape::Text {
                    at,
                    text,
                    face,
                    color,
                } => {
                    if shrink.is_some() {
                        continue;
                    }
                    let font = match face {
                        Face::Chrome => self.font,
                        Face::Menu => self.menu_font,
                    };
                    let Some(entry) =
                        self.texts
                            .get(self.renderer, font, *face, text, *color, self.scale)
                    else {
                        continue;
                    };
                    let location = origin + *at + entry.offset;
                    let texture = TextureRenderElement::from_static_texture(
                        Id::new(),
                        self.renderer.context_id(),
                        location.to_f64(),
                        entry.texture.clone(),
                        1,
                        Transform::Normal,
                        None,
                        Some(Rectangle::from_size(entry.size.to_f64().to_logical(1.0))),
                        Some(entry.size.to_f64().to_logical(self.scale).to_i32_round()),
                        None,
                        Kind::Unspecified,
                    );
                    elements.push(Element::Text(texture));
                }
            }
        }
    }
}

fn repeats(key: Key) -> bool {
    match key {
        Key::Focus(_) | Key::Move(_) | Key::Resize(_) => true,
        Key::Menu(key) => !matches!(key, MenuKey::Enter | MenuKey::Escape),
        Key::Zoom | Key::NewPane | Key::Palette | Key::Swallow => false,
    }
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
        keysyms::KEY_Return if shift => Some(Key::NewPane),
        keysyms::KEY_space if shift => Some(Key::Palette),
        _ => None,
    }
}

fn menu_key(pressed: bool, modifiers: &ModifiersState, sym: Keysym) -> Key {
    if !pressed {
        return Key::Swallow;
    }
    let ctrl = modifiers.ctrl;
    let key = match sym.raw() {
        keysyms::KEY_Escape => MenuKey::Escape,
        keysyms::KEY_space if modifiers.logo => MenuKey::Escape,
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => MenuKey::Enter,
        keysyms::KEY_BackSpace => MenuKey::Backspace,
        keysyms::KEY_Up => MenuKey::Up,
        keysyms::KEY_Down => MenuKey::Down,
        keysyms::KEY_Left => MenuKey::Left,
        keysyms::KEY_Right => MenuKey::Right,
        keysyms::KEY_h if ctrl => MenuKey::Left,
        keysyms::KEY_l if ctrl => MenuKey::Right,
        keysyms::KEY_k if ctrl => MenuKey::Up,
        keysyms::KEY_j if ctrl => MenuKey::Down,
        _ if ctrl || modifiers.logo => return Key::Swallow,
        _ => match sym.key_char().filter(|ch| !ch.is_control()) {
            Some(ch) => MenuKey::Char(ch),
            None => return Key::Swallow,
        },
    };
    Key::Menu(key)
}

fn app_id(surface: &WlSurface) -> Option<String> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().unwrap().app_id.clone())
    })
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
            let app_id = app_id(surface);
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
