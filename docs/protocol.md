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

The person can rename a pane. A pane answers to its former names for as
long as it lives, so a program started before the rename can keep using
the name in its `DECADE_PANE` and `DECADE_MCP`. A hello, a `to` and the
`pane` of an MCP call or address may give a former name. decade's replies
give only the current name, in `welcome` and in `panes`.

## Hello

The first line a client sends:

```json
{"hello": {"name": "decade-code", "pane": "code", "shows": [{"kind": "file", "highlight": true}]}}
```

`name` is how other clients and error messages refer to it. `accepts`
lists the verbs it handles. Each verb is a bare name, or an object with
its `name` and a `description` of one sentence saying what it does:

```json
{"hello": {"name": "decade-notes", "pane": "scratch", "accepts": ["clear", {"name": "append", "description": "Add the payload's text at the end of the notes."}]}}
```

decade accepts a verb either way. The description is for agents, which
read it in `panes`. `pane` claims the pane of that name, normally
the value of `DECADE_PANE`, and is optional: a client with no pane can
still send. `shows` lists the kinds of thing the client can show, and is
optional. Each kind has a name and says whether the client can highlight
it. `highlight` defaults to false.

A client that runs an agent adds an `agent` block with the agent's name,
its version and the id of its session:

```json
{"hello": {"name": "coax", "pane": "chat", "agent": {"name": "coax", "version": "0.4.0", "session": "7f3a"}}}
```

A client without it is a plain pane program.

When `accepts` is empty or missing, a client that lists no kinds takes
every verb. A client that lists kinds takes only `show`, `highlight` and
`clear`. decade answers:

```json
{"welcome": {"pane": "code", "version": 1}}
```

`pane` is null when no pane was claimed, when the name is unknown, or when
another client holds it; in the last two cases an error line comes first.
Every other message before hello is answered with an error.

A client may send hello again. The new hello replaces its name, its
`accepts` list and its `shows` list. The client gives up the pane it held
and claims the one the new hello names, if any. A hello that names the
session of an agent the workspace holds attaches to that agent, as
described under Agents.

## Messages to a pane

A message names a pane in `to` and carries exactly one verb. decade delivers
it to the client occupying that pane, with `to` replaced by `from`:

```json
{"to": "code", "reload": {"path": "src/gpu.rs"}}
```

arrives at the code pane as

```json
{"reload": {"path": "src/gpu.rs"}, "from": "coax"}
```

decade checks a verb only against the occupant's `accepts` list. The
payload is the business of the two clients. The three verbs below are the
exception.

A pane verb may not be one of these names, which decade keeps for its own
messages: `hello`, `agent`, `ended`, `panes`, `welcome`, `error` and
`from`.
decade answers a message to a pane that uses one with an error and
does not forward it.

## Show, highlight and clear

`show`, `highlight` and `clear` are standard verbs. Each names a kind, and
decade delivers it only to an occupant whose hello listed that kind in
`shows`. The occupant receives them whatever its `accepts` list says.
`highlight` and `clear` also need the kind to have `highlight` true.

| Verb | Payload | Meaning |
|---|---|---|
| `show` | `kind`, `target`, optional `at` | bring `target` into view, at `at` |
| `highlight` | `kind`, `target`, `ranges` | mark `ranges` of `target`, replacing the marks before |
| `clear` | `kind` | drop every mark of that kind |

```json
{"from": "agent", "show": {"kind": "file", "target": "src/gpu.rs", "at": 40}}
{"from": "agent", "highlight": {"kind": "file", "target": "src/gpu.rs", "ranges": [[10, 14]]}}
{"from": "agent", "clear": {"kind": "file"}}
```

`kind` and `target` are strings. `at` and `ranges` can be any JSON value,
and their form depends on the kind. decade passes them through unread.
decade-code shows `file`. Its target is a path, `at` is a 1-based line, and
`ranges` is a list of inclusive 1-based `[from, to]` line pairs. A `show`
without `at` leaves an open file where it is and starts any other file at
the top.

decade refuses one of these verbs when its payload lacks `kind` or
`target`, when the occupant does not show the kind, or when the occupant
cannot highlight it.

## Messages to decade

