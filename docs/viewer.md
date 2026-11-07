# The viewer protocol

A decade server runs the compositor and owns every pane. A viewer shows one
of the server's workspaces in a window and sends input back. This document is
the wire between them. It borrows Wayland's wire format and drops its object
model. It is decade's own protocol, and nothing here promises compatibility
with Wayland or anything else.

Version 1 defines a local viewer, on the same machine as the server. Frames
cross as dmabufs. File descriptors travel beside messages. A remote viewer
will use the same messages over a network stream, with encoded frames in
place of dmabufs. Version 1 reserves room for that without defining it.

## Transport

A local viewer connects to the server's unix stream socket. The design of
the commands that start and attach to a server sets the socket's path, not
this document.

Either side may close the connection at any time. A viewer that closes has
detached. The server keeps running.

## Wire format

Every message starts with an 8-byte header, followed by its arguments. All
integers are little-endian.

| bytes | field | meaning |
|---|---|---|
| 0–3 | length | the whole message in bytes, header included, a multiple of 4 |
| 4–5 | opcode | the message, numbered per direction |
| 6 | fds | how many file descriptors travel with the message |
| 7 | flags | 0 in version 1 |

A message is at most 1 MiB long. A receiver rejects a longer message, a
nonzero `flags` and an opcode it does not know. It sends `error` and closes.
The `fds` count lets a later version skip messages it does not know.

Arguments follow in the order a message lists them. Each takes a whole
number of 4-byte words.

| type | encoding |
|---|---|
| `u32`, `i32` | one word |
| `u64` | two words, low word first |
| `fixed` | a signed 24.8 fixed-point number in one word, as in Wayland |
| `scale` | a `u32` holding a scale in 120ths: 120 is 1, 150 is 1.25 |
| `string` | a `u32` byte count, then UTF-8 bytes with no terminator, padded with zeros to a word |
| `bytes` | a `u32` byte count, then the bytes, padded with zeros to a word |
| `fd` | no bytes in the message. One file descriptor, passed as `SCM_RIGHTS` ancillary data on the socket |

### File descriptors

On a Linux stream socket, the kernel delivers ancillary data with the first
byte of the `sendmsg` call that carried it. The rules below follow from that.

- A sender passes a message's fds in the same `sendmsg` call as the
  message's first byte, never later.
- A sender passes at most 28 fds in one `sendmsg` call.
- A receiver reads with ancillary space for at least 28 fds. If the kernel
  reports truncated ancillary data (`MSG_CTRUNC`), the receiver sends
  `error` and closes.
- The receiver keeps received fds in a queue. A message takes its `fds`
  count from the front of the queue, in the order its arguments list them. A
  complete message whose fds are not all in the queue is an error.

## Hello

The viewer speaks first. `hello` has this exact layout in every version of
the protocol. Any server can read it.

`hello(version: u32, transport: u32, width: u32, height: u32, scale: scale)`
: `version` is the newest protocol version the viewer speaks. `transport` is
  0 for a local viewer. `width` and `height` are the window's size in
  logical pixels. `scale` is the host's scale for it.

Right after `hello`, a local viewer sends `formats`.

The server answers with `welcome`, then the workspace list (see
Workspaces), then everything else.

`welcome(version: u32)`
: The version both sides use from now on: the lower of the two. A server
  that cannot speak any version the viewer offers sends `error` and closes.

Before `welcome`, the viewer may send nothing but `hello` and `formats`.
The one exception is `list` (see Workspaces), which a client that only
wants the workspace list sends in place of `hello`.

## Size and scale

A window's physical size is `round(logical size × scale)`, computed the way
the host computes it. The server renders at physical size. The viewer shows
each buffer one to one, with `wp_viewporter` setting the logical size. A
local viewer never scales a frame of its own size.

Each pane's client has one size. Only one viewer can decide the size of the
layout. The **active viewer** is the one that most recently sent `hello`,
`resize`, `key` or `button`. Pointer motion and scrolling do not change the
active viewer. The server sizes its output to the active viewer's window.

`resize(width: u32, height: u32, scale: scale)`
: Viewer to server, when the window's logical size or scale changes.

`output(width: u32, height: u32, scale: scale)`
: Server to viewer, whenever the output's logical size or scale changes. A
  viewer that is not active receives frames of the active viewer's size. It
  scales them to fit its own window, keeping their aspect ratio.

## Frames

### Formats

The server can only allocate buffers the viewer's host can import. The
viewer tells it what the host accepts before the first buffer.

`formats(device: u64, count: u32, then for each: format: u32, modifier: u64)`
: Viewer to server. `device` is the `dev_t` of the host's main device, from
  the host's dmabuf feedback. The pairs are every DRM fourcc and modifier
  the host can import for this window. The viewer sends `formats` again
  whenever the host's feedback changes. For example, a scanout tranche
  appears when the window goes fullscreen.

The server allocates on a device the host can read. It picks a format it can
render to that the host also lists. It falls back to `LINEAR` when nothing
else matches.

### Buffers

