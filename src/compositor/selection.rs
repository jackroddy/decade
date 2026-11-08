use std::os::unix::io::OwnedFd;

use smithay::input::Seat;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, clear_primary_selection,
    current_primary_selection_userdata, request_primary_client_selection, set_primary_selection,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};

use crate::wire::{Selection, ServerMessage};

use super::State;

impl State {
    pub(super) fn viewer_offer(
        &mut self,
        id: usize,
        selection: Selection,
        serial: u32,
        seen: u32,
        mimes: Vec<String>,
    ) {
        let offered = &mut self.offered[selection as usize];

        // the viewer sent the offer before it had seen the server's newest one
        if seen < offered.serial {
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

    /// Pass a viewer's request for the selection's data to the client
    /// that owns it, if the request is for the server's newest offer.
    pub(super) fn viewer_request(
        &mut self,
        selection: Selection,
        serial: u32,
        mime: String,
        fd: OwnedFd,
    ) {
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

    pub(super) fn viewer_owns(&self, selection: Selection, id: usize) -> bool {
        match selection {
            Selection::Clipboard => current_data_device_selection_userdata(&self.seat)
                .is_some_and(|owner| owner.viewer == id),
            Selection::Primary => current_primary_selection_userdata(&self.seat)
                .is_some_and(|owner| owner.viewer == id),
        }
    }

    pub(super) fn clear_selection(&mut self, selection: Selection) {
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

    pub(super) fn tell_selection(&mut self, id: usize, selection: Selection) {
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
}

#[derive(Default)]
pub(super) struct Offered {
    /// The serial of the server's newest offer or clear.
    pub(super) serial: u32,

    /// The MIME types of a client's selection, while the newest offer stands.
    pub(super) mimes: Option<Vec<String>>,
}

/// The viewer offer that a selection the server holds for its clients came from.
#[derive(Clone)]
pub struct ViewerOffer {
    viewer: usize,
    serial: u32,
}

fn selection_of(target: SelectionTarget) -> Selection {
    match target {
        SelectionTarget::Clipboard => Selection::Clipboard,
        SelectionTarget::Primary => Selection::Primary,
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

impl ServerDndGrabHandler for State {}
