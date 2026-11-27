# Changelog

All notable changes to `decade-channel` are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not.

## [Unreleased]

### Added

- `Channel::record` sends a `record` message carrying a `Record`: the
  pane, the absolute path of the file decade writes, the optional pane
  of the agent asking, and the optional `Format`, frame rate, duration,
  width, quality, pointer, pointer scale and loop settings. decade's
  answer arrives once the file is written, as `Message::Record` with a `Recorded`, or
  as `Message::Error`. `Recorded` gives the file, the frames written and
  dropped, the seconds recorded and what ended the recording.
- `Channel::record_stop` sends a `record.stop` message carrying a
  `RecordStop`, which ends the recording of a pane.
- `Message::Notify` carries a `Notify` from decade: a notice for the
  agent in the client's pane about a call that ended after it returned.
  `Notify` holds the `text` for the agent, the call it is `about`, the
  `pane` the call was about, and the call's `result` as JSON when it
  has one. decade sends one when a recording ends without a
  `record.stop`.

### Changed

- `Message` and `Reply` each have `Record` and `Notify` variants. A
  `match` over either without a wildcard arm no longer compiles.

## [0.2.0] - 2026-11-26

### Added

- `Channel::screenshot` sends a `screenshot` message carrying a
  `Screenshot`: the pane, the file the client will write, and the
  optional pane of the agent asking. decade's answer arrives as
  `Message::Screenshot` with the PNG's bytes, or as `Message::Error`.
  `Reply::Screenshot` holds a `Captured`, the PNG in base64 as decade
  writes it.
- `AgentUpdate` has a `subagents` field with the list of sub-agents and
  background tasks the agent started. Each `Subagent` has a name, an
  optional activity and a `SubagentState` of working, done or failed.
  Like the other fields, an absent list keeps decade's last one, and
  `null` or an empty list clears it.
- `Agent` has an `args` field with the flags the program was started
  with, so decade can start it again on the same session. A hello
  without `args` reads as empty.
- `Hello` has a `restore` field holding a `Restore`, the argv that starts
  the program again, whether it runs in a terminal and the optional
  directory to start it in.
  `Channel::restore` sends a `restore` message that replaces it, and an
  empty argv clears it.

### Changed

- `Pane::scratch` can be true for several panes in one list, since a
  workspace can now have several scratch panes.

## [0.1.0] - 2026-11-12

### Added

- `Channel::connect` and `Channel::from_env` open the decade socket, send
  hello, and hand back a receiver of `Message`s read on a thread; `send`
  and `panes` write to it. `parse` turns one line into a `Message`.
- `Shows` is a kind of thing a program can show. `Channel::connect` and
  `Channel::from_env` take the program's kinds and send them in hello.
  `Show`, `Highlight` and `Clear` are the payloads of the standard verbs.
  `parse` returns those verbs as `Message::Show`, `Message::Highlight` and
  `Message::Clear`. `Pane` has a `shows` field with its occupant's kinds.
- `Hello`, `Welcome`, `Error`, `Pane` and `AgentState` are the messages a
  client and decade exchange. `Reply` holds the lines decade
  writes other than forwarded verbs. decade uses these same types.
- `Agent` holds the name, version and session an agent gives in hello.
  `Channel::connect` and `Channel::from_env` take an optional
  `Agent`. `Channel::agent` sends an `AgentUpdate`, which writes only the
  fields that are set and writes null for a field being cleared.
  `Channel::ended` ends the agent's session. `Context` and `Tokens` are
  the update's context use and token counts.
- `Verb` is a verb a program accepts, with an optional sentence saying
  what it does. `Hello::accepts` holds `Verb`s, and `Channel::connect` and
  `Channel::from_env` take them. A verb without a description is written
  as a bare name, as before. A `&str` converts into one. `Pane` has an
  `accepts` field with the verbs its occupant listed in hello.

[Unreleased]: https://github.com/jackroddy/decade/compare/decade-channel-v0.2.0...HEAD
[0.2.0]: https://github.com/jackroddy/decade/compare/decade-channel-v0.1.0...decade-channel-v0.2.0
[0.1.0]: https://github.com/jackroddy/decade/releases/tag/decade-channel-v0.1.0
