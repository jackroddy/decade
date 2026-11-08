use std::os::fd::{AsFd, OwnedFd};
use std::sync::Mutex;

use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
    zwp_primary_selection_offer_v1::{self, ZwpPrimarySelectionOfferV1},
    zwp_primary_selection_source_v1::{self, ZwpPrimarySelectionSourceV1},
};

use super::Viewer;
use crate::wire::{Selection, ViewerMessage};

// the viewer offers this MIME type in every source it
// sets on the host, so it can tell its own selection
// when the host reports it back
const OWN_MIME: &str = "application/x-decade-viewer";

type Mimes = Mutex<Vec<String>>;

/// The host's clipboard and primary selection, as the viewer sees them.
pub(super) struct Clipboard {
    data: Option<(WlDataDeviceManager, WlDataDevice)>,
    primary: Option<(
        ZwpPrimarySelectionDeviceManagerV1,
        ZwpPrimarySelectionDeviceV1,
    )>,

    /// The serial of the latest keyboard enter, key or button press.
    input_serial: Option<u32>,

    /// Each selection, clipboard first.
    selections: [Clip; 2],
}

#[derive(Default)]
struct Clip {
    /// The host's selection, while another host client owns it.
    offer: Option<Offer>,

    /// The serial of the viewer's newest offer or clear.
    serial: u32,

    /// The serial of the newest server offer or clear.
    seen: u32,

    /// The source holding the server's newest offer on the host.
    source: Option<Source>,

    /// Whether the host's selection is the viewer's own source.
    owned: bool,
}

enum Offer {
    Data(WlDataOffer),
    Primary(ZwpPrimarySelectionOfferV1),
}

enum Source {
    Data(WlDataSource),
    Primary(ZwpPrimarySelectionSourceV1),
}

impl Offer {
    fn mimes(&self) -> Vec<String> {
        let mimes = match self {
            Offer::Data(offer) => offer.data::<Mimes>(),
            Offer::Primary(offer) => offer.data::<Mimes>(),
        };
        mimes
            .map(|mimes| mimes.lock().unwrap().clone())
            .unwrap_or_default()
    }

    fn receive(&self, mime: String, fd: OwnedFd) {
        match self {
            Offer::Data(offer) => offer.receive(mime, fd.as_fd()),
            Offer::Primary(offer) => offer.receive(mime, fd.as_fd()),
        }
    }

    fn destroy(&self) {
        match self {
            Offer::Data(offer) => offer.destroy(),
            Offer::Primary(offer) => offer.destroy(),
        }
    }
}

impl Source {
    fn destroy(&self) {
        match self {
            Source::Data(source) => source.destroy(),
            Source::Primary(source) => source.destroy(),
        }
    }
}

impl Clipboard {
    pub(super) fn bind(globals: &GlobalList, qh: &QueueHandle<Viewer>, seat: &WlSeat) -> Clipboard {
        let data = globals
            .bind::<WlDataDeviceManager, _, _>(qh, 1..=3, ())
            .ok()
            .map(|manager| {
                let device = manager.get_data_device(seat, qh, ());
                (manager, device)
            });
        let primary = globals
            .bind::<ZwpPrimarySelectionDeviceManagerV1, _, _>(qh, 1..=1, ())
            .ok()
            .map(|manager| {
                let device = manager.get_device(seat, qh, ());
                (manager, device)
            });
        Clipboard {
            data,
            primary,
            input_serial: None,
            selections: Default::default(),
        }
    }
}

impl Viewer {
    /// Offer the host's selections to a server that has just welcomed the viewer.
    pub(super) fn send_selections(&mut self) {
        let mut messages = Vec::new();
        let selections = [Selection::Clipboard, Selection::Primary];
        for (selection, clip) in selections.into_iter().zip(&self.clipboard.selections) {
            if let Some(offer) = &clip.offer {
                messages.push(ViewerMessage::Offer {
                    selection,
                    serial: clip.serial,
                    seen: clip.seen,
                    mimes: offer.mimes(),
                });
            }
        }
        for message in messages {
            self.send(message);
        }
    }

    pub(super) fn input_serial(&mut self, serial: u32) {
        self.clipboard.input_serial = Some(serial);
    }

    /// Set a server offer the host has not taken yet, now that the window has focus.
    pub(super) fn keyboard_entered(&mut self, serial: u32) {
        self.input_serial(serial);
        self.set_host_selection(Selection::Clipboard);
        self.set_host_selection(Selection::Primary);
    }

