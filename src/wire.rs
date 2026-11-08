#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, IoSlice, IoSliceMut};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::io::Errno;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};

// the header, all integers little-endian:
//
//     0       4       6     7       8
//     | length | opcode | fds | flags |
//
// see docs/viewer.md for the messages that follow it
const HEADER_LEN: usize = 8;
const LENGTH: usize = 0;
const OPCODE: usize = 4;
const FDS: usize = 6;
const FLAGS: usize = 7;

pub const MAX_MESSAGE_LEN: usize = 1 << 20;
pub const MAX_FDS_PER_SEND: usize = 28;

const FD_SPACE: usize = rustix::cmsg_space!(ScmRights(MAX_FDS_PER_SEND));
const RECEIVE_CHUNK: usize = 64 * 1024;

// rustix 0.38 has no name for MSG_CTRUNC; these are its
// values in <bits/socket.h> on every Linux architecture
// and in <sys/socket.h> on macOS
#[cfg(target_os = "linux")]
const MSG_CTRUNC: u32 = 0x8;
#[cfg(target_os = "macos")]
const MSG_CTRUNC: u32 = 0x20;

#[cfg(target_os = "linux")]
const SEND_FLAGS: SendFlags = SendFlags::DONTWAIT.union(SendFlags::NOSIGNAL);
#[cfg(target_os = "linux")]
const RECEIVE_FLAGS: RecvFlags = RecvFlags::DONTWAIT.union(RecvFlags::CMSG_CLOEXEC);

// macOS has neither MSG_NOSIGNAL nor MSG_CMSG_CLOEXEC; a
// viewer there talks to a server on another machine and
// passes no fds
#[cfg(target_os = "macos")]
const SEND_FLAGS: SendFlags = SendFlags::DONTWAIT;
#[cfg(target_os = "macos")]
const RECEIVE_FLAGS: RecvFlags = RecvFlags::DONTWAIT;

/// A message from a viewer to the server.
#[derive(Debug)]
pub enum ViewerMessage {
    Error {
        message: String,
    },
    Hello {
        version: u32,
        transport: u32,
        width: u32,
        height: u32,
        scale: u32,
    },
    Formats {
        formats: Vec<Format>,
    },
    Resize {
        width: u32,
        height: u32,
        scale: u32,
    },
    BufferFailed {
        id: u32,
    },
    Release {
        id: u32,
        fence: Option<OwnedFd>,
    },
    Ready,
    Key {
        time: u32,
        keycode: u32,
        pressed: bool,
    },
    Keys {
        locked: u32,
        keycodes: Vec<u32>,
    },
    KeysReleased,
    Pointer {
        time: u32,
        x: f64,
        y: f64,
    },
    Button {
        time: u32,
        button: u32,
        pressed: bool,
    },
    Scroll {
        time: u32,
        source: u32,
        axis: u32,
        amount: f64,
        steps: i32,
        stop: bool,
        inverted: bool,
    },
    Offer {
        selection: Selection,
        serial: u32,
        seen: u32,
        mimes: Vec<String>,
    },
    Cleared {
        selection: Selection,
        serial: u32,
    },
    Request {
        selection: Selection,
        serial: u32,
        mime: String,
        fd: OwnedFd,
    },
    List,
}

/// A message from the server to a viewer.
#[derive(Debug)]
pub enum ServerMessage {
    Error {
        message: String,
    },
    Welcome {
        version: u32,
    },
    Output {
        width: u32,
        height: u32,
        scale: u32,
    },
    Buffer {
        id: u32,
        width: u32,
        height: u32,
        format: u32,
        modifier: u64,
        planes: Vec<Plane>,
    },
    Forget {
        id: u32,
    },
    Frame {
        id: u32,
        fence: Option<OwnedFd>,
    },
    CursorShape {
        shape: String,
    },
    CursorHidden,
    Offer {
        selection: Selection,
        serial: u32,
        mimes: Vec<String>,
    },
    Cleared {
        selection: Selection,
        serial: u32,
    },
    Request {
        selection: Selection,
        serial: u32,
        mime: String,
        fd: OwnedFd,
    },
    Workspaces {
        workspaces: Vec<Workspace>,
    },
    Showing {
        id: u32,
    },
    Close {
        message: String,
    },
    Listing {
        workspaces: Vec<Listed>,
    },
}

/// A DRM fourcc and modifier the viewer's host can import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub format: u32,
    pub modifier: u64,
}

/// A selection that crosses between server and viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Clipboard,
    Primary,
}

/// One plane of a dmabuf.
#[derive(Debug)]
pub struct Plane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: u32,
    pub name: String,
}

/// A workspace as `listing` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: u32,
    pub name: String,
    pub cwd: String,
    pub panes: u32,
}

