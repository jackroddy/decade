use std::sync::Arc;

use winit::window::Window;

use crate::grid::Grid;
use crate::text::{Atlas, FONT_PX, Font};

const CELL_BYTES: u64 = 48;
const UNIFORM_BYTES: u64 = 32;

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    cells: wgpu::Buffer,
    cell_capacity: u64,
    font: Font,
    atlas: Atlas,
}

impl Gpu {
    pub fn new(window: Arc<Window>) -> Gpu {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(window).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .unwrap();
        let formats = surface.get_capabilities(&adapter).formats;
        if let Some(format) = formats.iter().find(|format| !format.is_srgb()) {
            config.format = *format;
        }
        surface.configure(&device, &config);

        let font = Font::load(FONT_PX * scale);
        let atlas = Atlas::new(&device, &font);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cells"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let bind_group = bind_group(&device, &bind_group_layout, &uniforms, &atlas, &sampler);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cells"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let shader = device.create_shader_module(wgpu::include_wgsl!("cell.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cells"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: CELL_BYTES,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Uint32x4, 1 => Float32x4, 2 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(config.format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        let cell_capacity = 1;
        let cells = cell_buffer(&device, cell_capacity);
        Gpu {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            bind_group,
            sampler,
            uniforms,
            cells,
            cell_capacity,
            font,
            atlas,
        }
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.font = Font::load(FONT_PX * scale);
        self.atlas = Atlas::new(&self.device, &self.font);
        self.bind_group = bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.uniforms,
            &self.atlas,
            &self.sampler,
        );
    }

    pub fn grid_size(&self) -> (u32, u32) {
        (
            self.config.width / self.font.cell_width,
            self.config.height / self.font.cell_height,
        )
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn render(&mut self, grid: &Grid) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return,
        };

        let count = grid.cells.len() as u64;
        if count > self.cell_capacity {
            self.cell_capacity = count;
            self.cells = cell_buffer(&self.device, count);
        }
        let mut bytes = Vec::with_capacity((count * CELL_BYTES) as usize);
        for (i, cell) in grid.cells.iter().enumerate() {
            let column = i as u32 % grid.columns;
            let row = i as u32 / grid.columns;
            let slot = self.atlas.slot(&self.queue, &self.font, cell.ch);
            for value in [column, row, slot, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in cell.bg.iter().chain(&cell.fg) {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        self.queue.write_buffer(&self.cells, 0, &bytes);

        let mut uniforms = Vec::with_capacity(UNIFORM_BYTES as usize);
        for value in [
            self.config.width as f32,
            self.config.height as f32,
            self.font.cell_width as f32,
            self.font.cell_height as f32,
            self.atlas.texture.width() as f32,
            self.atlas.texture.height() as f32,
        ] {
            uniforms.extend_from_slice(&value.to_le_bytes());
        }
        uniforms.extend_from_slice(&self.atlas.slots_per_row.to_le_bytes());
        uniforms.extend_from_slice(&0u32.to_le_bytes());
        self.queue.write_buffer(&self.uniforms, 0, &uniforms);

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.cells.slice(..));
            pass.draw(0..6, 0..count as u32);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}

fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    atlas: &Atlas,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let atlas_view = atlas
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cells"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn cell_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cells"),
        size: CELL_BYTES * capacity,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
