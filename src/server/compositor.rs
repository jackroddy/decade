mod channels;
mod input;
mod pool;
mod render;
mod selection;
mod viewers;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::gles::{GlesPixelProgram, GlesRenderer, UniformName, UniformType};
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy,
    find_popup_root_surface, get_popup_toplevel_coords,
};
use smithay::input::pointer::CursorImageStatus;
use smithay::input::pointer::Focus;
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
use smithay::reexports::wayland_server::backend::ClientData;
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
use smithay::wayland::selection::data_device::{DataDeviceState, set_data_device_focus};
use smithay::wayland::selection::primary_selection::{PrimarySelectionState, set_primary_focus};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_shell,
};

use crate::control::{self, viewer_socket_path};
use crate::files;
use crate::gpu::Gpu;
use crate::server::apps;
use crate::server::children::Children;
use crate::server::chrome::frame::{self, Hit};
use crate::server::chrome::menu::{Action, Menu, Outcome};
use crate::server::chrome::panel::Panel;
use crate::server::chrome::picker::{Grid, Picker};
use crate::server::chrome::settings::{self, Settings};
use crate::server::chrome::text::Text;
use crate::server::mcp;
use crate::server::panes::{self as panes, BarButton, CellRect, Layout, Pane};
use crate::server::save::{self, TerminalTab};
use crate::server::workspace::{self, Workspace};
use crate::wire::{self, OUTBOX_RETRY, ServerMessage};

use input::{Drag, Repeat};
use selection::Offered;
use viewers::Viewer;

const REFRESH_MILLIHERTZ: i32 = 60_000;
const PICKER_TICK: Duration = Duration::from_millis(500);
const SAVE_DELAY: Duration = Duration::from_secs(1);
const SAVE_EVERY: Duration = Duration::from_secs(60);
const AGE_EVERY: Duration = Duration::from_secs(60);

/// How often a frame is drawn while a point pulses, about 30 a second.
const PULSE_EVERY: Duration = Duration::from_millis(33);

/// The box or grid drawn over the workspace that takes the keys while it is open.
enum Overlay {
    Menu(Menu),
    Panel(Panel),
    Picker {
        picker: Picker,

        /// The timer that sends frame callbacks to hidden workspaces.
        tick: Option<RegistrationToken>,
    },
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
    render_failing: bool,
    stopping: bool,
    cursor: CursorImageStatus,
    display_handle: DisplayHandle,
    loop_handle: LoopHandle<'static, State>,
    sockets: HashMap<usize, (u32, UnixStream)>,
    next_client: usize,
    children: Children,
    text: Text,
    workspaces: Vec<Workspace>,
    next_workspace: u32,
    unplaced: Vec<ToplevelSurface>,
    popups: PopupManager,
    rounded: GlesPixelProgram,
    overlay: Option<Overlay>,
    sidebar_open: bool,
    settings: Settings,
    settings_path: Option<PathBuf>,

    /// Where the workspaces are saved, or `None` when saving is off.
    save_path: Option<PathBuf>,
    save_due: Option<RegistrationToken>,
    pointer: Point<f64, Logical>,
    drag: Option<Drag>,

    /// Whether the Ctrl bar is shown on the workspace on screen.
    bar: bool,
    hover: Option<(usize, BarButton)>,

    /// The pane and index of the pane tab under the pointer.
    tab_hover: Option<(usize, usize)>,

    /// The top bar or sidebar item under the pointer.
    frame_hover: Option<Hit>,
    ctrl_timer: Option<RegistrationToken>,

    /// The timer that draws frames while a point pulses.
    pulse_timer: Option<RegistrationToken>,
    scale: f64,
    start: Instant,
}

