# Changelog

All notable changes to decade are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not. What is versioned is the
command line and what the tool writes.

## [Unreleased]

### Added

- A server holds workspaces. Each has a name, a working directory and its
  own panes. A new server starts one workspace in the home directory, named
  after that directory, with one terminal. Programs decade starts run in
  their workspace's directory, with its name in `DECADE_WORKSPACE`. Each
  workspace has its own socket in `DECADE_SOCKET` and its own MCP address
  in `DECADE_MCP`. A client or an agent reaches only the panes of its own
  workspace, and a pane name means a pane in that workspace.
- New workspace in the palette takes a path, starting at `~/`, and lists
  the directories that complete it. Tab or Enter on a directory completes
  it, and Open creates a workspace there and shows it. The workspace takes
  the directory's name, with a count after it when that name is taken. The
  status line shows the name of the workspace on screen.
- Super+Tab opens a picker with a cell for each workspace, showing its
  panes scaled down, and a last cell for a new workspace. Super+Tab, Tab,
  the arrow keys or Ctrl+H/J/K/L move between cells. Enter or a click
  shows the workspace, and Esc closes the picker. Dragging a cell moves it
  in the order. The x on a cell closes its workspace after a second press.
  While the picker is open, programs in workspaces no viewer shows draw
  twice a second, so their cells stay current.
- Close workspace in the palette asks for a second Enter, then closes every
  pane in the workspace. The workspace goes once its last client has, and
  viewers showing it move to the first one left. Closing the last workspace
  starts a new one in the home directory.
- Panes are a split tree. A new window with no anchor of its own, or
  Super+Shift+Return, splits the focused pane along its longer side; a pane
  made that way closes when its client exits.
- Holding Ctrl alone for 50 ms shows a bar over the top row of each pane
  with its name and buttons to pin, zoom and close it. Dragging the outline
  between two panes resizes them.
- Pinning a pane makes it an anchor for its program's app id. An anchor
  stays as an empty slot, saying what it waits for, when its client goes,
  and takes it back when it returns. Unpinning an anchor makes it a pane
  that closes with its program.
- Ctrl+dragging a pane, or dragging it by the name on its bar, carries a
  small copy of it under the pointer. Dropping it on a target drawn over
  another pane swaps the two or splits that pane on one side. Dropping it
  on a window edge makes it span that side.
- Super+Shift+Space opens a command palette with Launch and Settings. Typing
  filters the list, the arrow keys move, and Esc goes back or closes it.
  Launch lists the installed GUI apps and starts the chosen one as a new
  pane.
- Settings live in `~/.config/decade/config.toml`. Settings in the palette
  opens a panel that changes them and saves the file.
- Scroll wheel events reach clients.
- decade is an MCP server over streamable HTTP, at the address in
  `DECADE_MCP` in the environment of everything it starts. Its tools are
  `focus`, `panes`, `code.show`, `code.mark` and `code.clear`; a call becomes
  the matching socket message and decade's answer is the tool result.
- A unix socket for decade-aware programs, with its path in `DECADE_SOCKET`
  and the pane name in `DECADE_PANE` of everything decade starts. JSON lines:
  a hello that claims a pane, messages addressed to a pane and forwarded to
  its occupant, `focus` and `panes` for decade itself, and an error line for
  anything else. `docs/protocol.md` has the details.
- `decade start`, `decade stop`, `decade restart` and `decade status`
  control the server. `start` runs a server in the background and opens no
  window. `stop` ends the server and every program in its panes. `status`
  prints the server's pid, and exits with 1 when no server is running.
- `--class <app id>` sets the window's app id, for window rules. The default
  is `decade`.
- Super with h, j, k or l moves focus between panes by direction,
  Super+Shift swaps the focused pane with its neighbour, Super with an arrow
  key moves the nearest divider, and Super+M zooms the focused pane. A click
  focuses the pane under the pointer.
- Held keys repeat in decade's own bindings, the palette and the settings
  panel, at the delay and rate Hyprland is set to.
- Under Hyprland, while decade's window is the only one on its workspace,
  Hyprland passes decade the keys it would otherwise take. Binds marked
  `dont_inhibit` keep working in Hyprland.

### Changed

- decade runs as two parts. `decade server` owns the panes and their
  programs and has no window. `decade` opens a window that shows the server
  and sends it input, and starts a server first if none is running. Closing
  the window leaves the server and its programs running, and running
  `decade` again shows them again. The two talk over the protocol in
  `docs/viewer.md`.
- Copy and paste work between panes, and between panes and the host, for
  both the clipboard and the primary selection. Super+C, Super+V and
  Super+X copy, paste and cut in the focused pane.
- A pane is drawn as a box-drawing outline, one stroke wide, in the accent
  colour on the focused pane and a muted one elsewhere; the title strips are
  gone. Colours come from a palette copied from the Omarchy theme.
- decade is a Wayland compositor nested in a window, on smithay. Any Wayland
  client is a pane; the terminal pane runs ghostty. Clients are told the
  host's scale and render at it. decade draws its own chrome on the GPU.
- The window opens on a headless Hyprland output during development; see
  `scripts/devscreen`.
- Clients in panes render on the same GPU as decade.

## [0.1.0] - 2026-11-05

The name on crates.io, and a window.

### Added

- A wgpu window that lays out three placeholder panes and a status line on a
  grid of 9 by 18 pixel cells. The panes draw nothing yet.

[0.1.0]: https://github.com/jackroddy/decade/releases/tag/v0.1.0
