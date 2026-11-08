# decade

A cargo workspace. The root package is `decade`, a Wayland compositor nested
in a window whose panes are other programs; `decade-channel` is the client
side of its socket as a library, for programs that run in those panes and
speak the protocol in `docs/protocol.md`. The pane programs themselves live in
the decade-apps repository.

Design rationale that the code does not state lives in foam's memories, and
pending work lives in foam's issues. `foam prime` prints both at session start.
Until something runs, the README stays at its header and license, and design,
plans and decisions go to foam, never to this file.

## conventions

**Formatting.** Stock rustfmt, with no configuration. There is no
`rustfmt.toml`. Do not add one: the code is written against rustfmt's defaults.

**Comments.** Follow the `rust-comments` skill, and load it before writing or
editing a comment rather than reconstructing the rules from memory. The default
is no comment.

**Tests.** Unit tests stay inline in the file they cover. `decade-channel`'s
socket test runs a fake decade end in the test; nothing needs the compositor
running. Test names are full sentences describing the property, not
`test_foo`.

**Branches.** Work is committed on `dev`, in batches. A milestone or issue that
changes the command line or an on-disk format, and will not be finished before
the next release, goes on a branch named for it. That branch merges into `dev`
once the work is whole, and takes merges from `dev` when the two drift apart.
Nothing else branches off `dev` unless Jack asks. `main` takes a merge from
`dev` only at a release.

**Releases.** Jack says when a release is, never Claude. Until he does, the
version in `Cargo.toml` stays where it is and changelog entries accumulate
under `## [Unreleased]` with `### Added` / `### Changed` / `### Removed`
headings and no date. The version heading, the date, the `Cargo.toml` bump and
the compare link at the foot of the changelog are all part of the release
commit. Release tags are annotated and named `v<version>` for `decade` and
`decade-channel-v<version>` for the library. Publish from `main` after `dev`
merges in, with a clean tree.

**Versions.** Each crate is versioned by what changed in it. For `decade` that
is the command line, the socket protocol and what the tool writes; for
`decade-channel` its public API. Under cargo's rule that 0.x's minor is the
breaking axis, a change that breaks one of those moves that crate's minor.
Anything else moves the patch. A protocol change moves both.

**One server.** One decade server runs per user, and it holds every
workspace. There are no named sessions and no multi-user modes.

**Closing panes.** No keybind closes a pane. A pane closes only through the
x on its bar or when the program in it exits.

**Text.** decade rasterizes text at its physical pixel size and draws each
texture at that size, on whole pixels. Text is never scaled. Anything drawn
scaled, such as the drag ghost, leaves its text out.

**UI toolkit.** decade's UI kit is kept apart so it can become its own crate
once a second program needs it. The kit is the cell grid, the scene and its
shapes, colours, text rasterization, and panes: the split tree, tabs, pane
bars, and the input that resizes, moves and focuses them. A pane's content is
an id that the kit lays out and leaves a placeholder for. The application's
backend draws what each id shows. The kit imports nothing from smithay and
nothing else from decade. Wayland clients, texture upload and decade's own
widgets, such as the sidebar and the palette, stay outside it. The split is
being designed under foam milestone decade-4a4e0f. No code moves until Jack
signs off the design.

**Formats.** A change to an on-disk layout bumps that format's version byte.
The reader keeps an arm for the old layout by default. A breaking release may
drop that arm, listed under `### Removed`.

**Running decade.** Claude never starts decade expecting Jack to test it.
When Jack has to look at something or press keys, Claude builds decade, asks
him to run it, and waits for his report. For its own checks, Claude may run
decade on `headless-dev`. To look at the UI, Claude runs `decade script` against a
server with its own `XDG_RUNTIME_DIR` and `XDG_STATE_HOME`. The command
sends scripted input and saves the frames it gets back as PNG files, with
no window and no seat focus. `decade-dev` runs the debug build and `decade` the release build.
Both are links to `scripts/launch`, which rebuilds first. Claude never asks
Jack for a screenshot. When Claude needs to know how something looks, Claude
asks him to describe it.

**Windows during development.** A decade window whose class is `decade-dev`
opens on a headless Hyprland output named `headless-dev`, kept off the real
monitors by rules in Jack's Hyprland config; a plain `cargo run` opens on
Jack's screen like any window. `scripts/devscreen run ARGS...` builds decade,
creates the output if needed, and passes `--class decade-dev`.
`scripts/devscreen shot FILE` takes a screenshot of that output.

## checks

```console
$ cargo fmt --all
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features
$ cargo test --workspace --all-features
```
