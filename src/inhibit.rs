use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Manager,
    zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1 as Inhibitor,
};

pub struct Shortcuts {
    connection: Connection,
    queue: EventQueue<Ignore>,
    manager: Manager,
    seat: WlSeat,
    surface: WlSurface,
    inhibitor: Option<Inhibitor>,
}

pub struct Ignore;

pub fn inhibit(window: &(impl HasDisplayHandle + HasWindowHandle)) -> Result<Shortcuts, String> {
    let display = window.display_handle().map_err(|error| error.to_string())?;
    let RawDisplayHandle::Wayland(display) = display.as_raw() else {
        return Err("the window is not on Wayland".to_string());
    };
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Wayland(handle) = handle.as_raw() else {
        return Err("the window is not on Wayland".to_string());
    };
    // SAFETY: the pointer is winit's live wl_display, and
    //         State declares its Shortcuts before its winit
    //         backend, so this connection is dropped first
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    let (globals, queue) =
        registry_queue_init::<Ignore>(&connection).map_err(|error| error.to_string())?;
    let handle_queue = queue.handle();
    let manager: Manager = globals
        .bind(&handle_queue, 1..=1, ())
        .map_err(|error| error.to_string())?;
    let seat: WlSeat = globals
        .bind(&handle_queue, 1..=1, ())
        .map_err(|error| error.to_string())?;
    // SAFETY: the pointer is the live wl_surface of the
    //         window passed in, a proxy of that interface
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }
        .map_err(|error| error.to_string())?;
    let surface = WlSurface::from_id(&connection, id).map_err(|error| error.to_string())?;
    let mut shortcuts = Shortcuts {
        connection,
        queue,
        manager,
        seat,
        surface,
        inhibitor: None,
    };
    shortcuts.set(true);
    Ok(shortcuts)
}

impl Shortcuts {
    pub fn set(&mut self, on: bool) {
        match (on, self.inhibitor.take()) {
            (true, None) => {
                let queue = self.queue.handle();
                let inhibitor =
                    self.manager
                        .inhibit_shortcuts(&self.surface, &self.seat, &queue, ());
                self.inhibitor = Some(inhibitor);
            }
            (true, Some(inhibitor)) => self.inhibitor = Some(inhibitor),
            (false, Some(inhibitor)) => inhibitor.destroy(),
            (false, None) => {}
        }
        let _ = self.connection.flush();
    }

    pub fn dispatch(&mut self) {
        let _ = self.queue.dispatch_pending(&mut Ignore);
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Ignore {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for Ignore {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<Manager, ()> for Ignore {
    fn event(
        _: &mut Self,
        _: &Manager,
        _: <Manager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<Inhibitor, ()> for Ignore {
    fn event(
        _: &mut Self,
        _: &Inhibitor,
        _: <Inhibitor as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