/// A failure to encode, decode or move messages.
#[derive(Debug)]
pub enum Error {
    UnknownOpcode(u16),
    BadLength(u32),
    TooLong(usize),
    TooManyFds(usize),
    Flags(u8),
    Malformed,
    MissingFds,
    TruncatedFds,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::UnknownOpcode(opcode) => write!(f, "unknown opcode {opcode}"),
            Error::BadLength(len) => write!(f, "bad message length {len}"),
            Error::TooLong(len) => write!(f, "message of {len} bytes is over 1 MiB"),
            Error::TooManyFds(count) => write!(f, "message carries {count} fds"),
            Error::Flags(flags) => write!(f, "nonzero flags {flags:#x}"),
            Error::Malformed => write!(f, "malformed message"),
            Error::MissingFds => write!(f, "message arrived without its fds"),
            Error::TruncatedFds => write!(f, "fds were truncated"),
            Error::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl ViewerMessage {
    /// Append the message's bytes to `bytes` and its fds, in argument order, to `fds`.
    ///
    /// # Errors
    ///
    /// When the message is over 1 MiB or carries more than 28 fds. Nothing is appended then.
    pub fn encode(self, bytes: &mut Vec<u8>, fds: &mut Vec<OwnedFd>) -> Result<(), Error> {
        let mut w = Writer::new(bytes, fds);
        let opcode = match self {
            ViewerMessage::Error { message } => {
                w.string(&message);
                0
            }
            ViewerMessage::Hello {
                version,
                transport,
                width,
                height,
                scale,
            } => {
                w.u32(version);
                w.u32(transport);
                w.u32(width);
                w.u32(height);
                w.u32(scale);
                1
            }
            ViewerMessage::Formats { formats } => {
                w.count(formats.len());
                for format in formats {
                    w.u32(format.format);
                    w.u64(format.modifier);
                }
                2
            }
            ViewerMessage::Resize {
                width,
                height,
                scale,
            } => {
                w.u32(width);
                w.u32(height);
                w.u32(scale);
                3
            }
            ViewerMessage::BufferFailed { id } => {
                w.u32(id);
                4
            }
            ViewerMessage::Release { id, fence } => {
                w.u32(id);
                w.fence(fence);
                5
            }
            ViewerMessage::Ready => 6,
            ViewerMessage::Key {
                time,
                keycode,
                pressed,
            } => {
                w.u32(time);
                w.u32(keycode);
                w.bool(pressed);
                7
            }
            ViewerMessage::Keys { locked, keycodes } => {
                w.u32(locked);
                w.count(keycodes.len());
                for keycode in keycodes {
                    w.u32(keycode);
                }
                8
            }
            ViewerMessage::KeysReleased => 9,
            ViewerMessage::Pointer { time, x, y } => {
                w.u32(time);
                w.fixed(x);
                w.fixed(y);
                10
            }
            ViewerMessage::Button {
                time,
                button,
                pressed,
            } => {
                w.u32(time);
                w.u32(button);
                w.bool(pressed);
                11
            }
            ViewerMessage::Scroll {
                time,
                source,
                axis,
                amount,
                steps,
                stop,
                inverted,
            } => {
                w.u32(time);
                w.u32(source);
                w.u32(axis);
                w.fixed(amount);
                w.i32(steps);
                w.bool(stop);
                w.bool(inverted);
                12
            }
            ViewerMessage::Offer {
                selection,
                serial,
                seen,
                mimes,
            } => {
                w.selection(selection);
                w.u32(serial);
                w.u32(seen);
                w.strings(&mimes);
                13
            }
            ViewerMessage::Cleared { selection, serial } => {
                w.selection(selection);
                w.u32(serial);
                14
            }
            ViewerMessage::Request {
                selection,
                serial,
                mime,
                fd,
            } => {
                w.selection(selection);
                w.u32(serial);
                w.string(&mime);
                w.fd(fd);
                15
            }
            ViewerMessage::List => 16,
        };
        w.finish(opcode)
    }
}

impl ServerMessage {
    /// Append the message's bytes to `bytes` and its fds, in argument order, to `fds`.
    ///
    /// # Errors
    ///
    /// When the message is over 1 MiB or carries more than 28 fds. Nothing is appended then.
    pub fn encode(self, bytes: &mut Vec<u8>, fds: &mut Vec<OwnedFd>) -> Result<(), Error> {
        let mut w = Writer::new(bytes, fds);
        let opcode = match self {
            ServerMessage::Error { message } => {
                w.string(&message);
                0
            }
            ServerMessage::Welcome { version } => {
                w.u32(version);
                1
            }
            ServerMessage::Output {
                width,
                height,
                scale,
            } => {
                w.u32(width);
                w.u32(height);
                w.u32(scale);
                2
            }
            ServerMessage::Buffer {
                id,
                width,
                height,
                format,
                modifier,
                planes,
            } => {
                w.u32(id);
                w.u32(width);
                w.u32(height);
                w.u32(format);
                w.u64(modifier);
                w.count(planes.len());
                for plane in planes {
                    w.fd(plane.fd);
                    w.u32(plane.offset);
                    w.u32(plane.stride);
                }
                3
            }
            ServerMessage::Forget { id } => {
                w.u32(id);
                4
            }
            ServerMessage::Frame { id, fence } => {
                w.u32(id);
                w.fence(fence);
                5
            }
            ServerMessage::CursorShape { shape } => {
                w.string(&shape);
                6
            }
            ServerMessage::CursorHidden => 7,
            ServerMessage::Offer {
                selection,
                serial,
                mimes,
            } => {
                w.selection(selection);
                w.u32(serial);
                w.strings(&mimes);
                8
            }
            ServerMessage::Cleared { selection, serial } => {
                w.selection(selection);
                w.u32(serial);
                9
            }
            ServerMessage::Request {
                selection,
                serial,
                mime,
                fd,
            } => {
                w.selection(selection);
                w.u32(serial);
                w.string(&mime);
                w.fd(fd);
                10
            }
            ServerMessage::Workspaces { workspaces } => {
                w.count(workspaces.len());
                for workspace in workspaces {
                    w.u32(workspace.id);
                    w.string(&workspace.name);
                }
                11
            }
            ServerMessage::Showing { id } => {
                w.u32(id);
                12
            }
            ServerMessage::Close { message } => {
                w.string(&message);
                13
            }
            ServerMessage::Listing { workspaces } => {
                w.count(workspaces.len());
                for workspace in workspaces {
                    w.u32(workspace.id);
                    w.string(&workspace.name);
                    w.string(&workspace.cwd);
                    w.u32(workspace.panes);
                }
                14
            }
        };
        w.finish(opcode)
    }
}

struct Writer<'a> {
    bytes: &'a mut Vec<u8>,
    fds: &'a mut Vec<OwnedFd>,
    start: usize,
    fds_start: usize,
}

