# Changelog

All notable changes to decade are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not. What is versioned is the
command line and what the tool writes.

## [Unreleased]

### Added

- A unix socket for decade-aware programs, with its path in `DECADE_SOCKET`
  and the pane name in `DECADE_PANE` of everything decade starts. JSON lines:
  a hello that claims a pane, messages addressed to a pane and forwarded to
  its occupant, `focus` and `panes` for decade itself, and an error line for
  anything else. `docs/protocol.md` has the details.
- `--chat <command> [args...]` runs the command in a terminal inside the chat
  pane, in addition to the shell in the terminal pane.
- Ctrl with h, j, k or l moves focus between panes by direction; a click
  focuses the pane under the pointer.

### Changed

- decade is a Wayland compositor nested in a window, on smithay. Any Wayland
  client is a pane; the terminal pane runs ghostty. Clients are told the
  host's scale and render at it. The window chrome is a grid of character
  cells.
- The window opens on a headless Hyprland output during development; see
  `scripts/devscreen`.

## [0.1.0] - 2026-11-05

The name on crates.io, and a window.

### Added

- A wgpu window that lays out three placeholder panes and a status line on a
  grid of 9 by 18 pixel cells. The panes draw nothing yet.

[0.1.0]: https://github.com/jackroddy/decade/releases/tag/v0.1.0