A message without `to` is for decade, and carries exactly one of its verbs.

```json
{"panes": {}}
```

`panes` is answered with every pane in the workspace, its occupant, the
kinds the occupant shows and the verbs its hello listed in `accepts`, in
the form the hello gave them. `client` is null when no client in the
pane has said hello, though a terminal or another window may be there.
`scratch` is true for the workspace's scratch pane, and `you` is true for
the pane of the client that asked:

```json
{"panes": [{"name": "chat", "client": "coax", "shows": [], "accepts": [], "scratch": false, "you": true}, {"name": "terminal", "client": null, "shows": [], "accepts": [], "scratch": true, "you": false}, {"name": "code", "client": "decade-code", "shows": [{"kind": "file", "highlight": true}], "accepts": [{"name": "reload", "description": "Read the file from disk again."}], "scratch": false, "you": false}]}
```

## Agents

A client whose hello named an agent and claimed a pane reports on that
agent with `agent`. Every field is optional:

```json
{"agent": {"state": "attention", "question": "which branch should this go on?", "model": "opus", "context": {"used": 81000, "limit": 200000}, "tokens": {"input": 120000, "output": 9000}, "cost": 1.42}}
```

| field | value |
|---|---|
| `state` | one of the states below |
| `activity` | what the agent is doing now |
| `question` | what the agent needs from the person |
| `model` | the model the agent runs on |
| `context` | `used` and `limit`, the tokens of its context window in use and the window's size |
| `tokens` | `input` and `output`, the tokens its session has read and written |
| `cost` | what the session has cost so far, a number |

decade merges each `agent` into what it holds for the agent. A field the
message leaves out keeps its value. A field set to `null` is cleared. An
agent that has its answer sends `"question": null`.

| state | meaning |
|---|---|
| `attention` | the agent is waiting on the person, with a question or a request |
| `done` | the agent finished what it was asked to do |
| `working` | the agent is working |
| `idle` | an agent is present and doing nothing |

decade shows the state with the question beside it, or the activity when
there is no question. A state holds until it is replaced, either by the
client or by the person at the screen, who can return `done` and
`attention` to `idle`. decade also returns a `done` agent to `idle` 10
minutes after a viewer first shows its workspace. Another `agent` message
from the agent stops that clock. `attention` never times out. Focusing the
pane changes nothing. A workspace
shows the most urgent state of its agents, in the order of the table.
When a pane enters `attention` and no focused viewer shows its workspace,
decade raises a desktop notification. Its text is the question, or else
the activity.

decade keeps each workspace's agents by session, in memory only. When a
client's connection drops, decade keeps its agent with everything it
reported and marks it disconnected. A later hello in the same workspace
that names the same session attaches to that agent again. The agent keeps
its report and its last 32 state changes. It moves to the pane the new
hello claims. decade checks once a minute and drops an agent that has
been disconnected for 10 minutes or more. A hello that names a new
session in a pane where a disconnected agent was left replaces that
agent. When a pane goes, its agent stays without a pane. When a second
client names a session another client holds, the session moves to the
second client.

A client sends `{"ended": {}}` to end the session. decade drops the agent
and everything it reported at once. decade then refuses `agent` messages
from the client until a new hello names an agent.

Neither message gets a reply. decade refuses both from a client whose
hello named no agent. decade also refuses `agent` from a client that
claimed no pane.

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
`?workspace=<id>`, and a tool call reaches only the panes of that workspace.
A program decade starts in a pane gets an address ending in
`?workspace=<id>&pane=<name>` instead. In the `panes` reply to a call on
that address, `you` marks that pane.
Its tools are `panes`, `agents`, `point`, `show`, `highlight`, `clear`,
`screenshot`, `scratch.open` and `scratch.send`. decade delivers a pane verb from a tool
call with `"from": "agent"`. decade answers `panes` with the pane list, in the shape
of the socket's `panes` reply, `agents` with the agent list below, `screenshot` with an image, and
every other call with a line saying what it did. When decade cannot act on a call, the result is a tool error whose
text is the reason the socket would give in `error`. decade refuses a call
to an address without `?workspace=` as invalid parameters. coax passes
`DECADE_MCP` to the agent in the ACP session's server list, so any agent
coax runs sees these tools.