    pub(super) fn server_offer(&mut self, selection: Selection, serial: u32, mimes: Vec<String>) {
        let Clipboard {
            data,
            primary,
            selections,
            ..
        } = &mut self.clipboard;
        let clip = &mut selections[selection as usize];
        clip.seen = serial;
        let source = match selection {
            Selection::Clipboard => data.as_ref().map(|(manager, _)| {
                let source = manager.create_data_source(&self.qh, serial);
                for mime in mimes.into_iter().chain([OWN_MIME.to_string()]) {
                    source.offer(mime);
                }
                Source::Data(source)
            }),
            Selection::Primary => primary.as_ref().map(|(manager, _)| {
                let source = manager.create_source(&self.qh, serial);
                for mime in mimes.into_iter().chain([OWN_MIME.to_string()]) {
                    source.offer(mime);
                }
                Source::Primary(source)
            }),
        };
        let old = std::mem::replace(&mut clip.source, source);
        clip.owned = false;
        self.set_host_selection(selection);

        // destroyed after the new source is set, so the
        // host never clears its selection in between
        if let Some(old) = old {
            old.destroy();
        }
    }

    pub(super) fn server_cleared(&mut self, selection: Selection, serial: u32) {
        let clip = &mut self.clipboard.selections[selection as usize];
        clip.seen = serial;
        if let Some(source) = clip.source.take() {
            source.destroy();
        }
    }

    pub(super) fn server_request(
        &mut self,
        selection: Selection,
        serial: u32,
        mime: String,
        fd: OwnedFd,
    ) {
        let clip = &self.clipboard.selections[selection as usize];
        if clip.serial == serial
            && let Some(offer) = &clip.offer
        {
            offer.receive(mime, fd);
        }
    }

    fn set_host_selection(&mut self, selection: Selection) {
        let Clipboard {
            data,
            primary,
            input_serial,
            selections,
        } = &mut self.clipboard;
        let clip = &selections[selection as usize];
        let Some(serial) = *input_serial else {
            return;
        };
        if clip.owned {
            return;
        }
        match &clip.source {
            Some(Source::Data(source)) => {
                if let Some((_, device)) = data {
                    device.set_selection(Some(source), serial);
                }
            }
            Some(Source::Primary(source)) => {
                if let Some((_, device)) = primary {
                    device.set_selection(Some(source), serial);
                }
            }
            None => {}
        }
    }

    fn host_selection(&mut self, selection: Selection, offer: Option<Offer>) {
        let clip = &mut self.clipboard.selections[selection as usize];
        if let Some(old) = clip.offer.take() {
            old.destroy();
        }
        let mimes = offer.as_ref().map(Offer::mimes);
        clip.owned = mimes
            .as_ref()
            .is_some_and(|mimes| mimes.iter().any(|mime| mime == OWN_MIME));
        if clip.owned {
            if let Some(offer) = offer {
                offer.destroy();
            }
            return;
        }
        clip.serial += 1;
        let (serial, seen) = (clip.serial, clip.seen);
        clip.offer = offer;
        self.input(match mimes {
            Some(mimes) => ViewerMessage::Offer {
                selection,
                serial,
                seen,
                mimes,
            },
            None => ViewerMessage::Cleared { selection, serial },
        });
    }

    fn source_cancelled(&mut self, selection: Selection, cancelled: &Source) {
        let clip = &mut self.clipboard.selections[selection as usize];
        let current = match (&clip.source, cancelled) {
            (Some(Source::Data(source)), Source::Data(other)) => source == other,
            (Some(Source::Primary(source)), Source::Primary(other)) => source == other,
            _ => false,
        };
        if current {
            clip.source = None;
            clip.owned = false;
        }
        cancelled.destroy();
    }
}

impl Dispatch<WlDataDevice, ()> for Viewer {
    fn event(
        viewer: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Selection { id } => {
                viewer.host_selection(Selection::Clipboard, id.map(Offer::Data))
            }

            // the viewer takes no drops, and a destroyed offer
            // tells the host so
            wl_data_device::Event::Enter {
                id: Some(offer), ..
            } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(Viewer, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Mimes::default())
    ]);
}

impl Dispatch<WlDataOffer, Mimes> for Viewer {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        mimes: &Mimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            mimes.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, u32> for Viewer {
    fn event(
        viewer: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        serial: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => viewer.input(ViewerMessage::Request {
                selection: Selection::Clipboard,
                serial: *serial,
                mime: mime_type,
                fd,
            }),
            wl_data_source::Event::Cancelled => {
                viewer.source_cancelled(Selection::Clipboard, &Source::Data(source.clone()))
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for Viewer {
    fn event(
        viewer: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_device_v1::Event::Selection { id } = event {
            viewer.host_selection(Selection::Primary, id.map(Offer::Primary));
        }
    }

    event_created_child!(Viewer, ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, Mimes::default())
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, Mimes> for Viewer {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        mimes: &Mimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event {
            mimes.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, u32> for Viewer {
    fn event(
        viewer: &mut Self,
        source: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        serial: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { mime_type, fd } => {
                viewer.input(ViewerMessage::Request {
                    selection: Selection::Primary,
                    serial: *serial,
                    mime: mime_type,
                    fd,
                })
            }
            zwp_primary_selection_source_v1::Event::Cancelled => {
                viewer.source_cancelled(Selection::Primary, &Source::Primary(source.clone()))
            }
            _ => {}
        }
    }
}
