use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::renderer::{Bind, ExportMem};
use smithay::utils::Rectangle;

use crate::control;
use crate::gpu::Gpu;
use crate::png;
use crate::wire::{
    self, BTN_LEFT, BTN_RIGHT, Decoder, Format, KEY_LEFTALT, KEY_LEFTCTRL, KEY_LEFTMETA,
    KEY_LEFTSHIFT, Outbox, SCALE, ServerMessage, ViewerMessage,
};
const SIZE: (u32, u32) = (1600, 900);
const STEP: u64 = 60;
const SETTLE: u64 = 300;

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
    let stream = control::connect()?;
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
        version: wire::VERSION,
        transport: wire::LOCAL,
        width: SIZE.0,
        height: SIZE.1,
        scale: SCALE,
    })?;
    session.send(ViewerMessage::Formats { formats })?;
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
                let (_, key) = chord(rest)?;
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
            "click" | "right-click" => {
                let [x, y] = numbers()?[..] else {
                    return Err(format!("{command} takes x and y"));
                };
                let button = if command == "click" {
                    BTN_LEFT
                } else {
                    BTN_RIGHT
                };
                self.pointer(x, y)?;
                self.button(button, true)?;
                self.button(button, false)?;
            }
            "drag" => {
                let [x1, y1, x2, y2] = numbers()?[..] else {
                    return Err("drag takes x1 y1 x2 y2".into());
                };
                self.pointer(x1, y1)?;
                self.button(BTN_LEFT, true)?;
                for i in 1..=8 {
                    let t = i as f64 / 8.0;
                    self.pointer(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t)?;
                }
                self.button(BTN_LEFT, false)?;
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
                let png = png::encode(image.width, image.height, &image.rgb);
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

    fn button(&mut self, button: u32, pressed: bool) -> Result<(), String> {
        let time = self.time();
        self.send(ViewerMessage::Button {
            time,
            button,
            pressed,
        })?;
        self.pump(STEP)
    }

    /// Send what is queued and take in what arrives until `ms` have passed.
    fn pump(&mut self, ms: u64) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            self.outbox
                .send(&self.stream)
                .map_err(|error| error.to_string())?;
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
            let wait = Instant::now() + left.min(Duration::from_millis(20));
            let open = self
                .decoder
                .wait(&self.stream, Some(wait))
                .map_err(|error| format!("{error:?}"))?;
            if !open {
                return Err("the server closed the connection".into());
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
            ServerMessage::Error { message, .. } | ServerMessage::Close { message, .. } => {
                return Err(message);
            }
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
        modifiers.push(match named_key(part) {
            Some(key @ (KEY_LEFTMETA | KEY_LEFTSHIFT | KEY_LEFTCTRL | KEY_LEFTALT)) => key,
            _ => return Err(format!("{part} is not a modifier")),
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

#[cfg(test)]
mod tests {
    use super::*;

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