The server renders into a pool of buffers for each viewer and lends them to
that viewer. A buffer belongs to the server until it sends it in `frame`. It
belongs to the viewer from then until the viewer sends `release`. The server
never draws into a buffer the viewer holds.

The host keeps the last buffer the viewer showed until the viewer shows
another. The viewer therefore always holds at least one buffer. A pool has
at least three buffers.

`buffer(id: u32, width: u32, height: u32, format: u32, modifier: u64,
opaque: u32, planes: u32, then for each plane: fd, offset: u32, stride: u32)`
: Server to viewer. Announces a dmabuf, once, before the first `frame` that
  uses it. Sizes are physical pixels, and row 0 is the top. `opaque` is 1
  when the frame has no transparency. The viewer can then mark its surface
  opaque.

`buffer_failed(id: u32)`
: Viewer to server. The host could not import buffer `id`. The server
  forgets the buffer and allocates another, with a different format or
  modifier, or `LINEAR`.

`forget(id: u32)`
: Server to viewer. The server will not use the buffer again. The viewer
  drops its import. The server sends this only for a buffer the viewer does
  not hold.

`frame(id: u32, fence: u32, damage: u32, then for each damage rect: x: i32,
y: i32, width: u32, height: u32, then if fence is 1: fd)`
: Server to viewer. Buffer `id` holds a new frame, of that buffer's size.
  Damage rects are in buffer pixels. A frame with no rects is wholly
  damaged.
: When `fence` is 1, the fd is a sync_file that signals when the server has
  finished rendering into the buffer. The viewer must not let the buffer
  reach the screen before then. It may wait on the fence itself, or hand it
  to the host, through `linux-drm-syncobj-v1` or by importing it into the
  dmabuf. When `fence` is 0, the buffer relies on implicit sync.

`release(id: u32, fence: u32, then if fence is 1: fd)`
: Viewer to server. The viewer and its host are done with buffer `id`. When
  `fence` is 1, the fd is a sync_file that signals when the host's last read
  finishes. The server waits on it before drawing into the buffer again.
  When `fence` is 0, the host released the buffer under implicit sync.

### Pacing

`ready()`
: Viewer to server. Now is a good time for the next frame, in the sense of a
  Wayland frame callback: the viewer received one from its host. The server
  sends at most one `frame` per `ready`, plus one after `welcome`. `ready`
  does not accumulate. A second `ready` before the next `frame` counts as
  one. With no viewer attached, the server renders nothing.

### Changing size

Frames of the old size may still arrive after `output` changes the size.
The server rendered them first. A frame's size is always its
buffer's size, never the last `output`. The viewer scales an old-size frame
to fit, as an inactive viewer does.

The server forgets each old-size buffer once the viewer has released it. It
allocates new buffers at the new size.

## Input

Positions are in physical pixels of the frame on the viewer's screen, with
the origin at the top left. A viewer that scales frames converts the pointer
position before sending it. Positions may be negative, or beyond the
frame's size, while a button is held and the host keeps sending motion from
outside the window. Times are milliseconds from an arbitrary start,
increasing.

The server has one seat. It tracks the keys and buttons each viewer holds.
When a viewer loses focus or detaches, the server releases only that
viewer's. Messages on different viewers' connections have no order between
them. The server handles each one when it arrives.

`key(time: u32, keycode: u32, pressed: u32)`
: A key went down (`pressed` 1) or up (0). `keycode` is a Linux evdev code.
  The server owns the keymap, and the host's layout plays no part. The
  server works out the modifier state from the keys it receives. It repeats
  keys itself, for decade's own bindings and for clients. A viewer sends no
  repeats.

`keys(locked: u32, count: u32, then for each: keycode: u32)`
: The window gained keyboard focus. `locked` holds the host's lock state:
  bit 0 for Caps Lock, bit 1 for Num Lock. The keycodes are the keys held
  down. The server presses them without repeating.

`keys_released()`
: The window lost keyboard focus. The server releases every key this viewer
  had pressed.

`pointer(time: u32, x: fixed, y: fixed)`
: The pointer moved to `x`, `y`.

`pointer_left()`
: The pointer left the window. A button still down stays down until its
  `button` release arrives. The host's implicit grab delivers that release.

`button(time: u32, button: u32, pressed: u32)`
: A mouse button changed. `button` is a Linux evdev code, such as `BTN_LEFT`.

`scroll(time: u32, source: u32, axis: u32, amount: fixed, steps: i32,
stop: u32, inverted: u32)`
: A scroll on one axis. `source` is 0 for a wheel, 1 for a finger, 2 for
  continuous and 3 for a wheel tilt, as in Wayland. `axis` is 0 for
  vertical and 1 for horizontal. `amount` is in physical pixels of the
  frame. A positive amount scrolls down or right, as in Wayland. `steps` is
  in 120ths of a wheel notch, 0 when the source has none. `stop` is 1 when a
  finger lifted off this axis, with an amount of 0. `inverted` is 1 when the
  host has natural scrolling on.

## Cursor

The viewer draws the cursor itself, with its host's cursor. The cursor then
moves at the host's rate and never waits for a frame. Each viewer may show a
different workspace. A cursor message applies only to the viewer that
receives it.

