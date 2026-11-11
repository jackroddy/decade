# Changelog

All notable changes to `decade-channel` are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
cargo's reading of it below 1.0: the **minor** is the breaking axis, so 0.2 to
0.3 is a breaking release and 0.2.0 to 0.2.1 is not.

## [Unreleased]

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
