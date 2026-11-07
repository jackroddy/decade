use std::collections::HashMap;

use swash::scale::{Render, ScaleContext, Source};
use swash::{CacheKey, FontRef};

pub const FONT_FAMILY: &str = "JetBrainsMono Nerd Font";
pub const FONT_PX: f32 = 14.0;
const ATLAS_PX: u32 = 2048;

pub struct Font {
    data: Vec<u8>,
    offset: u32,
    key: CacheKey,
    px: f32,
    pub cell_width: u32,
    pub cell_height: u32,
    baseline: i32,
}

impl Font {
    pub fn load(px: f32) -> Font {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(FONT_FAMILY), fontdb::Family::Monospace],
            ..Default::default()
        };
        let id = db.query(&query).expect("no monospace font installed");
        let face = db.face(id).unwrap();
        let data = match &face.source {
            fontdb::Source::File(path) => std::fs::read(path).unwrap(),
            _ => panic!("font {} is not a file on disk", FONT_FAMILY),
        };
        let font = FontRef::from_index(&data, face.index as usize).unwrap();
        let metrics = font.metrics(&[]).scale(px);
        let advance = font
            .glyph_metrics(&[])
            .scale(px)
            .advance_width(font.charmap().map('M'));
        let (offset, key) = (font.offset, font.key);
        Font {
            cell_width: advance.round().max(1.0) as u32,
            cell_height: (metrics.ascent + metrics.descent + metrics.leading)
                .round()
                .max(1.0) as u32,
            baseline: metrics.ascent.round() as i32,
            data,
            offset,
            key,
            px,
        }
    }

    fn as_ref(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }
}

pub struct Atlas {
    pub texture: wgpu::Texture,
    pub slots_per_row: u32,
    capacity: u32,
    slots: HashMap<char, u32>,
    next: u32,
    context: ScaleContext,
    pixels: Vec<u8>,
}

impl Atlas {
    pub fn new(device: &wgpu::Device, font: &Font) -> Atlas {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_PX,
                height: ATLAS_PX,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let slots_per_row = ATLAS_PX / font.cell_width;
        Atlas {
            texture,
            slots_per_row,
            capacity: slots_per_row * (ATLAS_PX / font.cell_height),
            slots: HashMap::new(),
            next: 1,
            context: ScaleContext::new(),
            pixels: vec![0; (font.cell_width * font.cell_height) as usize],
        }
    }

    pub fn slot(&mut self, queue: &wgpu::Queue, font: &Font, ch: char) -> u32 {
        if ch == ' ' {
            return 0;
        }
        if let Some(&slot) = self.slots.get(&ch) {
            return slot;
        }
        if self.next >= self.capacity {
            return 0;
        }
        let slot = self.next;
        self.next += 1;
        self.slots.insert(ch, slot);
        self.rasterize(font, ch);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: (slot % self.slots_per_row) * font.cell_width,
                    y: (slot / self.slots_per_row) * font.cell_height,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &self.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(font.cell_width),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: font.cell_width,
                height: font.cell_height,
                depth_or_array_layers: 1,
            },
        );
        slot
    }

    fn rasterize(&mut self, font: &Font, ch: char) {
        self.pixels.fill(0);
        let font_ref = font.as_ref();
        let glyph = font_ref.charmap().map(ch);
        let mut scaler = self
            .context
            .builder(font_ref)
            .size(font.px)
            .hint(true)
            .build();
        let Some(image) = Render::new(&[Source::Outline]).render(&mut scaler, glyph) else {
            return;
        };
        let width = font.cell_width as i32;
        let height = font.cell_height as i32;
        let image_width = image.placement.width as i32;
        for y in 0..image.placement.height as i32 {
            let cy = font.baseline - image.placement.top + y;
            if cy < 0 || cy >= height {
                continue;
            }
            for x in 0..image_width {
                let cx = image.placement.left + x;
                if cx < 0 || cx >= width {
                    continue;
                }
                self.pixels[(cy * width + cx) as usize] =
                    image.data[(y * image_width + x) as usize];
            }
        }
    }
}