impl<'a> Writer<'a> {
    fn new(bytes: &'a mut Vec<u8>, fds: &'a mut Vec<OwnedFd>) -> Self {
        let start = bytes.len();
        let fds_start = fds.len();
        bytes.extend_from_slice(&[0; HEADER_LEN]);
        Writer {
            bytes,
            fds,
            start,
            fds_start,
        }
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.u32(value as u32);
        self.u32((value >> 32) as u32);
    }

    fn fixed(&mut self, value: f64) {
        self.i32((value * 256.0).round() as i32);
    }

    fn bool(&mut self, value: bool) {
        self.u32(value as u32);
    }

    fn count(&mut self, count: usize) {
        self.u32(count as u32);
    }

    fn selection(&mut self, selection: Selection) {
        self.u32(selection as u32);
    }

    fn string(&mut self, s: &str) {
        self.count(s.len());
        self.bytes.extend_from_slice(s.as_bytes());
        let padding = s.len().next_multiple_of(4) - s.len();
        self.bytes.extend_from_slice(&[0; 3][..padding]);
    }

    fn fd(&mut self, fd: OwnedFd) {
        self.fds.push(fd);
    }

    fn fence(&mut self, fence: Option<OwnedFd>) {
        self.bool(fence.is_some());
        if let Some(fd) = fence {
            self.fd(fd);
        }
    }

    fn strings(&mut self, strings: &[String]) {
        self.count(strings.len());
        for s in strings {
            self.string(s);
        }
    }

    fn finish(self, opcode: u16) -> Result<(), Error> {
        let len = self.bytes.len() - self.start;
        let fd_count = self.fds.len() - self.fds_start;
        let error = if len > MAX_MESSAGE_LEN {
            Error::TooLong(len)
        } else if fd_count > MAX_FDS_PER_SEND {
            Error::TooManyFds(fd_count)
        } else {
            let header = &mut self.bytes[self.start..self.start + HEADER_LEN];
            header[LENGTH..OPCODE].copy_from_slice(&(len as u32).to_le_bytes());
            header[OPCODE..FDS].copy_from_slice(&opcode.to_le_bytes());
            header[FDS] = fd_count as u8;
            header[FLAGS] = 0;
            return Ok(());
        };
        self.bytes.truncate(self.start);
        self.fds.truncate(self.fds_start);
        Err(error)
    }
}

/// Messages waiting to be written to a socket, with their fds.
#[derive(Default)]
pub struct Outbox {
    bytes: Vec<u8>,
    fds: Vec<OwnedFd>,
    written: usize,

    /// The offset of the first message whose fds have not been sent.
    next_header: usize,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_viewer_message(&mut self, message: ViewerMessage) -> Result<(), Error> {
        message.encode(&mut self.bytes, &mut self.fds)
    }

    pub fn push_server_message(&mut self, message: ServerMessage) -> Result<(), Error> {
        message.encode(&mut self.bytes, &mut self.fds)
    }

    pub fn is_empty(&self) -> bool {
        self.written == self.bytes.len()
    }

    /// Write as much as the socket takes without blocking, and return whether everything is sent.
    pub fn send(&mut self, stream: &UnixStream) -> io::Result<bool> {
        while self.written < self.bytes.len() {
            let mut end = self.bytes.len();
            let mut fd_count = 0;
            let mut header = self.next_header;
            while header < self.bytes.len() {
                let count = self.bytes[header + FDS] as usize;
                if fd_count + count > MAX_FDS_PER_SEND {
                    end = header;
                    break;
                }
                fd_count += count;
                header += read_u32(&self.bytes[header + LENGTH..]) as usize;
            }

            let fds: Vec<BorrowedFd> = self.fds[..fd_count].iter().map(|fd| fd.as_fd()).collect();
            let mut space = [0; FD_SPACE];
            let mut control = SendAncillaryBuffer::new(&mut space);
            if !fds.is_empty() {
                control.push(SendAncillaryMessage::ScmRights(&fds));
            }
            let result = rustix::net::sendmsg(
                stream,
                &[IoSlice::new(&self.bytes[self.written..end])],
                &mut control,
                SEND_FLAGS,
            );
            drop(fds);
            match result {
                Ok(n) => {
                    // note: a short write still carries every fd, so
                    //       the fds of messages past the written bytes
                    //       arrive ahead of their first byte, which the
                    //       receiver's queue accepts
                    self.fds.drain(..fd_count);
                    self.written += n;
                    self.next_header = header;
                }
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => return Ok(false),
                Err(errno) => return Err(errno.into()),
            }
        }
        self.bytes.clear();
        self.fds.clear();
        self.written = 0;
        self.next_header = 0;
        Ok(true)
    }
}