`show`, `highlight` and `clear` take the payload of the socket verb of the
same name as their arguments, plus an optional `pane`. With `pane`, decade
sends the verb to that pane, whose occupant must show the kind. Without it,
decade sends the verb to the one pane whose occupant shows the kind. decade
refuses the call when several panes show the kind and when none does. In
the second case the error lists the kinds the workspace's panes show.

`agents` takes no arguments. decade answers with the workspace's agents in
the order they first said hello:

```json
{"agents": [{"name": "coax", "pane": "chat", "state": "attention", "age": 90, "question": "which branch?", "activity": null, "model": "opus", "context": {"used": 81000, "limit": 200000}, "tokens": {"input": 120000, "output": 9000}, "cost": 1.42, "connected": false}]}
```

`age` is the number of seconds the agent has held its state. The other
fields are the ones the agent last reported, null when unset, and
`connected` says whether a client holds its session.

`point` takes `pane` and an optional `note`. decade pulses the pane's
outline in magenta and puts the note on its top edge. Keyboard focus stays
where it is. When the pane is in a tab that is not shown, decade pulses
that tab's pill on the top bar. When the workspace is not shown, decade
pulses its card or tile in the sidebar. The point ends when the person
focuses the pane, when the pane goes, after 30 seconds, or when the agent
points again. A workspace has one point at a time.

`screenshot` takes `pane`. decade renders the workspace and answers with an
image result holding a PNG of the pane's client area. The PNG holds
neither the pane's bar and outline nor decade's menus and Ctrl bar. Its
pixels are the screen's. decade refuses the call when no pane has that name and when
the pane is in a tab that is not shown. The workspace itself need not be
shown.

Each pane has an accept screenshots mode, which the person turns on and
off from the pane's Ctrl bar or its right click menu. The mode starts off.
decade never saves it. A restored workspace has it off on every pane. With
the mode on, decade captures the pane at once. With it off, decade draws a
prompt over the pane that names the agent. decade also points at the pane,
as `point` does. The prompt offers allow once, allow and turn the mode on,
and deny. The person answers with a click, or with y, a or n while the
pane has focus. Escape denies. A deny, or no answer within 30
seconds, ends the call with a tool error that gives the reason. decade
refuses a second `screenshot` of a pane whose prompt is still up. While
the mode is on, the pane carries a camera tag at the left end of its
outline's top edge. The tag flashes at each capture.

The person may mark one pane in a workspace as its scratch pane, from the
Ctrl bar. Two tools reach it. `scratch.open` takes `program`, optional
`args`, optional `terminal` and optional `wait`. `terminal` defaults to
true. `program` is a
name that decade looks up on its `PATH`, or, when it holds a `/`, a path to
an executable file, relative to the workspace's directory. decade refuses
the call unless the workspace has a scratch pane and `program` names an
executable file. With `terminal`
true the program runs inside ghostty in the scratch pane. With `terminal`
false decade starts it directly. decade puts the first window of that
program, or of a process under it, in the scratch pane. Either way the
program gets the pane's name in `DECADE_PANE`. When a program already
occupies the scratch pane, decade asks it to close. decade starts the new
program once the old one has gone.

With `terminal` true, decade answers that the terminal started. With
`terminal` false, decade answers once the program's window lands in the
scratch pane, the program exits, or `wait` seconds pass. `wait` defaults
to 5. The clock starts when the program starts. While an old program
closes, the same wait limits how long decade waits for it to go. The
answer says which of these happened. After an exit, the answer gives the
exit status and the last 20 lines the program wrote to stderr. decade
also copies that stderr to its own log. A program still running when the
wait ends keeps running. decade puts its window in the scratch pane when
the window opens. When a second `scratch.open` arrives while the first
still waits, decade answers the first with what it knows so far.

`scratch.send` takes `verb` and
`payload`. decade sends `{"to": "<scratch pane>", "<verb>": payload}` from
`agent`, and the program in the scratch pane receives it like any other
message. The tool's description names `panes` as where the agent finds the
verbs the program takes, under `accepts`.

## Version

`welcome` carries `version`, 1 today. The number moves when a message changes
shape or meaning. Adding a verb does not move it.
