# decade

A single-crate cargo project: the `decade` command-line tool.

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

**Tests.** Unit tests stay inline in the file they cover. `tests/cli.rs`, once
it exists, drives the real binary so that argv parsing, exit codes and stderr
are pinned alongside the output. Test names are full sentences describing the
property, not `test_foo`.

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
commit. Release tags are annotated and named `v<version>`. Publish from `main`
after `dev` merges in, with a clean tree.

**Versions.** What is versioned is the command line and what the tool writes.
Under cargo's rule that 0.x's minor is the breaking axis, a change that breaks
either moves the minor. Anything else moves the patch. `tests/cli.rs` is what
says whether the interface moved.

**Formats.** A change to an on-disk layout bumps that format's version byte.
The reader keeps an arm for the old layout by default. A breaking release may
drop that arm, listed under `### Removed`.

**Windows during development.** A decade window opens on a headless Hyprland
output named `headless-dev`, kept off the real monitors by rules in Jack's
Hyprland config. Run `scripts/devscreen` before starting decade so the output
exists, and take screenshots with `scripts/devscreen shot FILE`. Keys cannot
be sent to that window from a script, so anything that needs typing is checked
by Jack.

## checks

```console
$ cargo fmt --all
$ cargo fmt --all --check
$ cargo clippy --all-targets --all-features
$ cargo test --all-features
```