pub fn run() {
    let viewer_path = viewer_socket_path();
    let _ = std::fs::create_dir_all(control::dir());
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
    let dmabuf_state = dmabuf_global(&gpu, &dh);
    let rounded = rounded_shader(&mut gpu.renderer);
    let size: Size<i32, Physical> = (1920, 1080).into();
    let scale = 1.0;
    let output = output(&dh, size, scale);
    let repeat = Repeat {
        delay: 200,
        rate: 25,
    };
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "decade");
    seat.add_keyboard(Default::default(), repeat.delay, repeat.rate)
        .unwrap();
    seat.add_pointer();
    let (settings, settings_path) = load_settings();

    let handle = event_loop.handle();
    let socket_name = insert_sources(&handle, display, &viewer_path);
    let runtime = files::runtime_dir();
    remove_stale_sockets(&runtime);
    let mcp = start_mcp(&handle);
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
        render_failing: false,
        stopping: false,
        cursor: CursorImageStatus::default_named(),
        display_handle: dh,
        loop_handle: handle.clone(),
        sockets: HashMap::new(),
        next_client: 0,
        children: Children::new(socket_name, runtime, mcp),
        text: Text::new(&settings, scale),
        rounded,
        next_workspace: 0,
        workspaces: Vec::new(),
        unplaced: Vec::new(),
        popups: PopupManager::default(),
        overlay: None,
        sidebar_open: true,
        settings,
        settings_path,
        save_path: save::path(),
        save_due: None,
        pointer: (0.0, 0.0).into(),
        drag: None,
        bar: false,
        hover: None,
        tab_hover: None,
        frame_hover: None,
        ctrl_timer: None,
        pulse_timer: None,
        scale,
        start: Instant::now(),
    };
    state.restore();

    let every = Timer::from_duration(SAVE_EVERY);
    let _ = handle.insert_source(every, |_, _, state| {
        state.save();
        TimeoutAction::ToDuration(SAVE_EVERY)
    });
    let ages = Timer::from_duration(AGE_EVERY);
    let _ = handle.insert_source(ages, |_, _, state| {
        let now = Instant::now();
        let mut expired = false;
        for shown in &mut state.workspaces {
            expired |= shown.router.agents.expire(now);
        }
        let reported = (state.workspaces.iter()).any(|shown| !shown.router.agents.list.is_empty());
        if expired || state.sidebar_open && reported {
            state.request_redraw();
        }
        TimeoutAction::ToDuration(AGE_EVERY)
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

    state.children.kill_all();
    for workspace in &state.workspaces {
        let _ = std::fs::remove_file(state.children.socket_path(workspace.id));
    }
    let _ = std::fs::remove_file(&viewer_path);
}

/// The linux-dmabuf global, offering the GPU's formats and naming the GPU
/// as the device clients render on.
fn dmabuf_global(gpu: &Gpu, dh: &DisplayHandle) -> DmabufState {
    let mut dmabuf_state = DmabufState::new();
    let formats = gpu.renderer.dmabuf_formats();
    let feedback = DmabufFeedbackBuilder::new(gpu.node.dev_id(), formats.clone())
        .build()
        .map_err(|error| error.to_string());
    match feedback {
        Ok(feedback) => {
            dmabuf_state.create_global_with_default_feedback::<State>(dh, &feedback);
        }
        Err(error) => {
            eprintln!(
                "decade: clients get no GPU to render on and may fall back to software: {error}"
            );
            dmabuf_state.create_global::<State>(dh, formats);
        }
    }
    dmabuf_state
}

fn rounded_shader(renderer: &mut GlesRenderer) -> GlesPixelProgram {
    renderer
        .compile_custom_pixel_shader(
            include_str!("rounded.frag"),
            &[
                UniformName::new("fill", UniformType::_4f),
                UniformName::new("border", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("border_width", UniformType::_1f),
            ],
        )
        .expect("the rounded rect shader compiles")
}

fn output(dh: &DisplayHandle, size: Size<i32, Physical>, scale: f64) -> Output {
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
    output.create_global::<State>(dh);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Fractional(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    output
}

/// The settings, and the path to write them to, or `None` when the file
/// there could not be read.
fn load_settings() -> (Settings, Option<PathBuf>) {
    let Some(path) = settings::path() else {
        return (Settings::default(), None);
    };
    match settings::load(&path) {
        Ok(settings) => (settings, Some(path)),
        Err(error) => {
            eprintln!(
                "decade: {} could not be read, so decade uses the defaults and will not write it: {error}",
                path.display()
            );
            (Settings::default(), None)
        }
    }
}

/// Listen for Wayland clients, viewers and the signals that stop the
/// server, and return the Wayland socket's name.
fn insert_sources(
    handle: &LoopHandle<'static, State>,
    display: Display<State>,
    viewer_path: &Path,
) -> OsString {
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
    handle
        .insert_source(Signals::new(&[Signal::SIGCHLD]).unwrap(), |_, _, state| {
            state.children.reap();
        })
        .unwrap();

    let _ = std::fs::remove_file(viewer_path);
    let viewer_listener = UnixListener::bind(viewer_path).unwrap();
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
    socket_name
}

/// Start the MCP server, whose requests arrive on the event loop, and
/// return its address.
fn start_mcp(handle: &LoopHandle<'static, State>) -> Option<String> {
    let (mcp_requests, mcp_source) = calloop_channel::channel();
    handle
        .insert_source(mcp_source, |event, _, state| {
            if let ChannelEvent::Msg(request) = event {
                state.mcp_request(request);
            }
        })
        .unwrap();
    match mcp::start(mcp_requests) {
        Ok(url) => Some(url),
        Err(error) => {
            eprintln!("decade: the MCP server did not start: {error}");
            None
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
    fn launch(&mut self, argv: &[String]) {
        let Some((program, args)) = argv.split_first() else {
            return;
        };
        let workspace = &self.workspaces[self.current()];
        let mut command = self.children.command(workspace, program, None);
        command.args(args);
        if let Err(error) = self.children.spawn(command) {
            eprintln!("decade: {error}");
        }
    }

    /// Put back the saved workspaces, or add the home workspace when none
    /// were saved.
    fn restore(&mut self) {
        if let Some(path) = self.save_path.clone() {
            let restored = save::load(&path).and_then(|file| {
                let Some(file) = file else {
                    return Ok(Vec::new());
                };
                self.sidebar_open = file.sidebar_open;
                let area = self.pane_area();
                (file.workspaces.iter().enumerate())
                    .map(|(id, saved)| saved.workspace(id as u32, area))
                    .collect::<Result<Vec<_>, String>>()
            });
            match restored {
                Ok(workspaces) => {
                    for (workspace, tabs) in workspaces {
                        self.workspaces.push(workspace);
                        self.next_workspace += 1;
                        self.start_workspace(self.workspaces.len() - 1, tabs);
                    }
                }
                Err(error) => {
                    eprintln!(
                        "decade: {} could not be read, so decade starts fresh and will not save over it: {error}",
                        path.display()
                    );
                    self.save_path = None;
                    self.sidebar_open = true;
                }
            }
        }
        if self.workspaces.is_empty() {
            self.add_workspace(workspace::home());
        }
    }

    /// Listen on the workspace's socket, and start a terminal for every pane
    /// that holds one and for every terminal tab in `tabs`.
    fn start_workspace(&mut self, workspace: usize, tabs: Vec<TerminalTab>) {
        self.listen(workspace);
        let terminals: Vec<(usize, usize)> = (self.workspaces[workspace].tabs.iter().enumerate())
            .flat_map(|(tab, entry)| {
                (entry.layout.panes.iter())
                    .filter(|pane| pane.cwd.is_some())
                    .map(move |pane| (tab, pane.id))
            })
            .collect();
        for (tab, id) in terminals {
            self.spawn_terminal(workspace, tab, id);
        }
        for saved in tabs {
            self.spawn_pane_tab(workspace, saved.tab, saved.pane, &saved.cwd);
        }
    }

    /// Start a ghostty that goes in the pane as a new tab.
    fn spawn_pane_tab(&mut self, workspace: usize, tab: usize, id: usize, cwd: &Path) {
        let Some(pane) = self.workspaces[workspace].tabs[tab].layout.pane_mut(id) else {
            return;
        };
        let class = format!("{}.t{}", pane.name, pane.next_tab);
        pane.next_tab += 1;
        let name = pane.name.clone();
        let ghostty = self
            .children
            .ghostty(&self.workspaces[workspace], &name, &class, cwd);
        if let Err(error) = self.children.spawn(ghostty) {
            eprintln!("decade: {error}");
        }
    }

    /// Start the ghostty of a pane that holds a terminal, in the pane's cwd.
    fn spawn_terminal(&mut self, workspace: usize, tab: usize, id: usize) {
        let shown = &self.workspaces[workspace];
        let Some(pane) = shown.tabs[tab].layout.pane(id) else {
            return;
        };
        let cwd = pane.cwd.as_deref().unwrap_or(&shown.cwd);
        let ghostty = self.children.ghostty(shown, &pane.name, &pane.name, cwd);
        match self.children.spawn(ghostty) {
            Ok(pid) => {
                let layout = &mut self.workspaces[workspace].tabs[tab].layout;
                layout.pane_mut(id).unwrap().pid = Some(pid);
            }
            Err(error) => eprintln!("decade: {error}"),
        }
    }

    fn now(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn current(&self) -> usize {
        self.active
            .and_then(|id| self.viewers.get(&id))
            .and_then(|viewer| self.workspace_index(viewer.workspace))
            .unwrap_or(0)
    }

    /// Focus the focused pane again, if the tab is the one on screen.
    fn refocus(&mut self, workspace: usize, tab: usize) {
        if workspace == self.current() && tab == self.workspaces[workspace].active_tab {
            let focused = self.layout().focused;
            self.focus(focused);
        }
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

    fn add_workspace(&mut self, cwd: PathBuf) -> u32 {
        let id = self.next_workspace;
        self.next_workspace += 1;
        let mut created = Workspace::new(id, cwd, self.pane_area());
        created.name = workspace::unique_name(&created.name, " ", |name| {
            self.workspaces.iter().all(|known| known.name != name)
        });
        self.workspaces.push(created);
        self.start_workspace(self.workspaces.len() - 1, Vec::new());
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
        self.show_on_active(id);
    }

    fn close_workspace(&mut self, id: u32) {
        let Some(index) = self.workspace_index(id) else {
            return;
        };
        for tab in 0..self.workspaces[index].tabs.len() {
            self.mark_closing(index, tab);
        }
        self.finish_closing(index);
    }

    fn close_tab(&mut self, workspace: u32, tab: usize) {
        let Some(index) = self.workspace_index(workspace) else {
            return;
        };
        if tab >= self.workspaces[index].tabs.len() {
            return;
        }
        self.mark_closing(index, tab);
        self.finish_closing(index);
    }

    /// Mark the tab closing and ask every client in it to close.
    fn mark_closing(&mut self, workspace: usize, tab: usize) {
        let closed = &mut self.workspaces[workspace].tabs[tab];
        closed.closing = true;
        for client in closed.layout.panes.iter().flat_map(|pane| &pane.clients) {
            client.toplevel.send_close();
        }
    }

    /// Remove the workspace's closing tabs that no client is left in, and
    /// the workspace once its last tab has gone.
    fn finish_closing(&mut self, workspace: usize) {
        let closed: Vec<usize> = self.workspaces[workspace].closed_tabs().collect();
        if closed.is_empty() {
            return;
        }
        if closed.len() == self.workspaces[workspace].tabs.len() {
            self.remove_workspace(workspace);
            return;
        }
        let mut active_removed = false;
        for &tab in closed.iter().rev() {
            active_removed |= self.workspaces[workspace].remove_tab(tab);
        }
        if active_removed && workspace == self.current() {
            self.workspace_shown();
        } else {
            self.relayout_workspace(workspace);
        }
    }

    fn remove_workspace(&mut self, index: usize) {
        let removed = self.workspaces.remove(index);
        if let Some(token) = removed.listener {
            self.loop_handle.remove(token);
        }
        let _ = std::fs::remove_file(self.children.socket_path(removed.id));
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
        for &viewer in &moved {
            self.set_shown(viewer, first);
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
            self.text.chrome.cell_height as i32,
        )
    }

    fn workspace_ids(&self) -> Vec<u32> {
        self.workspaces.iter().map(|known| known.id).collect()
    }

    fn open_picker(&mut self) {
        if matches!(self.overlay, Some(Overlay::Picker { .. })) {
            return;
        }
        let tick =
            self.loop_handle
                .insert_source(Timer::from_duration(PICKER_TICK), |_, _, state| {
                    state.tick_hidden();
                    TimeoutAction::ToDuration(PICKER_TICK)
                });
        self.overlay = Some(Overlay::Picker {
            picker: Picker::new(self.current()),
            tick: tick.ok(),
        });
        self.tick_hidden();
    }

    fn close_overlay(&mut self) {
        if let Some(Overlay::Picker {
            tick: Some(token), ..
        }) = self.overlay.take()
        {
            self.loop_handle.remove(token);
        }
        self.request_redraw();
    }

    fn tick_hidden(&mut self) {
        let time = self.now();
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

    fn act(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Open => {}
            Outcome::Close => self.close_overlay(),
            Outcome::Run(action) => {
                self.close_overlay();
                self.run_action(action);
            }
            Outcome::Apply(action) => self.run_action(action),
        }
        if let Some(Overlay::Picker { picker, .. }) = &mut self.overlay {
            picker.clamp(self.workspaces.len() + 1);
        }
        self.request_redraw();
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::Launch(argv) => self.launch(&argv),
            Action::Settings => self.overlay = Some(Overlay::Panel(Panel::new())),
            Action::NewWorkspace(cwd) => self.new_workspace(cwd),
            Action::CloseWorkspace(id) => self.close_workspace(id),
            Action::CloseTab { workspace, tab } => self.close_tab(workspace, tab),
            Action::ShowWorkspace(id) => self.show_on_active(id),
            Action::StartNewWorkspace => self.start_new_workspace(),
            Action::MoveWorkspace { from, to } => {
                let moved = self.workspaces.remove(from);
                self.workspaces.insert(to, moved);
                self.send_workspace_list();
            }
            Action::ChangeSettings(settings) => {
                let before = std::mem::replace(&mut self.settings, settings);
                self.settings_changed(before);
            }
        }
    }

    fn workspace_shown(&mut self) {
        self.drag = None;
        self.bar = false;
        self.hover = None;
        self.tab_hover = None;
        self.relayout_all();
        let focused = self.layout().focused;
        self.focus(focused);
    }

    fn workspace_index(&self, id: u32) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|workspace| workspace.id == id)
    }

    fn resized(&mut self, width: u32, height: u32, scale: f64) {
        self.size = (width as i32, height as i32).into();
        self.forget_stale();
        let rescaled = scale != self.scale;
        if rescaled {
            self.scale = scale;
            self.text = Text::new(&self.settings, scale);
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
            self.size.w as u32 / self.text.chrome.cell_width,
            self.size.h as u32 / self.text.chrome.cell_height,
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

    fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        self.frame_hover = None;
        self.resize_layouts();
        self.relayout_all();
        self.schedule_save();
    }

    fn frame_hit(&mut self, hit: Hit) {
        match hit {
            Hit::Show(id) => self.show_on_active(id),
            Hit::Clear(id) => {
                if let Some(index) = self.workspace_index(id) {
                    self.clear_agents(index);
                }
            }
            Hit::New => self.start_new_workspace(),
            Hit::Palette => self.open_palette(),
            Hit::Sidebar => self.toggle_sidebar(),
            Hit::Tab(tab) => self.show_tab(tab),
            Hit::NewTab => self.new_tab(),
            Hit::Next(state) => {
                if let Some(id) = self.frame(self.current()).next_in(state) {
                    self.show_on_active(id);
                }
            }
        }
        self.request_redraw();
    }

    fn open_palette(&mut self) {
        let shown = &self.workspaces[self.current()];
        let (id, name, tab) = (shown.id, shown.name.clone(), shown.active_tab);
        let menu = Menu::new(apps::installed(), workspace::home(), id, &name, tab);
        self.close_overlay();
        self.overlay = Some(Overlay::Menu(menu));
    }

    fn start_new_workspace(&mut self) {
        self.open_palette();
        if let Some(Overlay::Menu(menu)) = &mut self.overlay {
            menu.start_new_workspace();
        }
    }

    fn forget_stale(&mut self) {
        for viewer in self.viewers.values_mut() {
            viewer.pool.forget_stale(self.size, &mut viewer.outbox);
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

    fn save(&self) {
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

    fn configure(&self, workspace: usize, tab: usize, id: usize) {
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
        if previous != id {
            self.end_point_at(id);
        }
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
        self.request_redraw();
        self.schedule_save();
    }

    /// End the point in the current workspace if it is at pane `id`.
    fn end_point_at(&mut self, id: usize) {
        let Some(name) = self.layout().pane(id).map(|pane| pane.name.clone()) else {
            return;
        };
        let current = self.current();
        self.workspaces[current].end_point_at(&name);
    }

    /// Draw frames while any workspace's point pulses, and end points
    /// as they expire.
    fn start_pulse(&mut self) {
        if self.pulse_timer.is_some() {
            return;
        }
        let timer = Timer::from_duration(PULSE_EVERY);
        let token = self.loop_handle.insert_source(timer, |_, _, state| {
            let now = Instant::now();
            let mut pulsing = false;
            for shown in &mut state.workspaces {
                pulsing |= shown.keep_point(now);
            }
            state.request_redraw();
            if pulsing {
                TimeoutAction::ToDuration(PULSE_EVERY)
            } else {
                state.pulse_timer = None;
                TimeoutAction::Drop
            }
        });
        self.pulse_timer = token.ok();
    }

    fn new_terminal_pane(&mut self) {
        let current = self.current();
        let shown = &mut self.workspaces[current];
        let tab = shown.active_tab;
        let cwd = shown.cwd.clone();
        let id = shown.add_pane(tab, "terminal", Some(cwd));
        self.spawn_terminal(current, tab, id);
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
        if pane.show(index) {
            self.focus(id);
        }
    }

    fn new_tab(&mut self) {
        let current = self.current();
        let area = self.pane_area();
        let (tab, _) = self.workspaces[current].add_tab(area);
        let id = self.workspaces[current].tabs[tab].layout.focused;
        self.spawn_terminal(current, tab, id);
        self.show_tab(tab);
    }

    /// Make tab `tab` of the current workspace the one drawn.
    fn show_tab(&mut self, tab: usize) {
        let current = self.current();
        let shown = &mut self.workspaces[current];
        if tab >= shown.tabs.len() || tab == shown.active_tab {
            return;
        }
        shown.active_tab = tab;
        self.workspace_shown();
    }

    fn restart_pane(&mut self, workspace: usize, id: usize) {
        let Some(pane) = self.workspaces[workspace].layout().pane(id) else {
            return;
        };
        if !pane.restartable() {
            return;
        }
        if pane.pid.is_some_and(|pid| self.children.running(pid)) {
            return;
        }
        let tab = self.workspaces[workspace].active_tab;
        self.spawn_terminal(workspace, tab, id);
    }

    fn remove_pane(&mut self, workspace: usize, tab: usize, id: usize) {
        let shown = &mut self.workspaces[workspace];
        let layout = &mut shown.tabs[tab].layout;
        let Some(pane) = layout.pane(id) else {
            return;
        };
        let name = pane.name.clone();
        let removed = pane.pid;
        layout.remove(id);
        shown.forget_pane(&name);
        let children = &mut self.children;

        // the removed pane's program can outlive its window
        // for a moment, and a tab's last pane stays in the
        // layout with its pid, so that pid never holds the
        // tab open
        let running = |pid| Some(pid) != removed && children.running(pid);
        if shown.tab_is_spent(tab, running) {
            shown.tabs[tab].closing = true;
        }
        if shown.tabs[tab].closing {
            self.finish_closing(workspace);
            return;
        }
        self.relayout_workspace(workspace);
        self.refocus(workspace, tab);
    }

    /// Remove the pane if it has no client, is not pinned and is not the
    /// scratch pane, and return whether it went.
    fn prune(&mut self, workspace: usize, tab: usize, id: usize) -> bool {
        let shown = &self.workspaces[workspace];
        let Some(pane) = shown.tabs[tab].layout.pane(id) else {
            return false;
        };
        if !pane.clients.is_empty() || shown.keeps_empty(pane) {
            return false;
        }
        self.remove_pane(workspace, tab, id);
        true
    }

    fn close_pane(&mut self, id: usize) {
        let Some(pane) = self.layout().pane(id) else {
            return;
        };
        let current = self.current();
        let scratched = self.workspaces[current].is_scratch(pane);
        match pane.toplevel() {
            Some(toplevel) => toplevel.send_close(),
            None if !pane.pinned || scratched => {
                let tab = self.workspaces[current].active_tab;
                self.remove_pane(current, tab, id);
            }
            None => {}
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
            self.text = Text::new(&self.settings, self.scale);
            self.resize_layouts();
            self.relayout_all();
        }
    }

    fn toggle_pin(&mut self, id: usize) {
        let Some(pane) = self.layout().pane(id) else {
            return;
        };
        let pinned = pane.pinned;
        let app_id =
            (pane.clients.first()).and_then(|client| panes::app_id(client.toplevel.wl_surface()));
        if pinned {
            self.layout_mut().unpin(id);
            let current = self.current();
            let tab = self.workspaces[current].active_tab;
            self.prune(current, tab, id);
        } else if let Some(app_id) = app_id {
            self.layout_mut().pin(id, app_id);
        }
        self.request_redraw();
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
        if let Some((tab, before)) = before {
            self.prune(current, tab, before);
        }
        self.request_redraw();
        self.schedule_save();
    }

    fn client_geometry(&self, pane: &Pane) -> (Point<i32, Logical>, Size<i32, Logical>) {
        let rect = self.text.chrome.px_rect(pane.client_rect());
        (
            rect.loc.to_f64().to_logical(self.scale).to_i32_round(),
            rect.size.to_f64().to_logical(self.scale).to_i32_round(),
        )
    }

    /// Give `popup` the positioner it asked for and the place it takes,
    /// kept inside the client area of the pane showing its toplevel.
    fn place_popup(&self, popup: &PopupSurface, positioner: PositionerState) {
        let kind = PopupKind::Xdg(popup.clone());
        let bounds = find_popup_root_surface(&kind).ok().and_then(|root| {
            let (workspace, tab, id) = self.pane_of_surface(&root)?;
            let pane = self.workspaces[workspace].tabs[tab].layout.pane(id)?;
            let (_, client) = self.client_geometry(pane);
            Some((
                client,
                window_origin(&root) + get_popup_toplevel_coords(&kind),
            ))
        });
        let geometry = match bounds {
            Some((client, parent)) => popup_geometry(positioner, client, parent),
            None => positioner.get_geometry(),
        };
        popup.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = geometry;
        });
    }

    fn send_scale(&self, surface: &WlSurface) {
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(self.scale);
            });
        });
    }
}

/// Where the corner of `surface`'s window geometry sits from the corner of
/// its buffer.
fn window_origin(surface: &WlSurface) -> Point<i32, Logical> {
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        cached.current().geometry.map(|geometry| geometry.loc)
    })
    .unwrap_or_default()
}

