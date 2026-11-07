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

**Formats.** A change to an on-disk layout bumps that format's version byte.
The reader keeps an arm for the old layout by default. A breaking release may
drop that arm, listed under `### Removed`.

**Windows during development.** A decade window whose class is `decade-dev`
opens on a headless Hyprland output named `headless-dev`, kept off the real
monitors by rules in Jack's Hyprland config; a plain `cargo run` opens on
Jack's screen like any window. Start decade with `scripts/devscreen run
ARGS...`, which builds it, creates the output if needed, and passes
`--class decade-dev`. Take screenshots with `scripts/devscreen shot FILE`. Keys
cannot be sent to that window from a script, so anything that needs typing is
checked by Jack.

## checks

```console
$ cargo fmt --all
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features
$ cargo test --workspace --all-features
```
