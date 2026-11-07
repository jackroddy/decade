use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::renderer::{Bind, ExportMem};
use smithay::utils::Rectangle;

use crate::control::viewer_socket_path;
use crate::gpu::Gpu;
use crate::wire::{Decoder, Format, Outbox, ServerMessage, ViewerMessage};
const SIZE: (u32, u32) = (1600, 900);
const SCALE: u32 = 120;
const STEP: u64 = 60;
const SETTLE: u64 = 300;
const BTN_LEFT: u32 = 0x110;
const KEY_LEFTCTRL: u32 = 29;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTALT: u32 = 56;
const KEY_LEFTMETA: u32 = 125;

/// Run a script of input against the running server, saving frames as it says.
pub fn run(path: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
    let gpu = Gpu::open()?;

    // frames come in a format this process can render to, since the
    // pixels are read back from a framebuffer
    let formats = gpu
        .renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .filter(|format| matches!(format.code, Fourcc::Xrgb8888 | Fourcc::Argb8888))
        .map(|format| Format {
            format: format.code as u32,
            modifier: format.modifier.into(),
        })
        .collect();
    let device = gpu.node.dev_id();
    let stream = UnixStream::connect(viewer_socket_path())
        .map_err(|_| "no server is running".to_string())?;
    stream
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let mut session = Session {
        stream,
        outbox: Outbox::new(),
        decoder: Decoder::new(),
        buffers: HashMap::new(),
        latest: None,
        start: Instant::now(),
        gpu,
    };
    session.send(ViewerMessage::Hello {
        version: 1,
        transport: 0,
        width: SIZE.0,
        height: SIZE.1,
        scale: SCALE,
    })?;
    session.send(ViewerMessage::Formats { device, formats })?;
    session.pump(SETTLE * 2)?;
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        session
            .step(line)
            .map_err(|error| format!("{path}:{}: {error}", number + 1))?;
    }
    Ok(())
}

/// A frame as RGB rows, top first.
struct Image {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

struct Session {
    stream: UnixStream,
    outbox: Outbox,
    decoder: Decoder,
    buffers: HashMap<u32, Dmabuf>,
    latest: Option<Image>,
    start: Instant,
    gpu: Gpu,
}

impl Session {
    fn send(&mut self, message: ViewerMessage) -> Result<(), String> {
        self.outbox
            .push_viewer_message(message)
            .map_err(|error| format!("{error:?}"))
    }

    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn step(&mut self, line: &str) -> Result<(), String> {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let numbers = || -> Result<Vec<f64>, String> {
            rest.split_whitespace()
                .map(|word| word.parse().map_err(|_| format!("{word} is not a number")))
                .collect()
        };
        match command {
            "key" => {
                let (modifiers, key) = chord(rest)?;
                self.press(&modifiers, key, 0)?;
            }
            "hold" => {
                let (keys, ms) = rest.rsplit_once(' ').ok_or("hold takes keys and a time")?;
                let ms: u64 = ms.parse().map_err(|_| format!("{ms} is not a time"))?;
                let (modifiers, key) = chord(keys)?;
                self.press(&modifiers, key, ms)?;
            }
            "press" | "release" => {
                let key = named_key(rest)
                    .or_else(|| chord(rest).ok().map(|(_, key)| key))
                    .ok_or(format!("no key named {rest}"))?;
                self.key(key, command == "press")?;
                self.pump(STEP)?;
            }
            "type" => {
                for ch in rest.chars() {
                    let (key, shift) = char_key(ch).ok_or(format!("no key types {ch:?}"))?;
                    let modifiers = if shift { vec![KEY_LEFTSHIFT] } else { vec![] };
                    self.press(&modifiers, key, 0)?;
                }
            }
            "move" => {
                let [x, y] = numbers()?[..] else {
                    return Err("move takes x and y".into());
                };
                self.pointer(x, y)?;
            }
            "click" => {
                let [x, y] = numbers()?[..] else {
                    return Err("click takes x and y".into());
                };
                self.pointer(x, y)?;
                self.button(true)?;
                self.button(false)?;
            }
            "drag" => {
                let [x1, y1, x2, y2] = numbers()?[..] else {
                    return Err("drag takes x1 y1 x2 y2".into());
                };
                self.pointer(x1, y1)?;
                self.button(true)?;
                for i in 1..=8 {
                    let t = i as f64 / 8.0;
                    self.pointer(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t)?;
                }
                self.button(false)?;
            }
            "size" => {
                let [width, height] = numbers()?[..] else {
                    return Err("size takes a width and a height".into());
                };
                self.send(ViewerMessage::Resize {
                    width: width as u32,
                    height: height as u32,
                    scale: SCALE,
                })?;
                self.pump(SETTLE)?;
            }
            "wait" => {
                let ms: u64 = rest.parse().map_err(|_| format!("{rest} is not a time"))?;
                self.pump(ms)?;
            }
            "shot" => {
                self.pump(SETTLE)?;
                let image = self.latest.as_ref().ok_or("no frame has arrived")?;
                let png = encode_png(image.width, image.height, &image.rgb);
                std::fs::write(rest, png).map_err(|error| format!("{rest}: {error}"))?;
            }
            _ => return Err(format!("no command named {command}")),
        }
        Ok(())
    }

