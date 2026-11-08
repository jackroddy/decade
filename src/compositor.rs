mod channels;
mod input;
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
use smithay::input::pointer::CursorImageStatus;
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
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER, Serial, Size, Transform};
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
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::viewporter::ViewporterState;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_shell,
};

use crate::apps;
use crate::children::Children;
use crate::control::{self, viewer_socket_path};
use crate::draw::TextCache;
use crate::files;
use crate::frame::{self, Hit};
use crate::gpu::Gpu;
use crate::mcp;
use crate::menu::Menu;
use crate::panel::Panel;
use crate::panes::{self as panes, BarButton, CellRect, Layout, Pane};
use crate::picker::{self, Grid, Picker};
use crate::save::{self, TerminalTab};
use crate::settings::{self, Settings};
use crate::text::Font;
use crate::wire::{self, OUTBOX_RETRY, ServerMessage};
use crate::workspace::{self, Workspace};

use input::{Drag, Repeat};
use render::build_frame;
use selection::Offered;
use viewers::Viewer;

const REFRESH_MILLIHERTZ: i32 = 60_000;
const PICKER_TICK: Duration = Duration::from_millis(500);
const SAVE_DELAY: Duration = Duration::from_secs(1);
const SAVE_EVERY: Duration = Duration::from_secs(60);

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

    /// Whether the Ctrl bar is shown on the workspace on screen.
    bar: bool,
    hover: Option<(usize, BarButton)>,
    ctrl_timer: Option<RegistrationToken>,
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
        font: Font::load(settings.font_size * scale as f32),
        texts: TextCache::new(),
        menu_font: Font::load(settings.palette_size * scale as f32),
        rounded,
        next_workspace: 0,
        workspaces: Vec::new(),
        unplaced: Vec::new(),
        menu: None,
        picker: None,
        sidebar_open: true,
        picker_tick: None,
        panel: None,
        settings,
        settings_path,
        save_path: save::path(),
        save_due: None,
        pointer: (0.0, 0.0).into(),
        drag: None,
        bar: false,
        hover: None,
        ctrl_timer: None,
        scale,
        start: Instant::now(),
    };
    state.restore();

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
        Ok(server) => Some(server.url),
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
        let taken = self.workspaces.iter().map(|known| known.name.as_str());
        created.name = workspace::unique_name(&created.name, taken);
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

    fn picker_outcome(&mut self, outcome: picker::Outcome) {
        match outcome {
            picker::Outcome::Open => {}
            picker::Outcome::Close => self.close_picker(),
            picker::Outcome::Show(id) => {
                self.close_picker();
                self.show_on_active(id);
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
        self.bar = false;
        self.hover = None;
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
            Hit::Show(id) => self.show_on_active(id),
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
        self.request_redraw();
        self.schedule_save();
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
        layout.remove(id);
        shown.forget_pane(&name);
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
            self.font = Font::load(self.settings.font_size * self.scale as f32);
            self.menu_font = Font::load(self.settings.palette_size * self.scale as f32);
            self.texts.clear();
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
        let rect = self.font.px_rect(pane.client_rect());
        (
            rect.loc.to_f64().to_logical(self.scale).to_i32_round(),
            rect.size.to_f64().to_logical(self.scale).to_i32_round(),
        )
    }

    fn send_scale(&self, surface: &WlSurface) {
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(self.scale);
            });
        });
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
                pane.add_client(panes::Client {
                    toplevel,
                    app_id,
                    pid,
                    terminal_tab,
                });
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