/// Received bytes and fds, from which complete messages are taken.
#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    start: usize,
    end: usize,
    fds: VecDeque<OwnedFd>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read once from the socket without blocking, and return false when the peer has closed it.
    pub fn receive(&mut self, stream: &UnixStream) -> Result<bool, Error> {
        self.make_room(RECEIVE_CHUNK);
        let mut space = [0; FD_SPACE];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let result = loop {
            match rustix::net::recvmsg(
                stream,
                &mut [IoSliceMut::new(&mut self.bytes[self.end..])],
                &mut control,
                RECEIVE_FLAGS,
            ) {
                Err(Errno::INTR) => {}
                result => break result,
            }
        };
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = message {
                self.fds.extend(fds);
            }
        }
        match result {
            Ok(received) => {
                if received.flags.bits() & MSG_CTRUNC != 0 {
                    return Err(Error::TruncatedFds);
                }
                self.end += received.bytes;
                Ok(received.bytes > 0)
            }
            Err(Errno::AGAIN) => Ok(true),
            Err(errno) => Err(Error::Io(errno.into())),
        }
    }

    fn make_room(&mut self, len: usize) {
        if self.start > 0 {
            self.bytes.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        if self.bytes.len() < self.end + len {
            self.bytes.resize(self.end + len, 0);
        }
    }

    fn next_message(&mut self) -> Result<Option<(u16, Args<'_>)>, Error> {
        let available = &self.bytes[self.start..self.end];
        if available.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = read_u32(&available[LENGTH..]);
        let opcode = u16::from_le_bytes([available[OPCODE], available[OPCODE + 1]]);
        let fd_count = available[FDS] as usize;
        if (len as usize) < HEADER_LEN || !len.is_multiple_of(4) {
            return Err(Error::BadLength(len));
        }
        if len as usize > MAX_MESSAGE_LEN {
            return Err(Error::TooLong(len as usize));
        }
        if available[FLAGS] != 0 {
            return Err(Error::Flags(available[FLAGS]));
        }
        if fd_count > MAX_FDS_PER_SEND {
            return Err(Error::TooManyFds(fd_count));
        }
        if available.len() < len as usize {
            return Ok(None);
        }
        if self.fds.len() < fd_count {
            return Err(Error::MissingFds);
        }
        let args = Args {
            bytes: &available[HEADER_LEN..len as usize],
            fds: self.fds.drain(..fd_count).collect::<Vec<_>>().into_iter(),
        };
        self.start += len as usize;
        Ok(Some((opcode, args)))
    }

    /// Take the next complete message from a viewer, or None until one has arrived.
    pub fn next_viewer_message(&mut self) -> Result<Option<ViewerMessage>, Error> {
        let Some((opcode, mut a)) = self.next_message()? else {
            return Ok(None);
        };
        let message = match opcode {
            0 => ViewerMessage::Error {
                message: a.string()?,
            },
            1 => ViewerMessage::Hello {
                version: a.u32()?,
                transport: a.u32()?,
                width: a.u32()?,
                height: a.u32()?,
                scale: a.u32()?,
            },
            2 => {
                let count = a.count(12)?;
                let mut formats = Vec::with_capacity(count);
                for _ in 0..count {
                    formats.push(Format {
                        format: a.u32()?,
                        modifier: a.u64()?,
                    });
                }
                ViewerMessage::Formats { formats }
            }
            3 => ViewerMessage::Resize {
                width: a.u32()?,
                height: a.u32()?,
                scale: a.u32()?,
            },
            4 => ViewerMessage::BufferFailed { id: a.u32()? },
            5 => ViewerMessage::Release {
                id: a.u32()?,
                fence: a.fence()?,
            },
            6 => ViewerMessage::Ready,
            7 => ViewerMessage::Key {
                time: a.u32()?,
                keycode: a.u32()?,
                pressed: a.bool()?,
            },
            8 => {
                let locked = a.u32()?;
                let count = a.count(4)?;
                let mut keycodes = Vec::with_capacity(count);
                for _ in 0..count {
                    keycodes.push(a.u32()?);
                }
                ViewerMessage::Keys { locked, keycodes }
            }
            9 => ViewerMessage::KeysReleased,
            10 => ViewerMessage::Pointer {
                time: a.u32()?,
                x: a.fixed()?,
                y: a.fixed()?,
            },
            11 => ViewerMessage::Button {
                time: a.u32()?,
                button: a.u32()?,
                pressed: a.bool()?,
            },
            12 => ViewerMessage::Scroll {
                time: a.u32()?,
                source: a.u32()?,
                axis: a.u32()?,
                amount: a.fixed()?,
                steps: a.i32()?,
                stop: a.bool()?,
                inverted: a.bool()?,
            },
            13 => ViewerMessage::Offer {
                selection: a.selection()?,
                serial: a.u32()?,
                seen: a.u32()?,
                mimes: a.strings()?,
            },
            14 => ViewerMessage::Cleared {
                selection: a.selection()?,
                serial: a.u32()?,
            },
            15 => ViewerMessage::Request {
                selection: a.selection()?,
                serial: a.u32()?,
                mime: a.string()?,
                fd: a.fd()?,
            },
            16 => ViewerMessage::List,
            _ => return Err(Error::UnknownOpcode(opcode)),
        };
        a.finish()?;
        Ok(Some(message))
    }

    /// Take the next complete message from the server, or None until one has arrived.
    pub fn next_server_message(&mut self) -> Result<Option<ServerMessage>, Error> {
        let Some((opcode, mut a)) = self.next_message()? else {
            return Ok(None);
        };
        let message = match opcode {
            0 => ServerMessage::Error {
                message: a.string()?,
            },
            1 => ServerMessage::Welcome { version: a.u32()? },
            2 => ServerMessage::Output {
                width: a.u32()?,
                height: a.u32()?,
                scale: a.u32()?,
            },
            3 => {
                let id = a.u32()?;
                let width = a.u32()?;
                let height = a.u32()?;
                let format = a.u32()?;
                let modifier = a.u64()?;
                let count = a.count(8)?;
                let mut planes = Vec::with_capacity(count);
                for _ in 0..count {
                    planes.push(Plane {
                        fd: a.fd()?,
                        offset: a.u32()?,
                        stride: a.u32()?,
                    });
                }
                ServerMessage::Buffer {
                    id,
                    width,
                    height,
                    format,
                    modifier,
                    planes,
                }
            }
            4 => ServerMessage::Forget { id: a.u32()? },
            5 => ServerMessage::Frame {
                id: a.u32()?,
                fence: a.fence()?,
            },
            6 => ServerMessage::CursorShape { shape: a.string()? },
            7 => ServerMessage::CursorHidden,
            8 => ServerMessage::Offer {
                selection: a.selection()?,
                serial: a.u32()?,
                mimes: a.strings()?,
            },
            9 => ServerMessage::Cleared {
                selection: a.selection()?,
                serial: a.u32()?,
            },
            10 => ServerMessage::Request {
                selection: a.selection()?,
                serial: a.u32()?,
                mime: a.string()?,
                fd: a.fd()?,
            },
            11 => {
                let count = a.count(8)?;
                let mut workspaces = Vec::with_capacity(count);
                for _ in 0..count {
                    workspaces.push(Workspace {
                        id: a.u32()?,
                        name: a.string()?,
                    });
                }
                ServerMessage::Workspaces { workspaces }
            }
            12 => ServerMessage::Showing { id: a.u32()? },
            13 => ServerMessage::Close {
                message: a.string()?,
            },
            14 => {
                let count = a.count(16)?;
                let mut workspaces = Vec::with_capacity(count);
                for _ in 0..count {
                    workspaces.push(Listed {
                        id: a.u32()?,
                        name: a.string()?,
                        cwd: a.string()?,
                        panes: a.u32()?,
                    });
                }
                ServerMessage::Listing { workspaces }
            }
            _ => return Err(Error::UnknownOpcode(opcode)),
        };
        a.finish()?;
        Ok(Some(message))
    }
}