    fn key(&mut self, keycode: u32, pressed: bool) -> Result<(), String> {
        let time = self.time();
        self.send(ViewerMessage::Key {
            time,
            keycode,
            pressed,
        })
    }

    fn press(&mut self, modifiers: &[u32], key: u32, hold: u64) -> Result<(), String> {
        for &modifier in modifiers {
            self.key(modifier, true)?;
        }
        self.key(key, true)?;
        self.pump(hold.max(10))?;
        self.key(key, false)?;
        for &modifier in modifiers.iter().rev() {
            self.key(modifier, false)?;
        }
        self.pump(STEP)
    }

    fn pointer(&mut self, x: f64, y: f64) -> Result<(), String> {
        let time = self.time();
        self.send(ViewerMessage::Pointer { time, x, y })?;
        self.pump(STEP / 3)
    }

    fn button(&mut self, pressed: bool) -> Result<(), String> {
        let time = self.time();
        self.send(ViewerMessage::Button {
            time,
            button: BTN_LEFT,
            pressed,
        })?;
        self.pump(STEP)
    }

    /// Send what is queued and take in what arrives until `ms` have passed.
    fn pump(&mut self, ms: u64) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            while !self.outbox.is_empty() {
                if !self
                    .outbox
                    .send(&self.stream)
                    .map_err(|error| error.to_string())?
                {
                    break;
                }
            }
            while let Some(message) = self
                .decoder
                .next_server_message()
                .map_err(|error| format!("{error:?}"))?
            {
                self.handle(message)?;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            let wait = left.as_millis().min(20) as i32;
            let mut fds = [PollFd::new(&self.stream, PollFlags::IN)];
            let _ = rustix::event::poll(&mut fds, wait);
            if fds[0].revents().contains(PollFlags::IN) {
                let open = self
                    .decoder
                    .receive(&self.stream)
                    .map_err(|error| format!("{error:?}"))?;
                if !open {
                    return Err("the server closed the connection".into());
                }
            }
        }
    }

    fn handle(&mut self, message: ServerMessage) -> Result<(), String> {
        match message {
            ServerMessage::Buffer {
                id,
                width,
                height,
                format,
                modifier,
                planes,
                ..
            } => {
                let format = Fourcc::try_from(format)
                    .map_err(|_| format!("the server sent an unknown format {format:#x}"))?;
                let mut builder = Dmabuf::builder(
                    (width as i32, height as i32),
                    format,
                    Modifier::from(modifier),
                    DmabufFlags::empty(),
                );
                for (index, plane) in planes.into_iter().enumerate() {
                    builder.add_plane(plane.fd, index as u32, plane.offset, plane.stride);
                }
                let dmabuf = builder
                    .build()
                    .ok_or("the server sent a buffer with no planes")?;
                self.buffers.insert(id, dmabuf);
            }
            ServerMessage::Forget { id } => {
                self.buffers.remove(&id);
            }
            ServerMessage::Frame { id, fence, .. } => {
                if let Some(fence) = &fence {
                    let mut fds = [PollFd::new(fence, PollFlags::IN)];
                    let _ = rustix::event::poll(&mut fds, 1000);
                }
                if let Some(mut dmabuf) = self.buffers.get(&id).cloned() {
                    self.latest = Some(read(&mut self.gpu, &mut dmabuf)?);
                }
                self.send(ViewerMessage::Release { id, fence: None })?;
                self.send(ViewerMessage::Ready)?;
            }
            ServerMessage::Error { message, .. } => return Err(message),
            ServerMessage::Close { message, .. } => return Err(message),
            _ => {}
        }
        Ok(())
    }
}

