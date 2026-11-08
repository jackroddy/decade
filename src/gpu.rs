use std::fs::OpenOptions;
use std::os::fd::OwnedFd;

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
use smithay::backend::drm::{DrmDeviceFd, DrmNode, NodeType};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::DeviceFd;

const PREFERRED: [Fourcc; 4] = [
    Fourcc::Xrgb8888,
    Fourcc::Argb8888,
    Fourcc::Xbgr8888,
    Fourcc::Abgr8888,
];

pub struct Gpu {
    pub renderer: GlesRenderer,
    pub node: DrmNode,
    allocator: GbmAllocator<DrmDeviceFd>,
}

impl Gpu {
    pub fn open() -> Result<Gpu, String> {
        let devices = EGLDevice::enumerate().map_err(|error| error.to_string())?;
        let (device, node) = devices
            .filter_map(|device| {
                let node = device.try_get_render_node().ok().flatten()?;
                Some((device, node))
            })
            .next()
            .ok_or("no GPU with a render node")?;
        let path = node
            .dev_path_with_type(NodeType::Render)
            .or_else(|| node.dev_path())
            .ok_or("the render node has no path")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
        let gbm = GbmDevice::new(fd).map_err(|error| error.to_string())?;
        // SAFETY: the EGL device comes from EGL's own enumeration and
        //         outlives the display, which the renderer owns
        let display = unsafe { EGLDisplay::new(device) }.map_err(|error| error.to_string())?;
        let context = EGLContext::new(&display).map_err(|error| error.to_string())?;
        // SAFETY: the context is new, owned by nothing else, and the
        //         renderer takes it over
        let renderer = unsafe { GlesRenderer::new(context) }.map_err(|error| error.to_string())?;
        let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
        Ok(Gpu {
            renderer,
            node,
            allocator,
        })
    }

    pub fn allocate(
        &mut self,
        width: u32,
        height: u32,
        accepted: &[(Fourcc, Modifier)],
    ) -> Result<Dmabuf, String> {
        let renderable = self.renderer.egl_context().dmabuf_render_formats();
        let both = |fourcc: Fourcc| -> Vec<Modifier> {
            accepted
                .iter()
                .filter(|&&(code, modifier)| {
                    code == fourcc
                        && renderable
                            .iter()
                            .any(|format| format.code == code && format.modifier == modifier)
                })
                .map(|&(_, modifier)| modifier)
                .collect()
        };
        let (fourcc, modifiers) = PREFERRED
            .iter()
            .map(|&fourcc| (fourcc, both(fourcc)))
            .find(|(_, modifiers)| !modifiers.is_empty())
            .unwrap_or((Fourcc::Xrgb8888, vec![Modifier::Linear]));
        let buffer = self
            .allocator
            .create_buffer(width, height, fourcc, &modifiers)
            .map_err(|error| error.to_string())?;
        buffer.export().map_err(|error| error.to_string())
    }
}