/// The arguments of one message, with the fds its header counted.
struct Args<'a> {
    bytes: &'a [u8],
    fds: std::vec::IntoIter<OwnedFd>,
}

impl<'a> Args<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        if len > self.bytes.len() {
            return Err(Error::Malformed);
        }
        let (taken, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(read_u32(self.take(4)?))
    }

    fn i32(&mut self) -> Result<i32, Error> {
        Ok(self.u32()? as i32)
    }

    fn u64(&mut self) -> Result<u64, Error> {
        let low = self.u32()? as u64;
        let high = self.u32()? as u64;
        Ok(low | high << 32)
    }

    fn fixed(&mut self) -> Result<f64, Error> {
        Ok(self.i32()? as f64 / 256.0)
    }

    fn bool(&mut self) -> Result<bool, Error> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Malformed),
        }
    }

    fn selection(&mut self) -> Result<Selection, Error> {
        match self.u32()? {
            0 => Ok(Selection::Clipboard),
            1 => Ok(Selection::Primary),
            _ => Err(Error::Malformed),
        }
    }

    /// Read an element count, rejecting one whose elements of
    /// at least `min_size` bytes could not fit in what is left.
    fn count(&mut self, min_size: usize) -> Result<usize, Error> {
        let count = self.u32()? as usize;
        if count * min_size > self.bytes.len() {
            return Err(Error::Malformed);
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, Error> {
        let len = self.u32()? as usize;
        let padded = self.take(len.next_multiple_of(4))?;
        let s = std::str::from_utf8(&padded[..len]).map_err(|_| Error::Malformed)?;
        Ok(s.to_owned())
    }

    fn strings(&mut self) -> Result<Vec<String>, Error> {
        let count = self.count(4)?;
        let mut strings = Vec::with_capacity(count);
        for _ in 0..count {
            strings.push(self.string()?);
        }
        Ok(strings)
    }

    fn fd(&mut self) -> Result<OwnedFd, Error> {
        self.fds.next().ok_or(Error::Malformed)
    }

    fn fence(&mut self) -> Result<Option<OwnedFd>, Error> {
        if self.bool()? {
            Ok(Some(self.fd()?))
        } else {
            Ok(None)
        }
    }

    fn finish(mut self) -> Result<(), Error> {
        if !self.bytes.is_empty() || self.fds.next().is_some() {
            return Err(Error::Malformed);
        }
        Ok(())
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    fn pipe() -> (std::io::PipeReader, OwnedFd) {
        let (reader, writer) = std::io::pipe().unwrap();
        (reader, writer.into())
    }

    fn fd() -> OwnedFd {
        pipe().1
    }

    impl Decoder {
        fn push_bytes(&mut self, data: &[u8]) {
            self.make_room(data.len());
            self.bytes[self.end..self.end + data.len()].copy_from_slice(data);
            self.end += data.len();
        }

        fn push_fd(&mut self, fd: OwnedFd) {
            self.fds.push_back(fd);
        }
    }

    fn viewer_messages() -> Vec<ViewerMessage> {
        vec![
            ViewerMessage::Error {
                message: "it broke".into(),
            },
            ViewerMessage::Hello {
                version: 1,
                transport: 0,
                width: 1280,
                height: 800,
                scale: 150,
            },
            ViewerMessage::Formats {
                formats: vec![
                    Format {
                        format: 0x34325241,
                        modifier: 0,
                    },
                    Format {
                        format: 0x34325258,
                        modifier: 0x0100_0000_0000_0002,
                    },
                ],
            },
            ViewerMessage::Resize {
                width: 640,
                height: 480,
                scale: 120,
            },
            ViewerMessage::BufferFailed { id: 7 },
            ViewerMessage::Release { id: 2, fence: None },
            ViewerMessage::Release {
                id: 3,
                fence: Some(fd()),
            },
            ViewerMessage::Ready,
            ViewerMessage::Key {
                time: 1000,
                keycode: 30,
                pressed: true,
            },
            ViewerMessage::Keys {
                locked: 2,
                keycodes: vec![29, 42],
            },
            ViewerMessage::KeysReleased,
            ViewerMessage::Pointer {
                time: 1001,
                x: -1.5,
                y: 1000.25,
            },
            ViewerMessage::Button {
                time: 1002,
                button: 0x110,
                pressed: false,
            },
            ViewerMessage::Scroll {
                time: 1003,
                source: 1,
                axis: 0,
                amount: -12.75,
                steps: -120,
                stop: false,
                inverted: true,
            },
            ViewerMessage::Offer {
                selection: Selection::Clipboard,
                serial: 4,
                seen: 2,
                mimes: vec!["text/plain".into(), "UTF8_STRING".into(), "".into()],
            },
            ViewerMessage::Cleared {
                selection: Selection::Primary,
                serial: 5,
            },
            ViewerMessage::Request {
                selection: Selection::Clipboard,
                serial: 9,
                mime: "text/plain;charset=utf-8".into(),
                fd: fd(),
            },
            ViewerMessage::List,
        ]
    }

    fn server_messages() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Error {
                message: "unknown opcode".into(),
            },
            ServerMessage::Welcome { version: 1 },
            ServerMessage::Output {
                width: 1280,
                height: 800,
                scale: 150,
            },
            ServerMessage::Buffer {
                id: 1,
                width: 1920,
                height: 1200,
                format: 0x34325241,
                modifier: 0x0100_0000_0000_0002,
                planes: vec![
                    Plane {
                        fd: fd(),
                        offset: 0,
                        stride: 7680,
                    },
                    Plane {
                        fd: fd(),
                        offset: 9216000,
                        stride: 512,
                    },
                ],
            },
            ServerMessage::Forget { id: 1 },
            ServerMessage::Frame { id: 1, fence: None },
            ServerMessage::Frame {
                id: 2,
                fence: Some(fd()),
            },
            ServerMessage::CursorShape {
                shape: "default".into(),
            },
            ServerMessage::CursorHidden,
            ServerMessage::Offer {
                selection: Selection::Primary,
                serial: 8,
                mimes: vec!["image/png".into()],
            },
            ServerMessage::Cleared {
                selection: Selection::Clipboard,
                serial: 9,
            },
            ServerMessage::Request {
                selection: Selection::Primary,
                serial: 4,
                mime: "text/uri-list".into(),
                fd: fd(),
            },
            ServerMessage::Workspaces {
                workspaces: vec![
                    Workspace {
                        id: 1,
                        name: "main".into(),
                    },
                    Workspace {
                        id: 2,
                        name: "naïve".into(),
                    },
                ],
            },
            ServerMessage::Showing { id: 2 },
            ServerMessage::Close {
                message: "shutting down".into(),
            },
            ServerMessage::Listing {
                workspaces: vec![Listed {
                    id: 3,
                    name: "decade".into(),
                    cwd: "/home/jack/projects/decade".into(),
                    panes: 2,
                }],
            },
        ]
    }

    fn encode_viewer(messages: Vec<ViewerMessage>) -> (Vec<u8>, Vec<OwnedFd>) {
        let mut bytes = Vec::new();
        let mut fds = Vec::new();
        for message in messages {
            message.encode(&mut bytes, &mut fds).unwrap();
        }
        (bytes, fds)
    }

    fn encode_server(messages: Vec<ServerMessage>) -> (Vec<u8>, Vec<OwnedFd>) {
        let mut bytes = Vec::new();
        let mut fds = Vec::new();
        for message in messages {
            message.encode(&mut bytes, &mut fds).unwrap();
        }
        (bytes, fds)
    }

    fn drain_viewer(decoder: &mut Decoder, out: &mut Vec<String>) {
        while let Some(message) = decoder.next_viewer_message().unwrap() {
            out.push(format!("{message:?}"));
        }
    }

    fn drain_server(decoder: &mut Decoder, out: &mut Vec<String>) {
        while let Some(message) = decoder.next_server_message().unwrap() {
            out.push(format!("{message:?}"));
        }
    }

    fn debug<T: fmt::Debug>(messages: &[T]) -> Vec<String> {
        messages.iter().map(|m| format!("{m:?}")).collect()
    }

    fn header(len: u32, opcode: u16, fds: u8, flags: u8) -> Vec<u8> {
        let mut bytes = len.to_le_bytes().to_vec();
        bytes.extend_from_slice(&opcode.to_le_bytes());
        bytes.push(fds);
        bytes.push(flags);
        bytes
    }

    // the decoded messages hold the same OwnedFds the
    // encoder took, so their Debug output, fd numbers
    // included, matches the originals exactly

    #[test]
    fn every_viewer_message_survives_encoding_and_decoding() {
        let messages = viewer_messages();
        let expected = debug(&messages);
        let (bytes, fds) = encode_viewer(messages);
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        fds.into_iter().for_each(|fd| decoder.push_fd(fd));
        let mut decoded = Vec::new();
        drain_viewer(&mut decoder, &mut decoded);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn every_server_message_survives_encoding_and_decoding() {
        let messages = server_messages();
        let expected = debug(&messages);
        let (bytes, fds) = encode_server(messages);
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        fds.into_iter().for_each(|fd| decoder.push_fd(fd));
        let mut decoded = Vec::new();
        drain_server(&mut decoder, &mut decoded);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn viewer_messages_decode_when_the_bytes_split_at_any_offset() {
        let len = encode_viewer(viewer_messages()).0.len();
        for split in 0..=len {
            let messages = viewer_messages();
            let expected = debug(&messages);
            let (bytes, fds) = encode_viewer(messages);
            let mut decoder = Decoder::new();
            fds.into_iter().for_each(|fd| decoder.push_fd(fd));
            let mut decoded = Vec::new();
            decoder.push_bytes(&bytes[..split]);
            drain_viewer(&mut decoder, &mut decoded);
            decoder.push_bytes(&bytes[split..]);
            drain_viewer(&mut decoder, &mut decoded);
            assert_eq!(decoded, expected, "split at {split}");
        }
    }

    #[test]
    fn server_messages_decode_when_the_bytes_split_at_any_offset() {
        let len = encode_server(server_messages()).0.len();
        for split in 0..=len {
            let messages = server_messages();
            let expected = debug(&messages);
            let (bytes, fds) = encode_server(messages);
            let mut decoder = Decoder::new();
            fds.into_iter().for_each(|fd| decoder.push_fd(fd));
            let mut decoded = Vec::new();
            decoder.push_bytes(&bytes[..split]);
            drain_server(&mut decoder, &mut decoded);
            decoder.push_bytes(&bytes[split..]);
            drain_server(&mut decoder, &mut decoded);
            assert_eq!(decoded, expected, "split at {split}");
        }
    }

    #[test]
    fn a_message_with_fds_takes_them_from_the_front_of_the_queue_in_argument_order() {
        let planes: Vec<OwnedFd> = (0..3).map(|_| fd()).collect();
        let raw: Vec<i32> = planes.iter().map(|fd| fd.as_raw_fd()).collect();
        let fence = fd();
        let fence_raw = fence.as_raw_fd();
        let (bytes, _) = encode_server(vec![
            ServerMessage::Buffer {
                id: 1,
                width: 1,
                height: 1,
                format: 0,
                modifier: 0,
                planes: (0..3)
                    .map(|_| Plane {
                        fd: fd(),
                        offset: 0,
                        stride: 4,
                    })
                    .collect(),
            },
            ServerMessage::Frame {
                id: 1,
                fence: Some(fd()),
            },
        ]);
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        planes.into_iter().for_each(|fd| decoder.push_fd(fd));
        decoder.push_fd(fence);

        let Some(ServerMessage::Buffer { planes, .. }) = decoder.next_server_message().unwrap()
        else {
            panic!("expected a buffer");
        };
        let decoded: Vec<i32> = planes.iter().map(|p| p.fd.as_raw_fd()).collect();
        assert_eq!(decoded, raw);
        let Some(ServerMessage::Frame { fence, .. }) = decoder.next_server_message().unwrap()
        else {
            panic!("expected a frame");
        };
        assert_eq!(fence.unwrap().as_raw_fd(), fence_raw);
    }

    #[test]
    fn a_complete_message_whose_fds_have_not_arrived_is_an_error() {
        let (bytes, _) = encode_viewer(vec![ViewerMessage::Request {
            selection: Selection::Clipboard,
            serial: 1,
            mime: "text/plain".into(),
            fd: fd(),
        }]);
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::MissingFds)
        ));
    }

    #[test]
    fn a_buffer_with_fewer_fds_queued_than_planes_is_an_error() {
        let (bytes, mut fds) = encode_server(vec![ServerMessage::Buffer {
            id: 1,
            width: 1,
            height: 1,
            format: 0,
            modifier: 0,
            planes: (0..3)
                .map(|_| Plane {
                    fd: fd(),
                    offset: 0,
                    stride: 4,
                })
                .collect(),
        }]);
        fds.pop();
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        fds.into_iter().for_each(|fd| decoder.push_fd(fd));
        assert!(matches!(
            decoder.next_server_message(),
            Err(Error::MissingFds)
        ));
    }

    #[test]
    fn an_unknown_opcode_is_an_error() {
        let mut decoder = Decoder::new();
        decoder.push_bytes(&header(8, 19, 0, 0));
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::UnknownOpcode(19))
        ));
        let mut decoder = Decoder::new();
        decoder.push_bytes(&header(8, 1000, 0, 0));
        assert!(matches!(
            decoder.next_server_message(),
            Err(Error::UnknownOpcode(1000))
        ));
    }

    #[test]
    fn nonzero_flags_are_an_error() {
        let mut decoder = Decoder::new();
        decoder.push_bytes(&header(8, 6, 0, 1));
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::Flags(1))
        ));
    }

    #[test]
    fn a_length_that_is_not_a_multiple_of_four_is_an_error() {
        let mut decoder = Decoder::new();
        decoder.push_bytes(&header(10, 6, 0, 0));
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::BadLength(10))
        ));
    }

    #[test]
    fn a_length_shorter_than_the_header_is_an_error() {
        let mut decoder = Decoder::new();
        decoder.push_bytes(&header(4, 6, 0, 0));
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::BadLength(4))
        ));
    }

    #[test]
    fn a_message_over_one_mebibyte_is_an_error_from_its_header_alone() {
        let mut decoder = Decoder::new();
        decoder.push_bytes(&header(MAX_MESSAGE_LEN as u32 + 4, 6, 0, 0));
        assert!(matches!(
            decoder.next_server_message(),
            Err(Error::TooLong(_))
        ));
    }

    #[test]
    fn encoding_a_message_over_one_mebibyte_fails_and_appends_nothing() {
        let mut bytes = vec![1, 2, 3, 4];
        let mut fds = Vec::new();
        let result = ServerMessage::CursorShape {
            shape: "x".repeat(MAX_MESSAGE_LEN),
        }
        .encode(&mut bytes, &mut fds);
        assert!(matches!(result, Err(Error::TooLong(_))));
        assert_eq!(bytes, [1, 2, 3, 4]);
    }

    #[test]
    fn a_selection_past_the_primary_selection_is_malformed() {
        let mut bytes = header(16, 14, 0, 0);
        bytes.extend_from_slice(&[2, 0, 0, 0, 1, 0, 0, 0]);
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::Malformed)
        ));
    }

    #[test]
    fn extra_bytes_after_the_arguments_are_malformed() {
        let mut bytes = header(16, 4, 0, 0);
        bytes.extend_from_slice(&[0; 8]);
        let mut decoder = Decoder::new();
        decoder.push_bytes(&bytes);
        assert!(matches!(
            decoder.next_viewer_message(),
            Err(Error::Malformed)
        ));
    }

    #[test]
    fn strings_carry_a_count_and_pad_to_a_word_with_zeros() {
        let (bytes, _) = encode_server(vec![
            ServerMessage::CursorShape {
                shape: "text".into(),
            },
            ServerMessage::CursorShape {
                shape: "default".into(),
            },
            ServerMessage::CursorShape { shape: "".into() },
        ]);
        let mut expected = header(16, 6, 0, 0);
        expected.extend_from_slice(&[4, 0, 0, 0]);
        expected.extend_from_slice(b"text");
        expected.extend_from_slice(&header(20, 6, 0, 0));
        expected.extend_from_slice(&[7, 0, 0, 0]);
        expected.extend_from_slice(b"default\0");
        expected.extend_from_slice(&header(12, 6, 0, 0));
        expected.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn a_u64_is_written_low_word_first() {
        let (bytes, _) = encode_viewer(vec![ViewerMessage::Formats {
            formats: vec![Format {
                format: 0,
                modifier: 0x1122_3344_5566_7788,
            }],
        }]);
        assert_eq!(
            &bytes[16..],
            [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
    }

    #[test]
    fn a_header_counts_the_fds_its_message_carries() {
        let (bytes, fds) = encode_viewer(vec![ViewerMessage::Release {
            id: 1,
            fence: Some(fd()),
        }]);
        assert_eq!(bytes, [16, 0, 0, 0, 5, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(fds.len(), 1);
    }

    #[test]
    fn messages_and_their_fds_cross_a_real_socket() {
        let (server, viewer) = UnixStream::pair().unwrap();
        let mut outbox = Outbox::new();

        let mut readers = Vec::new();
        let mut planes = Vec::new();
        for i in 0..4 {
            let (reader, writer) = pipe();
            readers.push(reader);
            planes.push(Plane {
                fd: writer,
                offset: i * 100,
                stride: 4096,
            });
        }
        outbox
            .push_server_message(ServerMessage::Buffer {
                id: 1,
                width: 1024,
                height: 768,
                format: 0x34325241,
                modifier: 0,
                planes,
            })
            .unwrap();

        // 40 fenced frames carry 40 fds after the buffer's 4,
        // more than one sendmsg may pass
        for id in 0..40 {
            let (reader, writer) = pipe();
            readers.push(reader);
            outbox
                .push_server_message(ServerMessage::Frame {
                    id,
                    fence: Some(writer),
                })
                .unwrap();
            outbox
                .push_server_message(ServerMessage::Showing { id })
                .unwrap();
        }
        while !outbox.send(&server).unwrap() {}
        assert!(outbox.is_empty());

        let mut decoder = Decoder::new();
        let mut received = Vec::new();
        while received.len() < 81 {
            assert!(decoder.receive(&viewer).unwrap());
            while let Some(message) = decoder.next_server_message().unwrap() {
                received.push(message);
            }
        }

        let mut written = Vec::new();
        for message in received {
            match message {
                ServerMessage::Buffer { planes, .. } => {
                    for plane in planes {
                        written.push(plane.fd);
                    }
                }
                ServerMessage::Frame { fence, .. } => written.push(fence.unwrap()),
                ServerMessage::Showing { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(written.len(), 44);
        for (i, (fd, mut reader)) in written.into_iter().zip(readers).enumerate() {
            std::fs::File::from(fd).write_all(&[i as u8]).unwrap();
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            assert_eq!(byte[0], i as u8);
        }

        drop(server);
        assert!(!decoder.receive(&viewer).unwrap());
    }
}
