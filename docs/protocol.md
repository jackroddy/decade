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

## Messages to decade

```json
{"focus": {"pane": "terminal"}}
{"panes": {}}
```

`focus` moves keyboard focus to the pane and has no reply. In a workspace
that is not on screen, the pane has focus once the workspace is shown.
`panes` is answered with every pane in the workspace and its occupant:

```json
{"panes": [{"name": "chat", "client": "coax"}, {"name": "terminal", "client": null}, {"name": "code", "client": null}]}
```

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
workspace. Its tools are decade's own `focus` and `panes` and, for
the code pane, `code.show`, `code.mark` and `code.clear`, each with the payload
described above as its arguments. A tool call becomes the matching socket
message from a built-in client named `agent`, so a pane sees `"from": "agent"`.
decade's answer, an error line or the pane list, is the tool result; a message
that was only forwarded answers "sent". coax passes `DECADE_MCP` to the agent
in the ACP session's server list, so any agent coax runs sees these tools.

## Version

`welcome` carries `version`, 1 today. The number moves when a message changes
shape or meaning. Adding a verb does not move it.