fn read(gpu: &mut Gpu, dmabuf: &mut Dmabuf) -> Result<Image, String> {
    let size = smithay::backend::allocator::Buffer::size(dmabuf);
    let (width, height) = (size.w as u32, size.h as u32);
    let renderer = &mut gpu.renderer;
    let framebuffer = renderer
        .bind(dmabuf)
        .map_err(|error| format!("binding a frame failed: {error}"))?;
    let mapping = renderer
        .copy_framebuffer(&framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
        .map_err(|error| format!("reading a frame failed: {error}"))?;
    drop(framebuffer);
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|error| format!("mapping a frame failed: {error}"))?;
    let rgb = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    Ok(Image { width, height, rgb })
}

fn chord(text: &str) -> Result<(Vec<u32>, u32), String> {
    let mut modifiers = Vec::new();
    let mut parts: Vec<&str> = text.split('+').collect();
    let key = parts.pop().ok_or("an empty chord")?;
    for part in parts {
        modifiers.push(match part.to_lowercase().as_str() {
            "super" => KEY_LEFTMETA,
            "shift" => KEY_LEFTSHIFT,
            "ctrl" => KEY_LEFTCTRL,
            "alt" => KEY_LEFTALT,
            other => return Err(format!("{other} is not a modifier")),
        });
    }
    let key = named_key(key)
        .or_else(|| {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => char_key(ch.to_ascii_lowercase()).map(|(key, _)| key),
                _ => None,
            }
        })
        .ok_or(format!("no key named {key}"))?;
    Ok((modifiers, key))
}

fn named_key(name: &str) -> Option<u32> {
    Some(match name.to_lowercase().as_str() {
        "escape" | "esc" => 1,
        "backspace" => 14,
        "tab" => 15,
        "return" | "enter" => 28,
        "space" => 57,
        "ctrl" => KEY_LEFTCTRL,
        "shift" => KEY_LEFTSHIFT,
        "alt" => KEY_LEFTALT,
        "super" => KEY_LEFTMETA,
        "up" => 103,
        "left" => 105,
        "right" => 106,
        "down" => 108,
        "home" => 102,
        "end" => 107,
        "delete" => 111,
        _ => return None,
    })
}

/// The evdev key that types `ch` on a US layout, and whether it needs Shift.
fn char_key(ch: char) -> Option<(u32, bool)> {
    const ROWS: [(&str, &str, u32); 4] = [
        ("1234567890-=", "!@#$%^&*()_+", 2),
        ("qwertyuiop[]", "QWERTYUIOP{}", 16),
        ("asdfghjkl;'`", "ASDFGHJKL:\"~", 30),
        ("\\zxcvbnm,./", "|ZXCVBNM<>?", 43),
    ];
    if ch == ' ' {
        return Some((57, false));
    }
    ROWS.iter().find_map(|&(plain, shifted, first)| {
        let find = |row: &str| row.chars().position(|c| c == ch).map(|i| first + i as u32);
        find(plain)
            .map(|key| (key, false))
            .or_else(|| find(shifted).map(|key| (key, true)))
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// An RGB image as a PNG, its pixel data in zlib's uncompressed blocks.
fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row = width as usize * 3;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgb.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(0xffff).collect();
    for (i, block) in blocks.iter().enumerate() {
        let len = block.len() as u16;
        zlib.push((i + 1 == blocks.len()) as u8);
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_ends_with_the_standard_iend_chunk() {
        let png = encode_png(2, 1, &[255, 0, 0, 0, 0, 255]);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[png.len() - 12..], b"\0\0\0\0IEND\xae\x42\x60\x82");
    }

    #[test]
    fn the_crc_matches_the_published_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn chords_name_modifiers_then_one_key() {
        assert_eq!(
            chord("super+shift+space").unwrap(),
            (vec![KEY_LEFTMETA, KEY_LEFTSHIFT], 57)
        );
        assert_eq!(chord("ctrl+h").unwrap(), (vec![KEY_LEFTCTRL], 35));
        assert_eq!(chord("Return").unwrap(), (vec![], 28));
        assert!(chord("hyper+a").is_err());
    }

    #[test]
    fn typed_characters_map_to_keys_with_shift_where_needed() {
        assert_eq!(char_key('a'), Some((30, false)));
        assert_eq!(char_key('~'), Some((41, true)));
        assert_eq!(char_key('/'), Some((53, false)));
        assert_eq!(char_key('Z'), Some((44, true)));
        assert_eq!(char_key('é'), None);
    }
}