`cursor_image(id: u32, hotspot_x: i32, hotspot_y: i32, width: u32, height:
u32, scale: scale, pixels: bytes)`
: Server to viewer. Defines cursor image `id`, for later `cursor` messages.
  Pixels are premultiplied ARGB8888, `width` times `height`, row by row. The
  hotspot is in image pixels. `scale` is the scale the image was drawn for.
  A new image with an existing `id` replaces the old one.

`cursor(id: u32)`
: Server to viewer. Show cursor image `id`.

`cursor_shape(shape: string)`
: Server to viewer. A named cursor, using the names of the cursor-shape
  protocol, such as `default`, `text` or `pointer`. The viewer shows the
  matching cursor from its host's theme.

`cursor_hidden()`
: Server to viewer. No cursor over the frame.

## Clipboard

Two selections cross between server and viewer.

| number | selection |
|---|---|
| 0 | the clipboard |
| 1 | the primary selection |

Either side can own a selection. The same messages run in both directions.
A side offers a selection when one of its own clients, or its host, sets it.

`offer(selection: u32, serial: u32, seen: u32, count: u32, then for each:
mime: string)`
: The sender owns `selection`, with data available in these MIME types.
  `serial` names this offer. Each side numbers its own offers, increasing,
  and a newer offer from a side replaces its older one. `seen` is the
  serial of the newest offer for this selection that the sender had
  received from the other side, or 0.
: The receiver presents the offer to its own clients, or to its host, as
  their selection.

`cleared(selection: u32, serial: u32)`
: The sender no longer has data for `selection`. `serial` numbers this
  change the way it numbers offers.

Two rules keep the sides in agreement:

- A side never offers back a selection it set from the other side's offer.
  When its own clients or host then report that selection as new, it sends
  nothing.
- When offers cross, the server's offer wins. The server ignores a viewer
  offer whose `seen` is older than the server's newest offer for that
  selection. The viewer adopts every server offer it receives.

`request(selection: u32, serial: u32, mime: string, then: fd)`
: The sender wants the data from offer `serial` as `mime`. The fd is the
  write end of a pipe. The owner writes the data into it and closes it. The
  requester reads until end of file. A requester that no longer wants the
  data closes the read end. An owner that cannot provide the data closes
  the write end without writing.

Data never travels inside messages. A large paste never delays frames or
input. A remote viewer cannot pass a pipe. Remote transfers will need their
own messages, with flow control.

## Workspaces

`workspaces(count: u32, then for each: id: u32, name: string)`
: Server to viewer. The whole list, after `welcome` and whenever it changes.

`showing(id: u32)`
: Server to viewer. The workspace this viewer shows.

`show(id: u32)`
: Viewer to server. Show workspace `id` in this viewer. Each viewer shows
  its own workspace. The server answers with `showing`.

`list()`
: Client to server, as the first message in place of `hello`. The server
  answers with `listing` and closes the connection. `decade list` uses it.
  A viewer that has had `welcome` may send it too, and stays connected.

`listing(count: u32, then for each: id: u32, name: string, cwd: string, panes: u32)`
: Server to client. Every workspace in the order the picker shows them,
  with its working directory and its number of panes.

## Closing

`close(reason: u32, message: string)`
: Server to viewer, before the server closes the connection for a reason
  other than an error. `message` is for a person to read.

| reason | meaning |
|---|---|
| 1 | the server is shutting down |
| 2 | another reason, given in `message` |

`error(code: u32, message: string)`
: Either direction. `message` is for a person to read. After `error`, the
  sender closes the connection.

| code | meaning |
|---|---|
| 1 | an unknown opcode, a malformed message, or a missing or truncated fd |
| 2 | no protocol version in common |
| 3 | any other failure |

## Opcodes

Viewer to server:

| opcode | message |
|---|---|
| 0 | `error` |
| 1 | `hello` |
| 2 | `formats` |
| 3 | `resize` |
| 4 | `buffer_failed` |
| 5 | `release` |
| 6 | `ready` |
| 7 | `key` |
| 8 | `keys` |
| 9 | `keys_released` |
| 10 | `pointer` |
| 11 | `pointer_left` |
| 12 | `button` |
| 13 | `scroll` |
| 14 | `offer` |
| 15 | `cleared` |
| 16 | `request` |
| 17 | `show` |
| 18 | `list` |

Server to viewer:

| opcode | message |
|---|---|
| 0 | `error` |
| 1 | `welcome` |
| 2 | `output` |
| 3 | `buffer` |
| 4 | `forget` |
| 5 | `frame` |
| 6 | `cursor_image` |
| 7 | `cursor` |
| 8 | `cursor_shape` |
| 9 | `cursor_hidden` |
| 10 | `offer` |
| 11 | `cleared` |
| 12 | `request` |
| 13 | `workspaces` |
| 14 | `showing` |
| 15 | `close` |
| 16 | `listing` |

Opcodes from 1000 up, in both directions, are reserved for remote frames and
remote clipboard transfers.
