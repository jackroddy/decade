use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
use smithay::utils::{Physical, Size};

use crate::gpu::Gpu;
use crate::wire::{Format, Outbox, Plane, ServerMessage};

const POOL_SIZE: usize = 3;

/// The frame buffers the server has lent or may lend to one viewer.
pub struct Pool {
    /// The formats the viewer's host imports, once `formats` has arrived.
    formats: Option<Vec<(Fourcc, Modifier)>>,
    linear: bool,
    slots: Vec<Slot>,
    next_id: u32,
}

struct Slot {
    id: u32,
    dmabuf: Dmabuf,
    size: Size<i32, Physical>,
    lent: bool,
}

impl Pool {
    pub fn new() -> Pool {
        Pool {
            formats: None,
            linear: false,
            slots: Vec::new(),
            next_id: 0,
        }
    }

    pub fn set_formats(&mut self, formats: &[Format]) {
        let formats = formats
            .iter()
            .filter_map(|format| {
                let code = Fourcc::try_from(format.format).ok()?;
                Some((code, Modifier::from(format.modifier)))
            })
            .collect();
        self.formats = Some(formats);
    }

    pub fn has_formats(&self) -> bool {
        self.formats.is_some()
    }

    /// Find a buffer of `size` that is not lent out, or allocate one and
    /// announce it in `outbox`. None when every buffer of `size` is lent.
    pub fn slot(
        &mut self,
        size: Size<i32, Physical>,
        gpu: &mut Gpu,
        outbox: &mut Outbox,
    ) -> Option<(u32, Dmabuf)> {
        if let Some(slot) = self
            .slots
            .iter()
            .find(|slot| !slot.lent && slot.size == size)
        {
            return Some((slot.id, slot.dmabuf.clone()));
        }
        let current = self.slots.iter().filter(|slot| slot.size == size).count();
        if current >= POOL_SIZE {
            return None;
        }
        let (width, height) = (size.w as u32, size.h as u32);
        let accepted = match &self.formats {
            Some(formats) if !self.linear => formats.as_slice(),
            _ => &[],
        };
        let allocated = match gpu.allocate(width, height, accepted) {
            Err(error) if !accepted.is_empty() => {
                eprintln!(
                    "decade: allocating a frame buffer failed, so decade falls back to LINEAR: {error}"
                );
                self.linear = true;
                gpu.allocate(width, height, &[])
            }
            allocated => allocated,
        };
        let dmabuf = match allocated {
            Ok(dmabuf) => dmabuf,
            Err(error) => {
                eprintln!("decade: allocating a frame buffer failed: {error}");
                return None;
            }
        };
        let id = self.next_id;
        self.next_id += 1;
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
            id,
            width,
            height,
            format: format.code as u32,
            modifier: format.modifier.into(),
            planes,
        };
        outbox.push_server_message(message).ok()?;
        self.slots.push(Slot {
            id,
            dmabuf: dmabuf.clone(),
            size,
            lent: false,
        });
        Some((id, dmabuf))
    }

    pub fn lend(&mut self, id: u32) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == id) {
            slot.lent = true;
        }
    }

    /// Take back a lent buffer, forgetting it if the frame size has moved on.
    pub fn release(&mut self, id: u32, size: Size<i32, Physical>, outbox: &mut Outbox) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == id) {
            slot.lent = false;
        }
        self.forget_stale(size, outbox);
    }

    /// Drop and forget each buffer not lent out whose size is not `size`.
    pub fn forget_stale(&mut self, size: Size<i32, Physical>, outbox: &mut Outbox) {
        self.slots.retain(|slot| {
            let stale = !slot.lent && slot.size != size;
            if stale {
                let _ = outbox.push_server_message(ServerMessage::Forget { id: slot.id });
            }
            !stale
        });
    }

    /// Drop and forget a buffer the viewer's host could not import, and
    /// allocate linear buffers from then on.
    pub fn failed(&mut self, id: u32, outbox: &mut Outbox) {
        self.linear = true;
        self.slots.retain(|slot| slot.id != id);
        let _ = outbox.push_server_message(ServerMessage::Forget { id });
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    use smithay::backend::allocator::dmabuf::DmabufFlags;

    use super::*;
    use crate::wire::Decoder;

    fn size(width: i32, height: i32) -> Size<i32, Physical> {
        (width, height).into()
    }

    fn pool(slots: &[(u32, Size<i32, Physical>, bool)]) -> Pool {
        let mut pool = Pool::new();
        for &(id, size, lent) in slots {
            let mut builder = Dmabuf::builder(
                (size.w, size.h),
                Fourcc::Xrgb8888,
                Modifier::Linear,
                DmabufFlags::empty(),
            );
            let fd = OwnedFd::from(File::open("/dev/null").unwrap());
            builder.add_plane(fd, 0, 0, size.w as u32 * 4);
            pool.slots.push(Slot {
                id,
                dmabuf: builder.build().unwrap(),
                size,
                lent,
            });
        }
        pool
    }

    fn ids(pool: &Pool) -> Vec<u32> {
        pool.slots.iter().map(|slot| slot.id).collect()
    }

    fn forgotten(mut outbox: Outbox) -> Vec<u32> {
        let (ours, theirs) = UnixStream::pair().unwrap();
        outbox.send(&ours).unwrap();
        drop(ours);
        let mut decoder = Decoder::new();
        while decoder.receive(&theirs).unwrap() {}
        let mut ids = Vec::new();
        while let Some(message) = decoder.next_server_message().unwrap() {
            if let ServerMessage::Forget { id } = message {
                ids.push(id);
            }
        }
        ids
    }

    #[test]
    fn a_released_buffer_is_kept_at_the_frame_size_and_forgotten_at_another() {
        let mut pool = pool(&[(0, size(800, 600), true), (1, size(800, 600), true)]);
        let mut outbox = Outbox::new();
        pool.release(0, size(800, 600), &mut outbox);
        assert_eq!(ids(&pool), [0, 1]);
        assert!(!pool.slots[0].lent);
        pool.release(1, size(1024, 768), &mut outbox);
        assert_eq!(ids(&pool), Vec::<u32>::new());
        assert_eq!(forgotten(outbox), [0, 1]);
    }

    #[test]
    fn a_resize_forgets_idle_buffers_of_the_old_size_and_keeps_lent_ones() {
        let mut pool = pool(&[
            (0, size(800, 600), false),
            (1, size(800, 600), true),
            (2, size(1024, 768), false),
        ]);
        let mut outbox = Outbox::new();
        pool.forget_stale(size(1024, 768), &mut outbox);
        assert_eq!(ids(&pool), [1, 2]);
        assert_eq!(forgotten(outbox), [0]);
    }

    #[test]
    fn a_failed_buffer_is_forgotten_and_the_pool_turns_linear() {
        let mut pool = pool(&[(0, size(800, 600), false), (1, size(800, 600), false)]);
        let mut outbox = Outbox::new();
        pool.failed(0, &mut outbox);
        assert!(pool.linear);
        assert_eq!(ids(&pool), [1]);
        assert_eq!(forgotten(outbox), [0]);
    }
}
