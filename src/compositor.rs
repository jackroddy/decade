use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::io::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
use smithay::backend::input::{
    Axis as InputAxis, AxisRelativeDirection, AxisSource, ButtonState, KeyState,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::utils::{
    CommitCounter, draw_render_elements, on_commit_buffer_handler,
};
use smithay::backend::renderer::{Bind, Color32F, Frame, ImportDma, Renderer};
use smithay::input::keyboard::{FilterResult, Keycode, Keysym, ModifiersState, keysyms};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::channel::{self as calloop_channel, Event as ChannelEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::signals::{Signal, Signals};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode as CalloopMode, PostAction, RegistrationToken,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat};
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle, Resource};
use smithay::utils::{
    Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, TraversalAction,
    with_states, with_surface_tree_downward,
};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::fractional_scale::{
    FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_focus, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, clear_primary_selection,
    current_primary_selection_userdata, request_primary_client_selection, set_primary_focus,
    set_primary_selection,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_shell, render_elements,
};

use crate::apps;
use crate::channel::{AGENT, Host, Out};
use crate::control::{self, viewer_socket_path};
use crate::draw::{Face, Scene, Shape, TextCache};
use crate::frame::{self, Hit, fit};
use crate::gpu::Gpu;
use crate::hypr::Hyprland;
use crate::mcp;
use crate::menu::{Action, Menu, MenuKey, Outcome};
use crate::palette::Rgba;
use crate::palette::{self, ACCENT};
use crate::panel::{Panel, PanelOutcome};
use crate::panes::{
    self as panes, BACKGROUND, BarButton, Boundary, CellRect, Direction, Drop, Kind as PaneKind,
    Layout, Pane,
};
use crate::picker::{self, Grid, Picker};
use crate::save;
use crate::scratch::{self, Launch};
use crate::settings::{self, Settings};
use crate::text::Font;
use crate::wire::{
    self, BTN_LEFT, CAPS_LOCK, Decoder, KEY_LEFTCTRL, KEY_LEFTMETA, KEY_LEFTSHIFT, NUM_LOCK,
    OUTBOX_RETRY, Outbox, Plane, SCALE, Selection, ServerMessage, ViewerMessage,
};
use crate::workspace::{self, Workspace};
use decade_channel::AgentState;

const REFRESH_MILLIHERTZ: i32 = 60_000;
const POOL_SIZE: usize = 3;
const KEY_C: u32 = 46;
const KEY_V: u32 = 47;
const KEY_X: u32 = 45;
const KEY_RIGHTMETA: u32 = 126;
const RESIZE_STEP: u32 = 2;
const KEY_CAPSLOCK: u32 = 58;
const KEY_NUMLOCK: u32 = 69;
const MAX_KEYCODE: u32 = 0xffff;
const MAX_LOGICAL: u32 = 16384;
const MAX_SCALE: u32 = SCALE * 8;
const CLOSE_TIMEOUT_MS: i32 = 1000;
const PICKER_TICK: Duration = Duration::from_millis(500);
const SAVE_DELAY: Duration = Duration::from_secs(1);
const SAVE_EVERY: Duration = Duration::from_secs(60);
const CLOSE_ICON: char = '\u{f0156}';
const SCRIM: Rgba = [0.0, 0.0, 0.0, 0.35];

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
    Surface = CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Text = TextureRenderElement<GlesTexture>,
    Rounded = PixelShaderElement,
    Solid = SolidColorRenderElement,
    Ghost = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    GhostSolid = RescaleRenderElement<SolidColorRenderElement>,
    Thumbnail = CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
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
    Swallow,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chord {
    Copy,
    Paste,
    Cut,
}

struct ChildEnv {
    wayland: OsString,
    runtime: PathBuf,
    mcp: Option<String>,
}

pub struct State {
    repeat: Repeat,
    repeating: Option<(u32, RegistrationToken)>,
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<State>,
    data_device_state: DataDeviceState,
    primary_selection_state: PrimarySelectionState,

    /// What viewers last heard about each selection, clipboard first.
    offered: [Offered; 2],
    dmabuf_state: DmabufState,
    _fractional_scale_state: FractionalScaleManagerState,
    _viewporter_state: ViewporterState,
    seat: Seat<State>,
    output: Output,
    gpu: Gpu,
    size: Size<i32, Physical>,
    viewers: HashMap<usize, Viewer>,
    next_viewer: usize,
    active: Option<usize>,
    wants_redraw: bool,
    render_failing: bool,
    stopping: bool,
    cursor: CursorImageStatus,
    display_handle: DisplayHandle,
    loop_handle: LoopHandle<'static, State>,
    sockets: HashMap<usize, (u32, UnixStream)>,
    next_client: usize,
    env: ChildEnv,
    children: Vec<Child>,
    font: Font,
    texts: TextCache,
    workspaces: Vec<Workspace>,
    next_workspace: u32,
    unplaced: Vec<ToplevelSurface>,
    menu_font: Font,
    rounded: GlesPixelProgram,
    menu: Option<Menu>,
    picker: Option<Picker>,
    sidebar_open: bool,

    /// The timer that sends frame callbacks to hidden workspaces while the picker is open.
    picker_tick: Option<RegistrationToken>,
    panel: Option<Panel>,
    settings: Settings,
    settings_path: Option<PathBuf>,

    /// Where the workspaces are saved, or `None` when saving is off.
    save_path: Option<PathBuf>,
    save_due: Option<RegistrationToken>,
    pointer: Point<f64, Logical>,
    drag: Option<Drag>,
    ctrl_timer: Option<RegistrationToken>,
    scale: f64,
    start: Instant,
}

struct Viewer {
    stream: UnixStream,
    source: Option<RegistrationToken>,
    outbox: Outbox,
    welcomed: bool,
    logical: (u32, u32),
    scale: u32,

    /// The formats the viewer's host imports, once `formats` has arrived.
    formats: Option<Vec<(Fourcc, Modifier)>>,
    linear: bool,
    pool: Vec<Slot>,
    next_buffer: u32,
    ready: bool,
    wants_frame: bool,
    keys: Vec<u32>,
    buttons: Vec<u32>,
    workspace: u32,

    /// Whether the viewer's window has keyboard focus on its host.
    focused: bool,
}

#[derive(Default)]
struct Offered {
    /// The serial of the server's newest offer or clear.
    serial: u32,

    /// The MIME types of a client's selection, while the newest offer stands.
    mimes: Option<Vec<String>>,
}

/// The viewer offer that a selection the server holds for its clients came from.
#[derive(Clone)]
pub struct ViewerOffer {
    viewer: usize,
    serial: u32,
}

struct Slot {
    id: u32,
    dmabuf: Dmabuf,
    size: Size<i32, Physical>,
    lent: bool,
}

struct Scroll {
    time: u32,
    source: u32,
    axis: u32,
    amount: f64,
    steps: i32,
    stop: bool,
    inverted: bool,
}

#[derive(Clone, Copy)]
struct Repeat {
    delay: i32,
    rate: i32,
}

pub fn run() {
    let viewer_path = viewer_socket_path();
    if let Some(dir) = viewer_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _lock = control::lock_server();

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
    let display: Display<State> = Display::new().unwrap();
    let dh = display.handle();

    let mut gpu = match Gpu::open() {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("decade: no GPU to render on: {error}");
            std::process::exit(1);
        }
    };

    let mut dmabuf_state = DmabufState::new();
    let formats = gpu.renderer.dmabuf_formats();
    let rounded = gpu
        .renderer
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
    let feedback = DmabufFeedbackBuilder::new(gpu.node.dev_id(), formats.clone())
        .build()
        .map_err(|error| error.to_string());
    let _dmabuf_global = match feedback {
        Ok(feedback) => dmabuf_state.create_global_with_default_feedback::<State>(&dh, &feedback),
        Err(error) => {
            eprintln!(
                "decade: clients get no GPU to render on and may fall back to software: {error}"
            );
            dmabuf_state.create_global::<State>(&dh, formats)
        }
    };

    let size: Size<i32, Physical> = (1920, 1080).into();
    let scale = 1.0;
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
    let (columns, rows) = (
        size.w as u32 / font.cell_width,
        size.h as u32 / font.cell_height,
    );
    let (workspaces, sidebar_open, save_path) = restore(columns, rows);

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
        .insert_source(
            Signals::new(&[Signal::SIGTERM, Signal::SIGINT]).unwrap(),
            |_, _, state| state.stopping = true,
        )
        .unwrap();

    let _ = std::fs::remove_file(&viewer_path);
    let viewer_listener = UnixListener::bind(&viewer_path).unwrap();
    viewer_listener.set_nonblocking(true).unwrap();
    handle
        .insert_source(
            Generic::new(viewer_listener, Interest::READ, CalloopMode::Level),
            |_, listener, state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => state.viewer_connected(stream),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            eprintln!("decade: accepting a viewer failed: {error}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .unwrap();

    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    remove_stale_sockets(&runtime);
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
        repeat,
        repeating: None,
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        shm_state: ShmState::new::<State>(&dh, vec![]),
        seat_state,
        data_device_state: DataDeviceState::new::<State>(&dh),
        primary_selection_state: PrimarySelectionState::new::<State>(&dh),
        offered: Default::default(),
        dmabuf_state,
        _fractional_scale_state: FractionalScaleManagerState::new::<State>(&dh),
        _viewporter_state: ViewporterState::new::<State>(&dh),
        seat,
        output,
        gpu,
        size,
        viewers: HashMap::new(),
        next_viewer: 0,
        active: None,
        wants_redraw: true,
        render_failing: false,
        stopping: false,
        cursor: CursorImageStatus::default_named(),
        display_handle: dh,
        loop_handle: handle.clone(),
        sockets: HashMap::new(),
        next_client: 0,
        env: ChildEnv {
            wayland: socket_name,
            runtime,
            mcp,
        },
        children: Vec::new(),
        font,
        texts: TextCache::new(),
        menu_font: Font::load(settings.palette_size * scale as f32),
        rounded,
        next_workspace: workspaces.len() as u32,
        workspaces,
        unplaced: Vec::new(),
        menu: None,
        picker: None,
        sidebar_open,
        picker_tick: None,
        panel: None,
        settings,
        settings_path,
        save_path,
        save_due: None,
        pointer: (0.0, 0.0).into(),
        drag: None,
        ctrl_timer: None,
        scale,
        start: Instant::now(),
    };

    for workspace in 0..state.workspaces.len() {
        state.listen(workspace);
        let terminals: Vec<(String, PathBuf)> = state.workspaces[workspace]
            .panes()
            .filter_map(|pane| Some((pane.name.clone(), pane.cwd.clone()?)))
            .collect();
        for (pane, cwd) in terminals {
            state.spawn_terminal(workspace, &pane, &cwd);
        }
        let mut tabs = Vec::new();
        for (tab, entry) in state.workspaces[workspace].tabs.iter_mut().enumerate() {
            for pane in &mut entry.layout.panes {
                for cwd in std::mem::take(&mut pane.saved_tabs) {
                    tabs.push((tab, pane.id, cwd));
                }
            }
        }
        for (tab, id, cwd) in tabs {
            state.spawn_pane_tab(workspace, tab, id, &cwd);
        }
    }
    let every = Timer::from_duration(SAVE_EVERY);
    let _ = handle.insert_source(every, |_, _, state| {
        state.save();
        TimeoutAction::ToDuration(SAVE_EVERY)
    });
    while !state.stopping {
        let backlog = state
            .viewers
            .values()
            .any(|viewer| !viewer.outbox.is_empty());
        event_loop
            .dispatch(backlog.then_some(OUTBOX_RETRY), &mut state)
            .unwrap();
        state.render_viewers();
        state.flush_viewers();
        state.display_handle.flush_clients().unwrap();
    }
    state.close_viewers();
    state.save();

    for child in &mut state.children {
        let _ = child.kill();
    }
    for workspace in &state.workspaces {
        let _ = std::fs::remove_file(state.socket_path(workspace.id));
    }
    let _ = std::fs::remove_file(&viewer_path);
}

