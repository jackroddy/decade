use wayland_client::protocol::{wl_seat::WlSeat, wl_surface::WlSurface};
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Manager,
    zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1 as Inhibitor,
};

/// The host's keyboard shortcuts, held off while a surface has focus.
pub struct Shortcuts {
    manager: Manager,
    inhibitor: Option<Inhibitor>,
}

impl Shortcuts {
    pub fn new(manager: Manager) -> Shortcuts {
        Shortcuts {
            manager,
            inhibitor: None,
        }
    }

    pub fn set<D>(&mut self, on: bool, surface: &WlSurface, seat: &WlSeat, queue: &QueueHandle<D>)
    where
        D: Dispatch<Inhibitor, ()> + 'static,
    {
        match (on, self.inhibitor.take()) {
            (true, None) => {
                let inhibitor = self.manager.inhibit_shortcuts(surface, seat, queue, ());
                self.inhibitor = Some(inhibitor);
            }
            (true, Some(inhibitor)) => self.inhibitor = Some(inhibitor),
            (false, Some(inhibitor)) => inhibitor.destroy(),
            (false, None) => {}
        }
    }
}