/// The popups open over `toplevel`, topmost first, each with where its
/// buffer's corner sits from the corner of the toplevel's buffer.
fn popups_over(toplevel: &WlSurface) -> impl Iterator<Item = (PopupKind, Point<i32, Logical>)> {
    let origin = window_origin(toplevel);
    PopupManager::popups_for_surface(toplevel).map(move |(popup, offset)| {
        let at = origin + offset - popup.geometry().loc;
        (popup, at)
    })
}

/// The rectangle a popup takes, relative to its parent's window geometry,
/// moved by its positioner to fit a pane's client area of size `client`
/// where the parent's window geometry has its corner at `parent`.
fn popup_geometry(
    positioner: PositionerState,
    client: Size<i32, Logical>,
    parent: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let area = Rectangle::new((-parent.x, -parent.y).into(), client);
    positioner.get_unconstrained_geometry(area)
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

impl ClientData for ClientState {}

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
        self.popups.commit(surface);
        self.popups.cleanup();
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            let _ = popup.send_configure();
        }
        if let Some(i) = self
            .unplaced
            .iter()
            .position(|toplevel| toplevel.wl_surface() == surface)
        {
            let app_id = panes::app_id(surface);
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
                        let prefix = workspace::app_id_prefix(workspace.id);
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
                    let tab = shown.active_tab;
                    let id = shown.add_pane(tab, app_id.as_deref().unwrap_or("window"), None);
                    self.relayout_workspace(workspace);
                    (workspace, tab, id, false)
                }
            };
            if let Some(pane) = self.workspaces[workspace].tabs[tab].layout.pane_mut(id) {
                pane.add_client(panes::Client::new(toplevel, app_id, pid, terminal_tab));
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
        let shown = &mut self.workspaces[workspace];
        let closing = shown.tabs[tab].closing;
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
            self.refocus(workspace, tab);
            self.request_redraw();
            return;
        }
        let scratched = (shown.scratch.as_ref()).is_some_and(|scratch| scratch.pane == pane.name);

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
        if self.prune(workspace, tab, id) {
            return;
        }
        if self.workspaces[workspace].tabs[tab].closing {
            self.finish_closing(workspace);
        } else {
            self.refocus(workspace, tab);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        self.place_popup(&surface, positioner);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else {
            return;
        };
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let Ok(mut grab) = self.popups.grab_popup(root, popup, &seat, serial) else {
            return;
        };
        let keyboard = seat.get_keyboard().unwrap();
        let previous = grab.previous_serial().unwrap_or(serial);
        if keyboard.is_grabbed() && !keyboard.has_grab(serial) && !keyboard.has_grab(previous) {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        keyboard.set_focus(self, grab.current_grab(), serial);
        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        let pointer = seat.get_pointer().unwrap();
        let previous = grab.previous_serial().unwrap_or_else(|| grab.serial());
        if pointer.is_grabbed() && !pointer.has_grab(serial) && !pointer.has_grab(previous) {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        self.place_popup(&surface, positioner);
        surface.send_repositioned(token);
    }
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
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
        Anchor, ConstraintAdjustment, Gravity,
    };

    use super::*;

    #[test]
    fn a_popup_requested_near_the_edge_of_a_pane_is_kept_inside_the_pane() {
        let client = Size::from((400, 300));
        let parent = Point::from((10, 20));
        let positioner = PositionerState {
            rect_size: (200, 150).into(),
            anchor_rect: Rectangle::new((360, 260).into(), (1, 1).into()),
            anchor_edges: Anchor::BottomRight,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY,
            ..PositionerState::default()
        };
        let mut placed = popup_geometry(positioner, client, parent);
        placed.loc += parent;
        assert!(
            Rectangle::from_size(client).contains_rect(placed),
            "{placed:?}"
        );
        assert_eq!(placed.size, positioner.rect_size);
    }
}