/// The saved workspaces and whether the sidebar is open, for a window of
/// `columns` by `rows` cells.
fn restore(columns: u32, rows: u32) -> (Vec<Workspace>, bool, Option<PathBuf>) {
    let home = |open: bool| {
        let area = frame::pane_area(columns, rows, open);
        vec![Workspace::new(0, workspace::home(), area)]
    };
    let Some(path) = save::path() else {
        return (home(true), true, None);
    };
    let restored = save::load(&path).and_then(|file| {
        let Some(file) = file else {
            return Ok((Vec::new(), true));
        };
        let area = frame::pane_area(columns, rows, file.sidebar_open);
        let workspaces = file
            .workspaces
            .iter()
            .enumerate()
            .map(|(id, saved)| {
                let id = id as u32;
                let prefix = format!("decade.w{id}.");
                let layouts = saved.layouts(area, &prefix)?;
                let mut workspace = Workspace::with_tabs(
                    id,
                    saved.name.clone(),
                    saved.cwd.clone(),
                    layouts,
                    saved.active_tab,
                );
                if let Some(name) = &saved.scratch
                    && workspace.by_name(name).is_some()
                {
                    workspace.toggle_scratch(name);
                }
                Ok(workspace)
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((workspaces, file.sidebar_open))
    });
    match restored {
        Ok((workspaces, open)) if workspaces.is_empty() => (home(open), open, Some(path)),
        Ok((workspaces, open)) => (workspaces, open, Some(path)),
        Err(error) => {
            eprintln!(
                "decade: {} could not be read, so decade starts fresh and will not save over it: {error}",
                path.display()
            );
            (home(true), true, None)
        }
    }
}

fn remove_stale_sockets(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|name| name.ends_with(".sock"))
            .and_then(|name| name.strip_prefix("decade-"))
            .and_then(|rest| rest.split(['-', '.']).next())
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
    fn command(&self, workspace: usize, program: &str) -> Command {
        let workspace = &self.workspaces[workspace];
        let mut command = Command::new(program);
        command
            .current_dir(&workspace.cwd)
            .env("DECADE_WORKSPACE", &workspace.name)
            .env("WAYLAND_DISPLAY", &self.env.wayland)
            .env("DECADE_SOCKET", self.socket_path(workspace.id))
            .env_remove("DISPLAY");
        if let Some(mcp) = &self.env.mcp {
            command.env("DECADE_MCP", format!("{mcp}?workspace={}", workspace.id));
        }
        command
    }

    fn launch(&mut self, argv: &[String]) {
        let Some((program, args)) = argv.split_first() else {
            return;
        };
        match self.command(self.current(), program).args(args).spawn() {
            Ok(child) => self.children.push(child),
            Err(error) => eprintln!("decade: could not start {program}: {error}"),
        }
    }

    /// A ghostty whose window goes in the pane, with the app id `class`
    /// after the workspace's prefix.
    fn ghostty(&self, workspace: usize, pane: &str, class: &str) -> Command {
        let prefix = self.workspaces[workspace].app_id_prefix();
        let mut ghostty = self.command(workspace, "ghostty");
        ghostty
            .env("DECADE_PANE", pane)
            .args(["--window-decoration=none", "--gtk-titlebar=false"])
            .arg(format!("--class={prefix}{class}"));
        ghostty
    }

    /// Start a ghostty that goes in the pane as a new tab.
    fn spawn_pane_tab(&mut self, workspace: usize, tab: usize, id: usize, cwd: &Path) {
        let Some(pane) = self.workspaces[workspace].tabs[tab].layout.pane_mut(id) else {
            return;
        };
        let name = pane.name.clone();
        let class = format!("{name}.t{}", pane.next_tab);
        pane.next_tab += 1;
        let mut ghostty = self.ghostty(workspace, &name, &class);
        ghostty.current_dir(cwd);
        match ghostty.spawn() {
            Ok(child) => self.children.push(child),
            Err(error) => eprintln!("decade: could not start ghostty: {error}"),
        }
    }

    fn spawn_terminal(&mut self, workspace: usize, pane: &str, cwd: &Path) {
        let mut ghostty = self.ghostty(workspace, pane, pane);
        ghostty.current_dir(cwd);
        match ghostty.spawn() {
            Ok(child) => {
                let shown = &mut self.workspaces[workspace];
                if let Some((tab, id)) = shown.by_name(pane)
                    && let Some(pane) = shown.tabs[tab].layout.pane_mut(id)
                {
                    pane.pid = Some(child.id());
                }
                self.children.push(child);
            }
            Err(error) => eprintln!("decade: could not start ghostty: {error}"),
        }
    }

    fn request_redraw(&mut self) {
        self.wants_redraw = true;
    }

    fn viewer_connected(&mut self, stream: UnixStream) {
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
                formats: None,
                linear: false,
                pool: Vec::new(),
                next_buffer: 0,
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
        let now = self.start.elapsed().as_millis() as u32;
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
            self.menu = None;
            self.panel = None;
            self.close_picker();
            let next = self.welcomed().next();
            self.set_active(next);
        }
    }

    fn close_viewers(&mut self) {
        for viewer in self.viewers.values_mut().filter(|viewer| viewer.welcomed) {
            let _ = viewer.outbox.push_server_message(ServerMessage::Close {
                message: "the server is shutting down".to_string(),
            });
            let deadline = Instant::now() + Duration::from_millis(CLOSE_TIMEOUT_MS as u64);
            while let Ok(false) = viewer.outbox.send(&viewer.stream) {
                let left = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as i32;
                if left == 0 {
                    break;
                }
                let mut fds = [PollFd::new(&viewer.stream, PollFlags::OUT)];
                let _ = rustix::event::poll(&mut fds, left);
            }
        }
    }

    fn send(&mut self, id: usize, message: ServerMessage) {
        if let Some(viewer) = self.viewers.get_mut(&id)
            && viewer.outbox.push_server_message(message).is_err()
        {
            eprintln!("decade: a message to a viewer could not be encoded");
        }
    }

    fn flush_viewers(&mut self) {
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

    fn welcomed(&self) -> impl Iterator<Item = usize> + '_ {
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

    fn current(&self) -> usize {
        self.active
            .and_then(|id| self.viewers.get(&id))
            .and_then(|viewer| {
                self.workspaces
                    .iter()
                    .position(|workspace| workspace.id == viewer.workspace)
            })
            .unwrap_or(0)
    }

    fn layout(&self) -> &Layout {
        self.workspaces[self.current()].layout()
    }

    fn layout_mut(&mut self) -> &mut Layout {
        let current = self.current();
        self.workspaces[current].layout_mut()
    }

    /// The workspace, tab and id of the pane showing `surface`.
    fn pane_of_surface(&self, surface: &WlSurface) -> Option<(usize, usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(index, workspace)| {
                workspace.tabs.iter().enumerate().find_map(|(tab, entry)| {
                    let pane = entry.layout.panes.iter().find(|pane| {
                        pane.clients
                            .iter()
                            .any(|client| client.toplevel.wl_surface() == surface)
                    })?;
                    Some((index, tab, pane.id))
                })
            })
    }

    fn workspace_list(&self) -> ServerMessage {
        ServerMessage::Workspaces {
            workspaces: self
                .workspaces
                .iter()
                .map(|workspace| wire::Workspace {
                    id: workspace.id,
                    name: workspace.name.clone(),
                })
                .collect(),
        }
    }

    fn show(&mut self, viewer: usize, workspace: u32) {
        let before = self.current();
        let known = self.workspaces.iter().any(|known| known.id == workspace);
        let Some(entry) = self.viewers.get_mut(&viewer) else {
            return;
        };
        if known {
            entry.workspace = workspace;
        }
        let shown = entry.workspace;
        self.send(viewer, ServerMessage::Showing { id: shown });
        if self.current() != before {
            self.workspace_shown();
        }
        self.request_redraw();
    }

    fn add_workspace(&mut self, cwd: PathBuf) -> u32 {
        let id = self.next_workspace;
        self.next_workspace += 1;
        let mut created = Workspace::new(id, cwd, self.pane_area());
        let taken = self.workspaces.iter().map(|known| known.name.as_str());
        created.name = workspace::unique_name(&created.name, taken);
        self.workspaces.push(created);
        let index = self.workspaces.len() - 1;
        self.listen(index);
        let cwd = self.workspaces[index].cwd.clone();
        self.spawn_terminal(index, "terminal", &cwd);
        id
    }

    fn send_workspace_list(&mut self) {
        let viewers: Vec<usize> = self.welcomed().collect();
        for viewer in viewers {
            self.send(viewer, self.workspace_list());
        }
        self.schedule_save();
    }

    fn new_workspace(&mut self, cwd: PathBuf) {
        let id = self.add_workspace(cwd);
        self.send_workspace_list();
        if let Some(active) = self.active {
            self.show(active, id);
        }
    }

    fn close_workspace(&mut self, id: u32) {
        let Some(index) = self.workspace_index(id) else {
            return;
        };
        let workspace = &mut self.workspaces[index];
        workspace.closing = true;
        for client in workspace.panes().flat_map(|pane| &pane.clients) {
            client.toplevel.send_close();
        }
        self.finish_closing(index);
    }

    fn finish_closing(&mut self, index: usize) {
        let workspace = &self.workspaces[index];
        let clients = workspace.panes().any(|pane| !pane.clients.is_empty());
        if !workspace.closing || clients {
            return;
        }
        let removed = self.workspaces.remove(index);
        if let Some(token) = removed.listener {
            self.loop_handle.remove(token);
        }
        let _ = std::fs::remove_file(self.socket_path(removed.id));
        self.sockets
            .retain(|_, (workspace, _)| *workspace != removed.id);
        if self.workspaces.is_empty() {
            self.add_workspace(workspace::home());
        }
        let first = self.workspaces[0].id;
        let moved: Vec<usize> = self
            .viewers
            .iter()
            .filter(|(_, viewer)| viewer.workspace == removed.id)
            .map(|(&id, _)| id)
            .collect();
        self.send_workspace_list();
        for viewer in &moved {
            if let Some(entry) = self.viewers.get_mut(viewer) {
                entry.workspace = first;
            }
            self.send(*viewer, ServerMessage::Showing { id: first });
        }
        if self.active.is_some_and(|active| moved.contains(&active)) {
            self.workspace_shown();
        }
        self.request_redraw();
    }

    fn picker_grid(&self) -> Grid {
        Grid::new(
            self.workspaces.len() + 1,
            self.size,
            self.font.cell_height as i32,
        )
    }

    fn open_picker(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.picker = Some(Picker::new(self.current()));
        let tick =
            self.loop_handle
                .insert_source(Timer::from_duration(PICKER_TICK), |_, _, state| {
                    state.tick_hidden();
                    TimeoutAction::ToDuration(PICKER_TICK)
                });
        self.picker_tick = tick.ok();
        self.tick_hidden();
    }

    fn close_picker(&mut self) {
        self.picker = None;
        if let Some(token) = self.picker_tick.take() {
            self.loop_handle.remove(token);
        }
        self.request_redraw();
    }

    fn tick_hidden(&mut self) {
        let time = self.start.elapsed().as_millis() as u32;
        for workspace in &self.workspaces {
            let shown = self
                .viewers
                .values()
                .any(|viewer| viewer.welcomed && viewer.workspace == workspace.id);
            if shown {
                continue;
            }
            for pane in &workspace.layout().panes {
                if let Some(toplevel) = pane.toplevel() {
                    send_frames_surface_tree(toplevel.wl_surface(), time);
                }
            }
        }
        self.request_redraw();
    }

    fn picker_outcome(&mut self, outcome: picker::Outcome) {
        match outcome {
            picker::Outcome::Open => {}
            picker::Outcome::Close => self.close_picker(),
            picker::Outcome::Show(id) => {
                self.close_picker();
                if let Some(active) = self.active {
                    self.show(active, id);
                }
            }
            picker::Outcome::New => {
                self.close_picker();
                self.open_palette();
                if let Some(menu) = &mut self.menu {
                    menu.start_new_workspace();
                }
            }
            picker::Outcome::CloseWorkspace(id) => self.close_workspace(id),
            picker::Outcome::Move { from, to } => {
                let moved = self.workspaces.remove(from);
                self.workspaces.insert(to, moved);
                self.send_workspace_list();
            }
        }
        if let Some(picker) = &mut self.picker {
            picker.selected = picker.selected.min(self.workspaces.len());
        }
        self.request_redraw();
    }

    fn workspace_shown(&mut self) {
        self.drag = None;
        let layout = self.layout_mut();
        layout.dragging = None;
        layout.bar = false;
        layout.hover = None;
        self.relayout_all();
        let focused = self.layout().focused;
        self.focus(focused);
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
        let now = self.start.elapsed().as_millis() as u32;
        match message {
            ViewerMessage::Hello {
                version,
                width,
                height,
                scale,
                ..
            } => {
                if version == 0 {
                    self.viewer_failed(id, "decade speaks protocol version 1");
                    return;
                }
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.welcomed = true;
                    viewer.logical = (width, height);
                    viewer.scale = scale;
                    viewer.ready = true;
                }
                let version = version.min(wire::VERSION);
                self.send(id, ServerMessage::Welcome { version });
                self.send(id, self.workspace_list());
                let shown = self.workspaces[0].id;
                self.send(id, ServerMessage::Showing { id: shown });
                self.make_active(id);
                self.send_cursor(id);
                for selection in [Selection::Clipboard, Selection::Primary] {
                    if self.offered[selection as usize].mimes.is_some() {
                        self.tell_selection(id, selection);
                    }
                }
                self.request_redraw();
            }
            ViewerMessage::Formats { formats } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    let formats = formats
                        .iter()
                        .filter_map(|format| {
                            let code = Fourcc::try_from(format.format).ok()?;
                            Some((code, Modifier::from(format.modifier)))
                        })
                        .collect();
                    viewer.formats = Some(formats);
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
                self.active = None;
                self.make_active(id);
            }
            ViewerMessage::BufferFailed { id: buffer } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.linear = true;
                    viewer.pool.retain(|slot| slot.id != buffer);
                    viewer.ready = true;
                    viewer.wants_frame = true;
                }
                self.send(id, ServerMessage::Forget { id: buffer });
            }
            ViewerMessage::Release { id: buffer, .. } => {
                let size = self.size;
                let mut forget = None;
                if let Some(viewer) = self.viewers.get_mut(&id)
                    && let Some(index) = viewer.pool.iter().position(|slot| slot.id == buffer)
                {
                    viewer.pool[index].lent = false;
                    if viewer.pool[index].size != size {
                        viewer.pool.remove(index);
                        forget = Some(buffer);
                    }
                }
                if let Some(buffer) = forget {
                    self.send(id, ServerMessage::Forget { id: buffer });
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
                    viewer.keys.retain(|&held| held != keycode);
                    if pressed {
                        viewer.keys.push(keycode);
                    }
                }
                self.keyboard(keycode, pressed, time);
            }
            ViewerMessage::Keys { locked, keycodes } => {
                if let Some(viewer) = self.viewers.get_mut(&id) {
                    viewer.focused = true;
                }
                let keyboard = self.seat.get_keyboard().unwrap();
                let modifiers = keyboard.modifier_state();
                let locks = [
                    (modifiers.caps_lock, locked & CAPS_LOCK != 0, KEY_CAPSLOCK),
                    (modifiers.num_lock, locked & NUM_LOCK != 0, KEY_NUMLOCK),
                ];
                for (on, wanted, keycode) in locks {
                    if on == wanted {
                        continue;
                    }
                    for state in [KeyState::Pressed, KeyState::Released] {
                        keyboard.input::<(), _>(
                            self,
                            Keycode::new(keycode + 8),
                            state,
                            SERIAL_COUNTER.next_serial(),
                            now,
                            |_, _, _| FilterResult::Forward,
                        );
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
                    keyboard.input::<(), _>(
                        self,
                        Keycode::new(keycode + 8),
                        KeyState::Pressed,
                        SERIAL_COUNTER.next_serial(),
                        now,
                        |_, _, _| FilterResult::Forward,
                    );
                }
            }
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
                    viewer.buttons.retain(|&held| held != button);
                    if pressed {
                        viewer.buttons.push(button);
                    }
                }
                self.pointer_button(button, pressed, time);
            }
            ViewerMessage::Scroll {
                time,
                source,
                axis,
                amount,
                steps,
                stop,
                inverted,
            } => self.pointer_scroll(Scroll {
                time,
                source,
                axis,
                amount,
                steps,
                stop,
                inverted,
            }),
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
            } => {
                let offered = &self.offered[selection as usize];
                if offered.serial != serial || offered.mimes.is_none() {
                    return;
                }
                // an Err drops the fd, which closes the pipe
                // and tells the viewer there is no data
                match selection {
                    Selection::Clipboard => {
                        let _ = request_data_device_client_selection(&self.seat, mime, fd);
                    }
                    Selection::Primary => {
                        let _ = request_primary_client_selection(&self.seat, mime, fd);
                    }
                }
            }
            ViewerMessage::List => {
                let workspaces = self
                    .workspaces
                    .iter()
                    .map(|workspace| wire::Listed {
                        id: workspace.id,
                        name: workspace.name.clone(),
                        cwd: workspace.cwd.display().to_string(),
                        panes: workspace.panes().count() as u32,
                    })
                    .collect();
                self.send(id, ServerMessage::Listing { workspaces });
                self.flush_viewers();
                self.viewer_gone(id);
            }
            ViewerMessage::Error { message, .. } => {
                eprintln!("decade: a viewer reported an error: {message}");
                self.viewer_gone(id);
            }
        }
    }

    fn viewer_offer(
        &mut self,
        id: usize,
        selection: Selection,
        serial: u32,
        seen: u32,
        mimes: Vec<String>,
    ) {
        let offered = &mut self.offered[selection as usize];
        if crossed(seen, offered.serial) {
            return;
        }
        offered.mimes = None;
        let owner = ViewerOffer { viewer: id, serial };
        let dh = &self.display_handle;
        match selection {
            Selection::Clipboard => set_data_device_selection(dh, &self.seat, mimes, owner),
            Selection::Primary => set_primary_selection(dh, &self.seat, mimes, owner),
        }
    }

    fn viewer_owns(&self, selection: Selection, id: usize) -> bool {
        match selection {
            Selection::Clipboard => current_data_device_selection_userdata(&self.seat)
                .is_some_and(|owner| owner.viewer == id),
            Selection::Primary => current_primary_selection_userdata(&self.seat)
                .is_some_and(|owner| owner.viewer == id),
        }
    }

    fn clear_selection(&mut self, selection: Selection) {
        match selection {
            Selection::Clipboard => clear_data_device_selection(&self.display_handle, &self.seat),
            Selection::Primary => clear_primary_selection(&self.display_handle, &self.seat),
        }
    }

    fn client_selection(&mut self, target: SelectionTarget, mimes: Option<Vec<String>>) {
        let selection = selection_of(target);
        let offered = &mut self.offered[selection as usize];
        offered.serial += 1;
        offered.mimes = mimes;
        let ids: Vec<usize> = self.welcomed().collect();
        for id in ids {
            self.tell_selection(id, selection);
        }
    }

    fn tell_selection(&mut self, id: usize, selection: Selection) {
        let offered = &self.offered[selection as usize];
        let serial = offered.serial;
        let message = match &offered.mimes {
            Some(mimes) => ServerMessage::Offer {
                selection,
                serial,
                mimes: mimes.clone(),
            },
            None => ServerMessage::Cleared { selection, serial },
        };
        self.send(id, message);
    }

    fn render_viewers(&mut self) {
        if self.wants_redraw {
            self.wants_redraw = false;
            for viewer in self.viewers.values_mut() {
                viewer.wants_frame = true;
            }
        }
        let ids: Vec<usize> = self
            .viewers
            .iter()
            .filter(|(_, viewer)| {
                viewer.welcomed && viewer.ready && viewer.wants_frame && viewer.formats.is_some()
            })
            .map(|(&id, _)| id)
            .collect();
        for id in ids {
            self.render_to(id);
        }
    }

    fn render_to(&mut self, id: usize) {
        let size = self.size;
        let Some(viewer) = self.viewers.get_mut(&id) else {
            return;
        };
        let free = viewer
            .pool
            .iter()
            .position(|slot| !slot.lent && slot.size == size);
        let index = match free {
            Some(index) => index,
            None => {
                let current = viewer.pool.iter().filter(|slot| slot.size == size).count();
                if current >= POOL_SIZE {
                    return;
                }
                let (width, height) = (size.w as u32, size.h as u32);
                let formats = viewer.formats.as_deref().unwrap_or_default();
                let allocated = if viewer.linear || formats.is_empty() {
                    self.gpu.allocate_linear(width, height)
                } else {
                    match self.gpu.allocate(width, height, formats) {
                        Ok(dmabuf) => Ok(dmabuf),
                        Err(error) => {
                            eprintln!(
                                "decade: allocating a frame buffer failed, so decade falls back to LINEAR: {error}"
                            );
                            viewer.linear = true;
                            self.gpu.allocate_linear(width, height)
                        }
                    }
                };
                let dmabuf = match allocated {
                    Ok(dmabuf) => dmabuf,
                    Err(error) => {
                        eprintln!("decade: allocating a frame buffer failed: {error}");
                        return;
                    }
                };
                let buffer = viewer.next_buffer;
                viewer.next_buffer += 1;
                let planes = dmabuf
                    .handles()
                    .zip(dmabuf.offsets())
                    .zip(dmabuf.strides())
                    .filter_map(|((fd, offset), stride)| {
                        Some(Plane {
                            fd: fd.try_clone_to_owned().ok()?,
                            offset,
                            stride,
                        })
                    })
                    .collect();
                let format = dmabuf.format();
                let message = ServerMessage::Buffer {
                    id: buffer,
                    width,
                    height,
                    format: format.code as u32,
                    modifier: format.modifier.into(),
                    planes,
                };
                if viewer.outbox.push_server_message(message).is_err() {
                    return;
                }
                viewer.pool.push(Slot {
                    id: buffer,
                    dmabuf,
                    size,
                    lent: false,
                });
                viewer.pool.len() - 1
            }
        };
        let mut dmabuf = viewer.pool[index].dmabuf.clone();
        let buffer = viewer.pool[index].id;
        let workspace = viewer.workspace;
        let workspace = self
            .workspaces
            .iter()
            .position(|known| known.id == workspace)
            .unwrap_or(0);
        let fence = match self.render(&mut dmabuf, workspace) {
            Ok(fence) => fence,
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
        viewer.pool[index].lent = true;
        viewer.ready = false;
        viewer.wants_frame = false;
        let _ = viewer
            .outbox
            .push_server_message(ServerMessage::Frame { id: buffer, fence });
    }

    fn send_cursor(&mut self, id: usize) {
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

    fn socket_path(&self, workspace: u32) -> PathBuf {
        let pid = std::process::id();
        self.env
            .runtime
            .join(format!("decade-{pid}-w{workspace}.sock"))
    }

    fn listen(&mut self, workspace: usize) {
        let id = self.workspaces[workspace].id;
        let path = self.socket_path(id);
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("decade: {}: {error}", path.display());
                return;
            }
        };
        if let Err(error) = listener.set_nonblocking(true) {
            eprintln!("decade: {}: {error}", path.display());
            return;
        }
        let inserted = self.loop_handle.insert_source(
            Generic::new(listener, Interest::READ, CalloopMode::Level),
            move |_, listener, state| {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => state.channel_connected(stream, id),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            eprintln!("decade: accepting a channel client failed: {error}");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        );
        match inserted {
            Ok(token) => self.workspaces[workspace].listener = Some(token),
            Err(error) => eprintln!("decade: {} is not watched: {error}", path.display()),
        }
    }

    fn workspace_index(&self, id: u32) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|workspace| workspace.id == id)
    }

    fn channel_connected(&mut self, stream: UnixStream, workspace: u32) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(writer) = stream.try_clone() else {
            return;
        };
        let Some(index) = self.workspace_index(workspace) else {
            return;
        };
        let id = self.next_client;
        self.next_client += 1;
        self.workspaces[index].router.connected(id);
        self.sockets.insert(id, (workspace, writer));
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
        let Some(workspace) = request.workspace.and_then(|id| self.workspace_index(id)) else {
            let reason = match request.workspace {
                Some(id) => format!("workspace {id} is closed"),
                None => "the MCP address names no workspace".to_string(),
            };
            let _ = request.reply.send(vec![error_line(&reason)]);
            return;
        };
        let line = match request.call {
            mcp::Call::Line(line) => line,
            mcp::Call::ScratchOpen(launch) => {
                let reply = self
                    .scratch_open(workspace, launch)
                    .unwrap_or_else(|reason| error_line(&reason));
                let _ = request.reply.send(vec![reply]);
                return;
            }
            mcp::Call::ScratchSend { verb, payload } => {
                let Some(pane) = self.workspaces[workspace].scratch_pane() else {
                    let _ = request.reply.send(vec![error_line(NO_SCRATCH)]);
                    return;
                };
                let mut message = serde_json::Map::new();
                message.insert("to".to_string(), pane.into());
                message.insert(verb, payload);
                serde_json::Value::Object(message).to_string()
            }
        };
        let mut reply = Vec::new();
        for out in self.workspaces[workspace].router.handle(AGENT, &line) {
            match out {
                Out::Line(to, text) if to == AGENT => reply.push(text),
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Host(Host::Focus(pane)) => self.focus_named(workspace, &pane),
                Out::Host(Host::Status { .. }) => {}
            }
        }
        let _ = request.reply.send(reply);
    }

    /// Start the program in the workspace's scratch pane, or ask the
    /// program there to close and start it once it has.
    fn scratch_open(&mut self, workspace: usize, launch: Launch) -> Result<String, String> {
        let shown = &self.workspaces[workspace];
        let name = shown.scratch_pane().ok_or(NO_SCRATCH)?;
        scratch::find_program(
            &launch.program,
            &std::env::var_os("PATH").unwrap_or_default(),
        )?;
        let occupants: Vec<ToplevelSurface> = shown
            .by_name(name)
            .and_then(|(tab, id)| shown.tabs[tab].layout.pane(id))
            .map(|pane| {
                pane.clients
                    .iter()
                    .map(|client| client.toplevel.clone())
                    .collect()
            })
            .unwrap_or_default();
        if occupants.is_empty() {
            return self.start_scratch(workspace, launch);
        }
        for occupant in &occupants {
            occupant.send_close();
        }
        let reply = format!(
            "asked the program in pane {name} to close; {} starts there once it has",
            launch.program
        );
        if let Some(scratch) = &mut self.workspaces[workspace].scratch {
            scratch.next = Some(launch);
        }
        Ok(reply)
    }

    fn start_scratch(&mut self, workspace: usize, launch: Launch) -> Result<String, String> {
        let name = self.workspaces[workspace]
            .scratch_pane()
            .ok_or(NO_SCRATCH)?
            .to_string();
        let mut command = if launch.terminal {
            let mut ghostty = self.ghostty(workspace, &name, &name);
            ghostty.arg("-e").arg(&launch.program).args(&launch.args);
            ghostty
        } else {
            let mut command = self.command(workspace, &launch.program);
            command.env("DECADE_PANE", &name).args(&launch.args);
            command
        };
        let child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", launch.program))?;
        if !launch.terminal
            && let Some(scratch) = &mut self.workspaces[workspace].scratch
        {
            scratch.expected = Some(child.id());
        }
        self.children.push(child);
        let place = if launch.terminal {
            "in a terminal in"
        } else {
            "in"
        };
        Ok(format!("started {} {place} pane {name}", launch.program))
    }

    /// The workspace, tab and id of the empty scratch pane waiting for a
    /// window from a program under `pid`.
    fn expecting(&mut self, pid: u32) -> Option<(usize, usize, usize)> {
        for (index, workspace) in self.workspaces.iter_mut().enumerate() {
            let Some(scratch) = &mut workspace.scratch else {
                continue;
            };
            let Some(expected) = scratch.expected else {
                continue;
            };
            if !scratch::descends_from(pid, expected) {
                continue;
            }
            let Some((tab, id)) = workspace.tabs.iter().enumerate().find_map(|(tab, entry)| {
                let id = entry.layout.by_name(&scratch.pane)?;
                Some((tab, id))
            }) else {
                continue;
            };
            if !workspace.tabs[tab].layout.pane(id)?.clients.is_empty() {
                continue;
            }
            scratch.expected = None;
            return Some((index, tab, id));
        }
        None
    }

    fn channel_disconnected(&mut self, id: usize) {
        let Some((workspace, _)) = self.sockets.remove(&id) else {
            return;
        };
        if let Some(index) = self.workspace_index(workspace) {
            let workspace = &mut self.workspaces[index];
            workspace.router.disconnected(id);
            let before = workspace.agents.len();
            workspace.agents.retain(|_, agent| agent.client != id);
            if workspace.agents.len() != before {
                self.request_redraw();
            }
        }
    }

    fn channel_line(&mut self, id: usize, line: &str) {
        let Some(workspace) = self
            .sockets
            .get(&id)
            .and_then(|(workspace, _)| self.workspace_index(*workspace))
        else {
            return;
        };
        for out in self.workspaces[workspace].router.handle(id, line) {
            match out {
                Out::Line(to, text) => self.channel_send(to, &text),
                Out::Host(Host::Focus(pane)) => self.focus_named(workspace, &pane),
                Out::Host(Host::Status {
                    client,
                    pane,
                    state,
                    message,
                }) => self.agent_status(workspace, client, pane, state, message),
            }
        }
    }

    fn agent_status(
        &mut self,
        workspace: usize,
        client: usize,
        pane: String,
        state: AgentState,
        message: Option<String>,
    ) {
        let agents = &mut self.workspaces[workspace].agents;
        let raised = state == AgentState::Attention
            && agents.get(&pane).map(|agent| agent.state) != Some(AgentState::Attention);
        let body = message
            .clone()
            .unwrap_or_else(|| format!("the agent in {pane}"));
        agents.insert(
            pane,
            workspace::Agent {
                state,
                message,
                client,
            },
        );
        let id = self.workspaces[workspace].id;
        let watched = self
            .viewers
            .values()
            .any(|viewer| viewer.welcomed && viewer.focused && viewer.workspace == id);
        if raised && !watched {
            let title = format!("{} needs you", self.workspaces[workspace].name);
            let notify = Command::new("notify-send")
                .args(["--app-name=decade", &title, &body])
                .spawn();
            if let Ok(mut child) = notify {
                std::thread::spawn(move || child.wait());
            }
        }
        self.request_redraw();
    }

    /// Return every agent in the workspace that is done or needs attention
    /// to idle, as the person at the screen.
    fn clear_agents(&mut self, workspace: usize) {
        for agent in self.workspaces[workspace].agents.values_mut() {
            if agent.state > AgentState::Working {
                agent.state = AgentState::Idle;
            }
        }
        self.request_redraw();
    }

    fn channel_send(&mut self, to: usize, text: &str) {
        let Some((_, stream)) = self.sockets.get(&to) else {
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
        self.size = (width as i32, height as i32).into();
        self.forget_stale();
        let rescaled = scale != self.scale;
        if rescaled {
            self.scale = scale;
            self.font = Font::load(self.settings.font_size * scale as f32);
            self.menu_font = Font::load(self.settings.palette_size * scale as f32);
            self.texts.clear();
            for client in self
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.panes())
                .flat_map(|pane| &pane.clients)
            {
                self.send_scale(client.toplevel.wl_surface());
            }
        }
        self.resize_layouts();
        self.output.change_current_state(
            Some(Mode {
                size: (width as i32, height as i32).into(),
                refresh: REFRESH_MILLIHERTZ,
            }),
            None,
            rescaled.then_some(Scale::Fractional(scale)),
            None,
        );
        self.relayout_all();
    }

    fn grid(&self) -> (u32, u32) {
        (
            self.size.w as u32 / self.font.cell_width,
            self.size.h as u32 / self.font.cell_height,
        )
    }

    fn pane_area(&self) -> CellRect {
        let (columns, rows) = self.grid();
        frame::pane_area(columns, rows, self.sidebar_open)
    }

    fn resize_layouts(&mut self) {
        let area = self.pane_area();
        for tab in self
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.tabs)
        {
            tab.layout.resize(area);
        }
    }

    fn frame(&self) -> frame::Frame {
        let (columns, rows) = self.grid();
        build_frame(
            &self.workspaces,
            self.current(),
            columns,
            rows,
            self.sidebar_open,
        )
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        self.resize_layouts();
        self.relayout_all();
        self.schedule_save();
    }

    fn frame_hit(&mut self, hit: Hit) {
        match hit {
            Hit::Show(id) => {
                if let Some(active) = self.active {
                    self.show(active, id);
                }
            }
            Hit::Clear(id) => {
                if let Some(index) = self.workspace_index(id) {
                    self.clear_agents(index);
                }
            }
            Hit::New => {
                self.open_palette();
                if let Some(menu) = &mut self.menu {
                    menu.start_new_workspace();
                }
            }
            Hit::Palette => self.open_palette(),
            Hit::Sidebar => self.toggle_sidebar(),
            Hit::Tab(tab) => self.show_tab(tab),
            Hit::NewTab => self.new_tab(),
        }
        self.request_redraw();
    }

    fn open_palette(&mut self) {
        let shown = &self.workspaces[self.current()];
        let (id, name, tab) = (shown.id, shown.name.clone(), shown.active_tab);
        self.menu = Some(Menu::new(
            apps::installed(),
            workspace::home(),
            id,
            &name,
            tab,
        ));
        self.request_redraw();
    }

    fn forget_stale(&mut self) {
        let size = self.size;
        for viewer in self.viewers.values_mut() {
            let stale: Vec<u32> = viewer
                .pool
                .iter()
                .filter(|slot| !slot.lent && slot.size != size)
                .map(|slot| slot.id)
                .collect();
            viewer.pool.retain(|slot| slot.lent || slot.size == size);
            for buffer in stale {
                let _ = viewer
                    .outbox
                    .push_server_message(ServerMessage::Forget { id: buffer });
            }
        }
    }

    fn relayout(&mut self) {
        self.relayout_workspace(self.current());
    }

    fn relayout_all(&mut self) {
        for workspace in 0..self.workspaces.len() {
            self.relayout_workspace(workspace);
        }
    }

    fn schedule_save(&mut self) {
        if self.save_path.is_none() || self.save_due.is_some() {
            return;
        }
        let timer = Timer::from_duration(SAVE_DELAY);
        let due = self.loop_handle.insert_source(timer, |_, _, state| {
            state.save_due = None;
            state.save();
            TimeoutAction::Drop
        });
        self.save_due = due.ok();
    }

    fn save(&mut self) {
        let Some(path) = &self.save_path else {
            return;
        };
        if let Err(error) = save::save(path, &self.workspaces, self.sidebar_open) {
            eprintln!("decade: could not save to {}: {error}", path.display());
        }
    }

    fn relayout_workspace(&mut self, workspace: usize) {
        let panes: Vec<(usize, usize)> = self.workspaces[workspace]
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(tab, entry)| entry.layout.panes.iter().map(move |pane| (tab, pane.id)))
            .collect();
        for (tab, id) in panes {
            self.configure(workspace, tab, id);
        }
        self.request_redraw();
        self.schedule_save();
    }

    fn configure(&mut self, workspace: usize, tab: usize, id: usize) {
        let shown = &self.workspaces[workspace];
        let layout = &shown.tabs[tab].layout;
        let Some(pane) = layout.pane(id) else {
            return;
        };
        let (_, size) = self.client_geometry(pane);
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let focused =
            workspace == self.current() && tab == shown.active_tab && layout.focused == id;
        for (index, client) in pane.clients.iter().enumerate() {
            let toplevel = &client.toplevel;
            toplevel.with_pending_state(|state| {
                state.size = Some(size);
                if focused && index == pane.shown {
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
    }

    fn focus(&mut self, id: usize) {
        if self.layout().pane(id).is_none() {
            return;
        }
        let previous = self.layout().focused;
        self.layout_mut().focused = id;
        let surface = self
            .layout()
            .pane(id)
            .and_then(|pane| pane.toplevel())
            .map(|toplevel| toplevel.wl_surface().clone());
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        let current = self.current();
        let tab = self.workspaces[current].active_tab;
        self.configure(current, tab, previous);
        self.configure(current, tab, id);
        self.chrome_changed();
        self.schedule_save();
    }

    fn chrome_changed(&mut self) {
        self.request_redraw();
    }

    fn focus_named(&mut self, workspace: usize, name: &str) {
        let Some((tab, id)) = self.workspaces[workspace].by_name(name) else {
            return;
        };
        if workspace == self.current() {
            self.show_tab(tab);
            self.focus(id);
        } else {
            let shown = &mut self.workspaces[workspace];
            shown.active_tab = tab;
            shown.tabs[tab].layout.focused = id;
            self.schedule_save();
        }
    }

    fn new_terminal_pane(&mut self) {
        let current = self.current();
        let shown = &mut self.workspaces[current];
        let name = shown.unique_pane_name("terminal");
        let cwd = shown.cwd.clone();
        let layout = shown.layout_mut();
        let id = layout.add_transient(name.clone());
        if let Some(pane) = layout.pane_mut(id) {
            pane.cwd = Some(cwd.clone());
        }
        shown.router.add_pane(name.clone());
        self.spawn_terminal(current, &name, &cwd);
        self.relayout();
        self.focus(id);
    }

    /// Start a terminal as a new tab of the focused pane, in the pane's
    /// directory or else the workspace's.
    fn new_pane_tab(&mut self) {
        let current = self.current();
        let shown = &self.workspaces[current];
        let id = shown.layout().focused;
        let Some(pane) = shown.layout().pane(id) else {
            return;
        };
        let cwd = pane.cwd.clone().unwrap_or_else(|| shown.cwd.clone());
        let tab = shown.active_tab;
        self.spawn_pane_tab(current, tab, id, &cwd);
    }

    /// Show tab `index` of the pane in the current workspace's active tab.
    fn show_pane_tab(&mut self, id: usize, index: usize) {
        let Some(pane) = self.layout_mut().pane_mut(id) else {
            return;
        };
        if index < pane.clients.len() {
            pane.shown = index;
            self.focus(id);
        }
    }

    fn new_tab(&mut self) {
        let current = self.current();
        let area = self.pane_area();
        let (tab, name) = self.workspaces[current].add_tab(area);
        let cwd = self.workspaces[current].cwd.clone();
        self.spawn_terminal(current, &name, &cwd);
        self.show_tab(tab);
    }

    /// Make tab `tab` of the current workspace the one drawn.
    fn show_tab(&mut self, tab: usize) {
        let current = self.current();
        let shown = &mut self.workspaces[current];
        if tab >= shown.tabs.len() || tab == shown.active_tab {
            return;
        }
        let hidden = shown.layout_mut();
        hidden.dragging = None;
        hidden.bar = false;
        hidden.hover = None;
        shown.active_tab = tab;
        self.workspace_shown();
    }

    fn close_tab(&mut self, workspace: u32, tab: usize) {
        let Some(index) = self.workspace_index(workspace) else {
            return;
        };
        let shown = &mut self.workspaces[index];
        if tab >= shown.tabs.len() {
            return;
        }
        let others_closing = shown
            .tabs
            .iter()
            .enumerate()
            .all(|(other, entry)| other == tab || entry.closing);
        if others_closing {
            self.close_workspace(workspace);
            return;
        }
        let closed = &mut shown.tabs[tab];
        closed.closing = true;
        for client in closed.layout.panes.iter().flat_map(|pane| &pane.clients) {
            client.toplevel.send_close();
        }
        self.finish_closing_tab(index, tab);
    }

    fn finish_closing_tab(&mut self, workspace: usize, tab: usize) {
        let shown = &mut self.workspaces[workspace];
        let closed = &shown.tabs[tab];
        let clients = closed
            .layout
            .panes
            .iter()
            .any(|pane| !pane.clients.is_empty());
        if !closed.closing || clients {
            return;
        }
        let removed = shown.tabs.remove(tab);
        for pane in &removed.layout.panes {
            shown.router.remove_pane(&pane.name);
            shown.agents.remove(&pane.name);
        }
        let was_active = shown.active_tab == tab;
        if shown.active_tab > tab || shown.active_tab == shown.tabs.len() {
            shown.active_tab -= 1;
        }
        if was_active && workspace == self.current() {
            self.workspace_shown();
        } else {
            self.relayout_workspace(workspace);
        }
    }

    fn restart_pane(&mut self, workspace: usize, id: usize) {
        let Some(pane) = self.workspaces[workspace].layout().pane(id) else {
            return;
        };
        if !pane.restartable() {
            return;
        }
        let starting = pane.pid.is_some_and(|pid| {
            self.children
                .iter_mut()
                .find(|child| child.id() == pid)
                .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
        });
        if starting {
            return;
        }
        let (name, cwd) = (pane.name.clone(), pane.cwd.clone().unwrap_or_default());
        self.spawn_terminal(workspace, &name, &cwd);
    }

    fn remove_pane(&mut self, workspace: usize, tab: usize, id: usize) {
        let shown = &mut self.workspaces[workspace];
        let layout = &mut shown.tabs[tab].layout;
        let Some(pane) = layout.pane(id) else {
            return;
        };
        let name = pane.name.clone();
        layout.remove(id);
        shown.router.remove_pane(&name);
        shown.agents.remove(&name);
        if shown.scratch_pane() == Some(name.as_str()) {
            shown.scratch = None;
        }
        if shown.closing {
            self.finish_closing(workspace);
            return;
        }
        if shown.tabs[tab].closing {
            self.finish_closing_tab(workspace, tab);
            return;
        }
        self.relayout_workspace(workspace);
        if workspace == self.current() && tab == self.workspaces[workspace].active_tab {
            let focused = self.layout().focused;
            self.focus(focused);
        }
    }

    fn close_pane(&mut self, id: usize) {
        let Some(pane) = self.layout().pane(id) else {
            return;
        };
        let current = self.current();
        let scratched = self.workspaces[current].scratch_pane() == Some(pane.name.as_str());
        match (pane.toplevel(), &pane.kind) {
            (Some(toplevel), _) => toplevel.send_close(),
            (None, PaneKind::Transient) => {
                let tab = self.workspaces[current].active_tab;
                self.remove_pane(current, tab, id);
            }
            (None, PaneKind::Anchor { .. }) if scratched => {
                let tab = self.workspaces[current].active_tab;
                self.remove_pane(current, tab, id);
            }
            (None, PaneKind::Anchor { .. }) => {}
        }
    }

    fn show_bar(&mut self, show: bool) {
        if self.layout().bar != show {
            self.layout_mut().bar = show;
            self.layout_mut().hover = None;
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

    fn pointer_moved(&mut self, location: Point<f64, Logical>, time: u32) {
        self.pointer = location;
        if self.picker.is_some() {
            let point = self.pointer.to_physical(self.scale).to_i32_round();
            let ids: Vec<u32> = self.workspaces.iter().map(|known| known.id).collect();
            if self.picker.as_mut().unwrap().motion(point, &ids) {
                self.request_redraw();
            }
            return;
        }
        match self.drag.take() {
            Some(Drag::Boundary(boundary)) => {
                let (column, row) = self.pointer_cell();
                self.layout_mut().drag_boundary(&boundary, column, row);
                self.drag = Some(Drag::Boundary(boundary));
                self.relayout();
                return;
            }
            Some(Drag::Pane { id, grab, start }) => {
                let (column, row) = self.pointer_cell();
                self.layout_mut().drag_cell = Some((column, row));
                self.drag = Some(Drag::Pane { id, grab, start });
                self.request_redraw();
                return;
            }
            None => {}
        }
        if self.layout().bar {
            let (column, row) = self.pointer_cell();
            let hover = self
                .layout()
                .bar_hit(column, row)
                .filter(|&(_, button)| button != BarButton::Name);
            if hover != self.layout().hover {
                self.layout_mut().hover = hover;
                self.chrome_changed();
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        let focus = self.pane_under_pointer().and_then(|id| {
            let pane = self.layout().pane(id)?;
            let (origin, _) = self.client_geometry(pane);
            pane.toplevel()
                .map(|toplevel| (toplevel.wl_surface().clone(), origin.to_f64()))
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

    fn pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        if self.picker.is_some() {
            if button == BTN_LEFT {
                let point = self.pointer.to_physical(self.scale).to_i32_round();
                let ids: Vec<u32> = self.workspaces.iter().map(|known| known.id).collect();
                let grid = self.picker_grid();
                let cell_height = self.font.cell_height as i32;
                let picker = self.picker.as_mut().unwrap();
                let outcome = if pressed {
                    picker.press(point, &ids, &grid, cell_height)
                } else {
                    picker.release(point, &ids, &grid)
                };
                self.picker_outcome(outcome);
            }
            return;
        }
        if button == BTN_LEFT && pressed {
            let (column, row) = self.pointer_cell();
            if let Some(hit) = self.frame().hit(column, row) {
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

    fn pointer_scroll(&mut self, scroll: Scroll) {
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

    fn keyboard(&mut self, evdev: u32, pressed: bool, time: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let code = Keycode::new(evdev + 8);
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let palette = self.menu.is_some() || self.panel.is_some() || self.picker.is_some();
        let restartable = self
            .layout()
            .pane(self.layout().focused)
            .is_some_and(|pane| pane.restartable());
        let mut control_key = false;
        let key = keyboard.input::<Key, _>(
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
                    return FilterResult::Intercept(Key::Restart);
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
            self.run_key(key);
        }
    }

    fn send_chord(&mut self, chord: Chord) {
        let terminal = self
            .layout()
            .pane(self.layout().focused)
            .and_then(|pane| pane.toplevel())
            .and_then(|toplevel| app_id(toplevel.wl_surface()))
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
        let time = self.start.elapsed().as_millis() as u32;
        let keyboard = self.seat.get_keyboard().unwrap();
        let send = |state: &mut State, key: u32, pressed: KeyState| {
            keyboard.input::<(), _>(
                state,
                Keycode::new(key + 8),
                pressed,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        };
        for &key in &held_super {
            send(self, key, KeyState::Released);
        }
        for &key in keys {
            send(self, key, KeyState::Pressed);
        }
        for &key in keys.iter().rev() {
            send(self, key, KeyState::Released);
        }
        for &key in &held_super {
            send(self, key, KeyState::Pressed);
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
            Key::Clipboard(chord) => self.send_chord(chord),
            Key::Zoom => {
                let focused = self.layout().focused;
                self.layout_mut().toggle_zoom(focused);
                self.relayout();
            }
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
                if let (Some(id), Some(active)) = (self.frame().next_needing(), self.active) {
                    self.show(active, id);
                }
            }
            Key::Picker => self.open_picker(),
            Key::Restart => {
                let focused = self.layout().focused;
                self.restart_pane(self.current(), focused);
            }
            Key::Menu(key) if self.picker.is_some() => {
                let ids: Vec<u32> = self.workspaces.iter().map(|known| known.id).collect();
                let columns = self.picker_grid().columns;
                let outcome = self.picker.as_mut().unwrap().key(key, &ids, columns);
                self.picker_outcome(outcome);
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
                        Some(Outcome::Run(Action::NewWorkspace(cwd))) => {
                            self.menu = None;
                            self.new_workspace(cwd);
                        }
                        Some(Outcome::Run(Action::CloseWorkspace(id))) => {
                            self.menu = None;
                            self.close_workspace(id);
                        }
                        Some(Outcome::Run(Action::CloseTab { workspace, tab })) => {
                            self.menu = None;
                            self.close_tab(workspace, tab);
                        }
                        Some(Outcome::Run(Action::Settings)) => {
                            self.menu = None;
                            self.panel = Some(Panel::new());
                        }
                    }
                }
                self.request_redraw();
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
            self.resize_layouts();
            self.relayout_all();
        }
    }

    fn press_on_chrome(&mut self) -> bool {
        let (column, row) = self.pointer_cell();
        let bar = self.layout().bar_hit(column, row);
        match bar {
            Some((id, BarButton::Close)) => {
                self.close_pane(id);
                return true;
            }
            Some((id, BarButton::Zoom)) => {
                self.layout_mut().toggle_zoom(id);
                self.relayout();
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
        if id != self.layout().focused {
            self.focus(id);
        }
        false
    }

    fn toggle_pin(&mut self, id: usize) {
        let Some(pane) = self.layout().pane(id) else {
            return;
        };
        let client = pane
            .clients
            .first()
            .map(|client| client.toplevel.wl_surface().clone());
        let current = self.current();
        let scratched = self.workspaces[current].scratch_pane() == Some(pane.name.as_str());
        match &pane.kind {
            PaneKind::Anchor { .. } => {
                self.layout_mut().unpin(id);
                if client.is_none() && !scratched {
                    let current = self.current();
                    let tab = self.workspaces[current].active_tab;
                    self.remove_pane(current, tab, id);
                }
            }
            PaneKind::Transient => {
                if let Some(app_id) = client.as_ref().and_then(app_id) {
                    self.layout_mut().pin(id, app_id);
                }
            }
        }
        self.chrome_changed();
        self.schedule_save();
    }

    /// Mark the pane as the workspace's scratch pane, or unmark it. An empty
    /// transient pane that loses the mark is removed.
    fn toggle_scratch(&mut self, id: usize) {
        let Some(pane) = self.layout().pane(id) else {
            return;
        };
        let name = pane.name.clone();
        let current = self.current();
        let shown = &mut self.workspaces[current];
        let before = shown
            .scratch_pane()
            .and_then(|before| shown.by_name(before));
        shown.toggle_scratch(&name);
        if let Some((tab, before)) = before
            && let Some(pane) = shown.tabs[tab].layout.pane(before)
            && pane.clients.is_empty()
            && matches!(pane.kind, PaneKind::Transient)
        {
            self.remove_pane(current, tab, before);
        }
        self.chrome_changed();
        self.schedule_save();
    }

    fn start_pane_drag(&mut self, id: usize) {
        self.layout_mut().hover = None;
        self.focus(id);
        let pointer = self.pointer.to_physical(self.scale).to_i32_round();
        let grab = pointer - self.pane_origin(id);
        self.drag = Some(Drag::Pane {
            id,
            grab,
            start: Instant::now(),
        });
        self.layout_mut().dragging = Some(id);
        self.request_redraw();
    }

    fn drop(&mut self, drag: Drag) {
        let Drag::Pane { id, .. } = drag else {
            return;
        };
        self.layout_mut().dragging = None;
        let drop = self
            .layout_mut()
            .drag_cell
            .take()
            .and_then(|(column, row)| self.layout().drop_at(id, column, row));
        if let Some(drop) = drop {
            self.layout_mut().apply_drop(id, drop);
            self.relayout();
        } else {
            self.request_redraw();
        }
    }

    fn pane_origin(&self, id: usize) -> Point<i32, Physical> {
        let Some(pane) = self.layout().pane(id) else {
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
        self.layout().pane_at(column, row)
    }

    fn client_geometry(&self, pane: &Pane) -> (Point<i32, Logical>, Size<i32, Logical>) {
        let rect = self.layout().client_rect(pane);
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

    fn render(&mut self, target: &mut Dmabuf, workspace: usize) -> Result<Option<OwnedFd>, String> {
        let current = self.current();
        let State {
            gpu,
            size,
            wants_redraw,
            workspaces,
            font,
            menu_font,
            texts,
            start,
            scale,
            drag,
            pointer,
            menu,
            panel,
            picker,
            settings,
            rounded,
            sidebar_open,
            ..
        } = self;
        let layout = workspaces[workspace].layout();
        let scratch = workspaces[workspace].scratch_pane();
        let (menu, panel, drag, picker) = if workspace == current {
            (
                menu.as_ref(),
                panel.as_ref(),
                drag.as_ref(),
                picker.as_ref(),
            )
        } else {
            (None, None, None, None)
        };
        let scale = *scale;
        let size = *size;
        let damage = Rectangle::from_size(size);
        let mut framebuffer = gpu
            .renderer
            .bind(target)
            .map_err(|error| error.to_string())?;
        let mut paint = Paint {
            renderer: &mut gpu.renderer,
            texts,
            font,
            menu_font,
            rounded,
            scale,
        };
        let mut elements: Vec<Element> = Vec::new();

        let overlay_origin = |box_size: Size<i32, Physical>| -> Point<i32, Physical> {
            let top = (size.h / 7).min((size.h - box_size.h).max(0));
            ((size.w - box_size.w) / 2, top).into()
        };
        if let Some(menu) = menu {
            let mut scene = Scene::new(paint.menu_font);
            let box_size = menu.draw(&mut scene, paint.menu_font);
            paint.scene(&mut elements, &scene, overlay_origin(box_size), None);
        }
        if let Some(panel) = panel {
            let mut scene = Scene::new(paint.menu_font);
            let box_size = panel.draw(&mut scene, paint.menu_font, settings);
            paint.scene(&mut elements, &scene, overlay_origin(box_size), None);
        }

        if let Some(picker) = picker {
            let pointer = pointer.to_physical(scale).to_i32_round();
            paint_picker(
                &mut paint,
                &mut elements,
                picker,
                (workspaces, current),
                size,
                pointer,
            );
        }
        if menu.is_some() || panel.is_some() || picker.is_some() {
            let mut scrim = Scene::new(font);
            scrim.rect(0, 0, size.w, size.h, SCRIM);
            paint.scene(&mut elements, &scrim, Point::default(), None);
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
            layout.draw_ghost(pane.id, &mut ghost, scratch);
            paint.scene(&mut elements, &ghost, origin + cell, Some(shrink));
            if let Some(toplevel) = pane.toplevel() {
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
        layout.draw_bars(&mut above, scratch);
        layout.draw_drop(&mut above);
        paint.scene(&mut elements, &above, Point::default(), None);

        for pane in layout.visible().filter(|_| picker.is_none()) {
            let Some(toplevel) = pane.toplevel() else {
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
            let size = (
                (rect.columns * font.cell_width) as i32,
                (rect.rows * font.cell_height) as i32,
            );
            let crop = Rectangle::new(origin.into(), size.into());
            elements.extend(
                surfaces
                    .into_iter()
                    .filter_map(|surface| CropRenderElement::from_element(surface, scale, crop))
                    .map(Element::Surface),
            );
        }

        if picker.is_none() {
            let mut below = Scene::new(font);
            layout.draw(&mut below, scratch);
            let grid = (
                size.w as u32 / font.cell_width,
                size.h as u32 / font.cell_height,
            );
            build_frame(workspaces, workspace, grid.0, grid.1, *sidebar_open).draw(&mut below);
            paint.scene(&mut elements, &below, Point::default(), None);
        }

        let mut frame = paint
            .renderer
            .render(&mut framebuffer, size, Transform::Normal)
            .map_err(|error| error.to_string())?;
        frame
            .clear(
                Color32F::new(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], 1.0),
                &[damage],
            )
            .map_err(|error| error.to_string())?;
        draw_render_elements::<GlesRenderer, _, _>(&mut frame, scale, &elements, &[damage])
            .map_err(|error| error.to_string())?;
        let sync = frame.finish().map_err(|error| error.to_string())?;
        drop(framebuffer);
        texts.end_frame();
        if animating {
            *wants_redraw = true;
        }

        let time = start.elapsed().as_millis() as u32;
        for pane in layout.visible() {
            if let Some(toplevel) = pane.toplevel() {
                send_frames_surface_tree(toplevel.wl_surface(), time);
            }
        }
        Ok(sync.export())
    }
}

fn build_frame(
    workspaces: &[Workspace],
    shown: usize,
    columns: u32,
    rows: u32,
    open: bool,
) -> frame::Frame {
    let entries = workspaces
        .iter()
        .enumerate()
        .map(|(i, workspace)| {
            let urgent = workspace.agents.values().max_by_key(|agent| agent.state);
            frame::Entry {
                id: workspace.id,
                name: workspace.name.clone(),
                state: workspace.agent_state(),
                message: urgent.and_then(|agent| agent.message.clone()),
                current: i == shown,
            }
        })
        .collect();
    let shown = &workspaces[shown];
    let layout = shown.layout();
    let pane = layout
        .pane(layout.focused)
        .map(|pane| pane.name.clone())
        .unwrap_or_default();
    frame::Frame::new(
        columns,
        rows,
        open,
        entries,
        (shown.tabs.len(), shown.active_tab),
        pane,
        layout.zoom.is_some(),
    )
}

fn paint_picker(
    paint: &mut Paint,
    elements: &mut Vec<Element>,
    picker: &Picker,
    (workspaces, current): (&[Workspace], usize),
    size: Size<i32, Physical>,
    pointer: Point<i32, Physical>,
) {
    let (cell_width, cell_height) = (paint.font.cell_width, paint.font.cell_height);
    let (menu_width, menu_height) = (
        paint.menu_font.cell_width as i32,
        paint.menu_font.cell_height as i32,
    );
    let grid = Grid::new(workspaces.len() + 1, size, cell_height as i32);
    let mut over = Scene::new(paint.font);
    let mut under = Scene::new(paint.font);
    let line = over.line_width();
    let label_width = |text: &str| text.chars().count() as i32 * menu_width;
    let drop_target = picker.dragging.and_then(|_| grid.cell_at(pointer));
    let mut surfaces = Vec::new();
    for (i, cell) in grid.cells.iter().enumerate() {
        let (outline, stroke) = if drop_target == Some(i) {
            (palette::CYAN, line)
        } else if picker.selected == i {
            (ACCENT, line * 2)
        } else {
            (palette::MUTED, line)
        };
        for ring in 1..=stroke / line {
            over.box_lines(
                cell.loc.x - line * ring,
                cell.loc.y - line * ring,
                cell.size.w + line * ring * 2,
                cell.size.h + line * ring * 2,
                outline,
            );
        }
        under.rect(
            cell.loc.x,
            cell.loc.y,
            cell.size.w,
            cell.size.h,
            palette::DARK_BACKGROUND,
        );
        let room = (cell.size.w / menu_width).max(1) as usize;
        let label_y = cell.loc.y + cell.size.h + (grid.label - menu_height) / 2;
        let Some(workspace) = workspaces.get(i) else {
            let text = fit("+ New workspace", room);
            let x = cell.loc.x + (cell.size.w - label_width(&text)) / 2;
            let y = cell.loc.y + (cell.size.h - menu_height) / 2;
            over.text_px(x, y, &text, Face::Menu, palette::FOREGROUND);
            continue;
        };
        let confirming = picker.confirming == Some(workspace.id);
        let label = if confirming {
            "close? x again".to_string()
        } else if workspace.closing {
            format!("{} closing", workspace.name)
        } else if i == current {
            format!("● {}", workspace.name)
        } else {
            workspace.name.clone()
        };
        let label = fit(&label, room);
        let label_color = if confirming {
            palette::RED
        } else {
            palette::FOREGROUND
        };
        let x = cell.loc.x + (cell.size.w - label_width(&label)) / 2;
        over.text_px(x, label_y, &label, Face::Menu, label_color);
        let button = grid.close_button(i, cell_height as i32);
        let icon_color = if confirming {
            palette::RED
        } else {
            palette::LIGHT_FOREGROUND
        };
        let icon_x = button.loc.x + (button.size.w - menu_width) / 2;
        let icon_y = button.loc.y + (button.size.h - menu_height) / 2;
        over.text_px(
            icon_x,
            icon_y,
            &CLOSE_ICON.to_string(),
            Face::Menu,
            icon_color,
        );

        let layout = workspace.layout();

        // the cell shows the pane area, not the top bar and sidebar, so
        // positions are taken from the area's corner and scaled to fit it
        let area = layout.area;
        let corner = Point::from((
            (area.column * cell_width) as i32,
            (area.row * cell_height) as i32,
        ));
        let (width, height) = (area.columns * cell_width, area.rows * cell_height);
        let scale = (cell.size.w as f64 / width.max(1) as f64)
            .min(cell.size.h as f64 / height.max(1) as f64);
        let anchor = cell.loc
            + Point::from((
                (cell.size.w - (width as f64 * scale) as i32) / 2,
                (cell.size.h - (height as f64 * scale) as i32) / 2,
            ));
        for pane in layout.visible() {
            let Some(toplevel) = pane.toplevel() else {
                continue;
            };
            let client = layout.client_rect(pane);
            let origin = anchor - corner
                + Point::from((
                    (client.column * cell_width) as i32,
                    (client.row * cell_height) as i32,
                ));
            let tree: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    paint.renderer,
                    toplevel.wl_surface(),
                    origin,
                    paint.scale,
                    1.0,
                    Kind::Unspecified,
                );
            surfaces.extend(tree.into_iter().filter_map(|surface| {
                let scaled = RescaleRenderElement::from_element(surface, anchor, scale);
                CropRenderElement::from_element(scaled, paint.scale, *cell).map(Element::Thumbnail)
            }));
        }
    }
    let hint = fit(
        "tab move · enter show · esc close · x close",
        (size.w / menu_width).max(1) as usize,
    );
    let hint_y = size.h - (cell_height as i32 * 2 + menu_height) / 2;
    let hint_x = (size.w - label_width(&hint)) / 2;
    over.text_px(hint_x, hint_y, &hint, Face::Menu, palette::LIGHT_FOREGROUND);
    paint.scene(elements, &over, Point::default(), None);
    elements.extend(surfaces);
    paint.scene(elements, &under, Point::default(), None);
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
                        Face::Icon => self.font,
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

/// Whether a viewer offer crossed a server offer: the viewer
/// sent it before it had seen the server's newest offer.
fn crossed(seen: u32, newest: u32) -> bool {
    seen < newest
}

fn selection_of(target: SelectionTarget) -> Selection {
    match target {
        SelectionTarget::Clipboard => Selection::Clipboard,
        SelectionTarget::Primary => Selection::Primary,
    }
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
        | Key::Clipboard(_)
        | Key::Swallow => false,
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
        keysyms::KEY_Tab => MenuKey::Tab,
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

const NO_SCRATCH: &str =
    "this workspace has no scratch pane; the person marks one from the Ctrl bar";

fn error_line(reason: &str) -> String {
    serde_json::json!({ "error": { "reason": reason } }).to_string()
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
        self.request_redraw();
        if let Some(i) = self
            .unplaced
            .iter()
            .position(|toplevel| toplevel.wl_surface() == surface)
        {
            let app_id = app_id(surface);
            let toplevel = self.unplaced.remove(i);
            let pid = surface
                .client()
                .and_then(|client| client.get_credentials(&self.display_handle).ok())
                .map(|credentials| credentials.pid as u32);
            let expected = pid
                .and_then(|pid| self.expecting(pid))
                .map(|(index, tab, id)| (index, tab, id, false));
            let placed = expected.or_else(|| {
                self.workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(index, workspace)| {
                        let prefix = workspace.app_id_prefix();
                        workspace.tabs.iter().enumerate().find_map(|(tab, entry)| {
                            let (id, terminal_tab) =
                                entry.layout.place(app_id.as_deref(), &prefix)?;
                            Some((index, tab, id, terminal_tab))
                        })
                    })
            });
            let (workspace, tab, id, terminal_tab) = match placed {
                Some(placed) => placed,
                None => {
                    let workspace = self.current();
                    let shown = &mut self.workspaces[workspace];
                    let name = shown.unique_pane_name(app_id.as_deref().unwrap_or("window"));
                    let id = shown.layout_mut().add_transient(name.clone());
                    shown.router.add_pane(name);
                    let tab = shown.active_tab;
                    self.relayout_workspace(workspace);
                    (workspace, tab, id, false)
                }
            };
            if let Some(pane) = self.workspaces[workspace].tabs[tab].layout.pane_mut(id) {
                if !terminal_tab {
                    pane.app_id = app_id.clone();
                }
                let client = panes::Client {
                    toplevel,
                    app_id,
                    pid,
                    terminal_tab,
                };
                if terminal_tab {
                    pane.clients.push(client);
                    pane.shown = pane.clients.len() - 1;
                } else {
                    pane.clients.insert(0, client);
                    pane.shown = 0;
                }
            }
            self.configure(workspace, tab, id);

            // ghostty draws itself focused until its first keyboard leave,
            // so each new window gets focus once before it returns to the
            // focused pane
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
            let focused = self.layout().focused;
            self.focus(focused);
        }
        if let Some((workspace, tab, id)) = self.pane_of_surface(surface)
            && let Some(pane) = self.workspaces[workspace].tabs[tab].layout.pane(id)
            && pane
                .clients
                .iter()
                .any(|client| !client.toplevel.is_initial_configure_sent())
        {
            self.configure(workspace, tab, id);
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
        let Some((workspace, tab, id)) = self.pane_of_surface(surface.wl_surface()) else {
            return;
        };
        let current = self.current();
        let shown = &mut self.workspaces[workspace];
        let closing = shown.closing || shown.tabs[tab].closing;
        let Some(pane) = shown.tabs[tab].layout.pane_mut(id) else {
            return;
        };
        if let Some(index) = pane
            .clients
            .iter()
            .position(|client| client.toplevel == surface)
        {
            pane.remove_client(index);
        }
        if !pane.clients.is_empty() {
            self.configure(workspace, tab, id);
            if workspace == current && tab == self.workspaces[workspace].active_tab {
                let focused = self.layout().focused;
                self.focus(focused);
            }
            self.request_redraw();
            return;
        }
        let transient = matches!(pane.kind, PaneKind::Transient);
        let scratched = shown
            .scratch
            .as_ref()
            .is_some_and(|scratch| scratch.pane == pane.name);

        // the next program for the scratch pane starts only once its
        // occupant has gone, and not in a workspace or tab that is closing
        let next = shown
            .scratch
            .as_mut()
            .filter(|_| scratched && !closing)
            .and_then(|scratch| scratch.next.take());
        if let Some(next) = next
            && let Err(error) = self.start_scratch(workspace, next)
        {
            eprintln!("decade: {error}");
        }
        let shown = &self.workspaces[workspace];
        if transient && !scratched {
            self.remove_pane(workspace, tab, id);
        } else if shown.closing {
            self.finish_closing(workspace);
        } else if shown.tabs[tab].closing {
            self.finish_closing_tab(workspace, tab);
        } else if workspace == current && tab == shown.active_tab {
            let focused = self.layout().focused;
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
        if self.gpu.renderer.import_dmabuf(&dmabuf, None).is_ok() {
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

    fn focus_changed(&mut self, seat: &Seat<State>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|surface| dh.get_client(surface.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }
    fn cursor_image(&mut self, _: &Seat<State>, image: CursorImageStatus) {
        self.cursor = image;
        let ids: Vec<usize> = self.welcomed().collect();
        for id in ids {
            self.send_cursor(id);
        }
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ViewerOffer;

    fn new_selection(
        &mut self,
        target: SelectionTarget,
        source: Option<SelectionSource>,
        _: Seat<State>,
    ) {
        self.client_selection(target, source.map(|source| source.mime_types()));
    }

    fn send_selection(
        &mut self,
        target: SelectionTarget,
        mime: String,
        fd: OwnedFd,
        _: Seat<State>,
        owner: &ViewerOffer,
    ) {
        self.send(
            owner.viewer,
            ServerMessage::Request {
                selection: selection_of(target),
                serial: owner.serial,
                mime,
                fd,
            },
        );
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
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
delegate_primary_selection!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);

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

    #[test]
    fn a_viewer_offer_crosses_only_a_server_offer_it_had_not_seen() {
        assert!(!crossed(0, 0));
        assert!(!crossed(3, 3));
        assert!(crossed(2, 3));
        assert!(!crossed(4, 3));
    }
}
