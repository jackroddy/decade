# Changelog

All notable changes to `decade-channel` are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not.

## [Unreleased]

### Added

- `Channel::connect` and `Channel::from_env` open the decade socket, send
  hello, and hand back a receiver of `Message`s read on a thread; `send`,
  `focus` and `panes` write to it. `parse` turns one line into a `Message`.
- The `code` module with the `show`, `mark` and `clear` verbs and their
  payload types, shared by the code pane and whoever drives it.

