# The decade socket

The `decade-channel` crate in this repository is the client side of this
protocol for Rust programs. What follows is the wire itself.

decade listens on one unix socket for each workspace while it runs.
Programs it starts find these in their environment:

| variable | value |
|---|---|
| `DECADE_SOCKET` | the path of their workspace's socket |
| `DECADE_WORKSPACE` | the workspace's name |
| `DECADE_PANE` | the name of the pane they were started in, if any |

A client reaches only the panes of the workspace whose socket it connected
to. Every pane name below means a pane in that workspace. Each direction
carries one JSON object per line.

## Hello

The first line a client sends:

```json
{"hello": {"name": "code-pane", "accepts": ["show", "mark", "clear"], "pane": "code"}}
```

`name` is how other clients and error messages refer to it. `accepts` lists
the verbs it handles; an empty list accepts everything. `pane` claims the pane
of that name, normally the value of `DECADE_PANE`, and is optional: a client
with no pane can still send. decade answers:

```json
{"welcome": {"pane": "code", "version": 1}}
```

`pane` is null when no pane was claimed, when the name is unknown, or when
another client holds it; in the last two cases an error line comes first.
Every other message before hello is answered with an error.

A client may send hello again. The new hello replaces its name and
`accepts` list. The client gives up the pane it held and claims the one the
new hello names, if any.

## Messages to a pane

A message names a pane in `to` and carries exactly one verb. decade delivers
it to the client occupying that pane, with `to` replaced by `from`:

```json
{"to": "code", "show": {"path": "src/gpu.rs", "line": 40}}
```

arrives at the code pane as

```json
{"show": {"path": "src/gpu.rs", "line": 40}, "from": "coax"}
```

The verbs so far, and what a code pane does with them:

| Verb | Payload | Meaning |
|---|---|---|
| `show` | `path`, optional `line` | open the file, scroll to the line |
| `mark` | `path`, `lines` as `[[from, to], ...]` | highlight those line ranges |
| `clear` | none | drop every mark |

Verbs are not checked by decade beyond the occupant's `accepts` list; the
payload is the business of the two clients.

A pane verb may not be one of these names, which decade keeps for its own
messages: `hello`, `status`, `panes`, `welcome`, `error` and `from`.
decade answers a message to a pane that uses one with an error and
does not forward it.

## Messages to decade

A message without `to` is for decade, and carries exactly one of its verbs.

```json
{"panes": {}}
```

`panes` is answered with every pane in the workspace and its occupant:

```json
{"panes": [{"name": "chat", "client": "coax"}, {"name": "terminal", "client": null}, {"name": "code", "client": null}]}
```

## Agent state

A client that claimed a pane reports what the agent in that pane is doing:

```json
{"status": {"state": "attention", "message": "which branch should this go on?"}}
```

| state | meaning |
|---|---|
| `attention` | the agent is waiting on the person, with a question or a request |
| `done` | the agent finished what it was asked to do |
| `working` | the agent is working |
| `idle` | an agent is present and doing nothing |

`message` is optional and is shown beside the state. A state holds until
it is replaced, either by the client that reported it or by the person at
the screen, who can return `done` and `attention` to `idle`. Focusing the
pane changes nothing. A pane has no agent when its client never reported,
has disconnected, or gave the pane up with a new hello. A workspace shows the most urgent state of
its panes, in the order of the table. When a pane enters `attention` and no
focused viewer shows its workspace, decade raises a desktop notification.
There is no reply. A client that claimed no pane gets an error.

## Errors

Anything decade cannot act on comes back on the same connection:

```json
{"error": {"reason": "no client occupies pane terminal", "message": {"to": "terminal", "show": {}}}}
```

`message` is the offending line as parsed, or as a string when it was not
JSON. decade never drops a message silently.

## Agents: MCP

Agents do not use this socket. decade is also an MCP server, over streamable
HTTP at the address in `DECADE_MCP`, which every program decade starts gets
beside `DECADE_SOCKET`. Each workspace has its own address, ending in
`?workspace=<id>`, and a tool call reaches only the panes of that
workspace. Its tools are decade's own `panes` and `point` and, for
the code pane, `code.show`, `code.mark` and `code.clear`, each with the payload
described above as its arguments. decade delivers a pane verb from a tool
call with `"from": "agent"`. decade answers `panes` with the pane list, in the
shape of the socket's `panes` reply, and every other call with a line saying
what it did. When decade cannot act on a call, the result is a tool error
whose text is the reason the socket would give in `error`. decade refuses a
call to an address without `?workspace=` as invalid parameters. coax passes
`DECADE_MCP` to the agent in the ACP session's server list, so any agent coax
runs sees these tools.

`point` takes `pane` and an optional `note`. decade pulses the pane's
outline in magenta and puts the note on its top edge. Keyboard focus stays
where it is. When the pane is in a tab that is not shown, decade pulses
that tab's pill on the top bar. When the workspace is not shown, decade
pulses its card or tile in the sidebar. The point ends when the person
focuses the pane, when the pane goes, after 30 seconds, or when the agent
points again. A workspace has one point at a time.

The person may mark one pane in a workspace as its scratch pane, from the
Ctrl bar. Two tools reach it. `scratch.open` takes `program`, optional
`args` and optional `terminal`. `terminal` defaults to true. decade refuses
the call unless the workspace has a scratch pane and `program` is a bare
name starting with `decade-` that is on decade's `PATH`. With `terminal`
true the program runs inside ghostty in the scratch pane. With `terminal`
false decade starts it directly. decade puts the first window of that
program, or of a process under it, in the scratch pane. Either way the
program gets the pane's name in `DECADE_PANE`. When a program already
occupies the scratch pane, decade asks it to close. decade starts the new
program once the old one has gone. `scratch.send` takes `verb` and
`payload`. decade sends `{"to": "<scratch pane>", "<verb>": payload}` from
`agent`, and the program in the scratch pane receives it like any other
message.

## Version

`welcome` carries `version`, 1 today. The number moves when a message changes
shape or meaning. Adding a verb does not move it.
