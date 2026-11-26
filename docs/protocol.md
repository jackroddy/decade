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

A program sets one variable of its own. A program that starts a child with
`DECADE_INTO=<pane name>` in the child's environment asks decade to put the
first window of that child, or of any process under it, in that pane as a
new tab. The name may be a former name. When no pane in the workspace
answers to it, decade places the window beside the pane in `DECADE_PANE`
as it would without the variable.

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
{"hello": {"name": "coax", "pane": "chat", "agent": {"name": "coax", "version": "0.4.0", "session": "7f3a", "args": ["--agent", "claude"]}}}
```

`args` holds the flags the program was started with. It is optional and
defaults to empty. decade does not start a program again from the
`agent` block. An agent that should start again on its session sends a
`restore` block whose argv names the session, as described below.

A client without it is a plain pane program.

A client that decade should start again adds a `restore` block. `argv`
is the command that starts it. `terminal` says whether that command runs
in a terminal, and defaults to false. `cwd` is the directory to start it
in, and may be left out:

```json
{"hello": {"name": "decade-notes", "pane": "notes", "restore": {"argv": ["decade-notes", "--file", "todo.md"], "terminal": false}}}
```

decade saves the block with the pane the client holds. When decade
restores the workspace, or wakes it from sleep, it starts each saved
`argv` in `cwd`, or in the workspace's directory when `cwd` is absent or
is not a directory, with `DECADE_PANE` set to that pane.
A terminal command is typed into the shell of a ghostty in the pane,
which stays at its prompt when the command exits. That ghostty is the
terminal decade started and the program ran in. When the program ran in
no such terminal, it is the pane's own terminal, or a new terminal tab
when the pane has no terminal free. Any other command is started
directly. The program's hello then claims the pane. A block with an
empty `argv` counts as no block. decade starts nothing again for a client
that sent no block.

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
messages: `hello`, `agent`, `ended`, `panes`, `restore`, `screenshot`,
`record`, `record.stop`, `welcome`, `error` and `from`.
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
`scratch` is true for each of the workspace's scratch panes, and `you` is true for
the pane of the client that asked:

```json
{"panes": [{"name": "chat", "client": "coax", "shows": [], "accepts": [], "scratch": false, "you": true}, {"name": "terminal", "client": null, "shows": [], "accepts": [], "scratch": true, "you": false}, {"name": "code", "client": "decade-code", "shows": [{"kind": "file", "highlight": true}], "accepts": [{"name": "reload", "description": "Read the file from disk again."}], "scratch": false, "you": false}]}
```

`restore` replaces the client's restore block, in the form hello gives
it. A `restore` with an empty `argv` clears the block. decade holds the
block only while the client is connected. It gets no reply:

```json
{"restore": {"argv": ["decade-notes", "--file", "notes.md"], "terminal": false}}
```

`screenshot` asks for a PNG of a pane that the client will save. `pane`
names the pane. `file` is the path the client will write, which decade
shows to the person and does not open. The dialog shows the path on a line of
its own. decade cuts the start of a path too long for the dialog, so the
file name stays in view. `for` is optional, and names the
pane of the agent asking, normally the client's `DECADE_PANE`. decade
uses it only to name that agent in the dialog, and does not check it:

```json
{"screenshot": {"pane": "code", "file": "shot.png", "for": "chat"}}
```

decade captures the pane as for the MCP tool `screenshot`, described
under Agents: MCP, and answers with the PNG in standard base64 with
padding:

```json
{"screenshot": {"png": "iVBORw0KGgo..."}}
```

decade answers with an error in the socket's `error` form whenever it
sends no PNG. These are the reasons it gives, with the pane's current
name for `<pane>`:

| when | `reason` |
|---|---|
| the client has not said hello | `say hello first` |
| `pane` or `file` is missing or not a string | `malformed screenshot: <what is wrong>` |
| no pane answers to `pane` | `no pane named <pane>` |
| the pane is in a tab that is not shown | `pane <pane> is in tab <n>, which is not shown` |
| the dialog is up for another capture | `decade is already asking the person about another capture` |
| the person denies | `the person refused a screenshot of pane <pane>` |
| no answer within 30 seconds | `the person did not answer within 30 seconds, so decade took no screenshot of pane <pane>` |
| the workspace closed while decade asked | `workspace <id> is closed` |
| the workspace went to sleep while decade asked or captured | `workspace <name> went to sleep` |
| the pane is off the screen | `pane <pane> has no room to draw` |
| the capture failed | `making a buffer to capture into failed: <error>`, `binding the capture failed: <error>`, `reading the capture failed: <error>`, `mapping the capture failed: <error>`, or the renderer's own text |

`<id>` is the workspace's number. decade checks the pane again when it
captures, so `no pane named` and `is in tab` can also come after the
person allows the screenshot. When the client disconnects while the
dialog asks for it, decade closes the dialog, turns on no mode and sends
nothing.

decade checks the pane's accept captures mode for a `screenshot`
message, as it does for the MCP tool. With the mode off, decade asks in
the same dialog, with the same rules. Its question is then "coax asks to save a screenshot of
pane code to shot.png".

`record` asks decade to record what a pane draws into a new mp4 or GIF
file. `pane`, `file` and `for` work as for `screenshot`, except that
decade writes `file` itself, so it must be an absolute path. The other
fields are optional:

```json
{"record": {"pane": "code", "file": "/work/demo.gif", "for": "chat", "format": "gif", "fps": 15, "duration": 10, "width": 800, "pointer": true, "loop": true}}
```

| field | value |
|---|---|
| `format` | `mp4` or `gif`, which must match the file's extension; by default the extension's |
| `fps` | frames a second, 1 to 60; 30 for an mp4 and 15 for a GIF by default |
| `duration` | seconds to record; without it, decade records until a stop or the limit |
| `width` | the video's width in pixels, 2 to 7680, the height following the pane's aspect ratio; by default the pane's width |
| `quality` | an mp4's crf for ffmpeg, 0 to 51, lower for better quality; 23 by default |
| `pointer` | whether decade draws the pointer into the frames while its tip is over the pane's client area; true by default |
| `loop` | whether a GIF plays in a loop; true by default |

An mp4 runs for at most 5 minutes and a GIF for at most 1 minute.
decade cuts a longer `duration` to that limit and does not refuse it.
decade checks the pane's accept captures mode and asks in the same
dialog, as for `screenshot`. The dialog's question is "coax asks to
record pane code for 10 seconds to /work/demo.gif", or "until stopped,
up to 1 minute, to" without `duration`. A recording ends at its
duration, at its limit, at a `record.stop`, or when one of these
happens:

- the pane closes
- its tab stops being the workspace's shown tab
- no viewer shows its workspace any more, when a viewer showed it at
  the start
- the workspace sleeps or closes
- the client disconnects

A recording follows its pane through a rename. decade keeps what it
recorded in every case. It answers once ffmpeg has written the file,
with the file, the frames written, the frames dropped because ffmpeg
fell behind, the seconds recorded and what ended the recording:

```json
{"record": {"file": "/work/demo.gif", "frames": 150, "dropped": 0, "seconds": 10.0, "ended": "it ran for its duration"}}
```

A recording that ends before its first frame leaves no file and gets an
error.

decade renders the pane for each frame as it does for `screenshot`, with
neither the pane's bar and outline nor the dialog, menus and Ctrl bar.
It writes the frames to an `ffmpeg` on its `PATH`. ffmpeg stamps each
frame with the time it reads it and writes the video at a constant
`fps`, repeating a frame where one is missing. The video therefore runs
as long as the recording did, even when frames are dropped or late. A
frame is dropped while two wait for ffmpeg, and `dropped` counts only
those. A frame that fails to render is left out without a count, and
ffmpeg repeats the frame before it.

An mp4 is H.264 in yuv420p, which needs an even width and height.
Without `width`, decade cuts the last column or row off a pane whose
width or height is odd. With `width`, ffmpeg scales the frames to it.
ffmpeg makes a GIF's palette from all of its frames, so it holds them
all in memory until the recording ends. decade estimates that memory
as 90 MiB plus 4.5 bytes for each pixel of each frame the recording
can hold, at the GIF's size. The estimate was within 10% of ffmpeg 9's
peak memory at three frame sizes. decade refuses a GIF whose
estimate is more than half of `MemAvailable` in `/proc/meminfo`.

Each frame has the size the pane had at the start. When the pane grows,
decade keeps the top left of each frame at that size. When the pane
shrinks, decade fills the right and bottom with black. While the
recording runs, the pane's outline pulses red and carries a red tag at
the left of its top edge, after the camera tag when the mode is on. The
tag holds a blinking recording icon and the time the recording has
run, as `m:ss`. Neither shows in the frames.

decade answers `record` with an error in these cases:

| when | `reason` |
|---|---|
| the client has not said hello | `say hello first` |
| `pane` or `file` is missing, or a field has the wrong type | `malformed record: <what is wrong>` |
| `file` is not absolute | `<file> is not an absolute path` |
| the extension is neither `.mp4` nor `.gif` | `<file> does not end in .mp4 or .gif` |
| `format` and the extension disagree | `<file> does not end in .<format>, as format <format> needs` |
| `quality` for a GIF, or `loop` for an mp4 | `quality is for an mp4, not a gif`, `loop is for a gif, not an mp4` |
| a value out of range | `fps is 1 to 60, not <fps>`, `duration is a number of seconds above 0, not <duration>`, `width is 2 to 7680 pixels, not <width>`, `quality is 0 to 51, not <quality>` |
| no pane answers to `pane` | `no pane named <pane>` |
| the pane is in a tab that is not shown | `pane <pane> is in tab <n>, which is not shown` |
| the pane is off the screen | `pane <pane> has no room to draw` |
| the pane is being recorded | `pane <pane> is already being recorded` |
| decade's `PATH` has no `ffmpeg` | `ffmpeg is not on decade's PATH` |
| the file's directory is missing | `<dir> does not exist` |
| a file or a symlink is at the path | `<file> already exists` |
| the GIF's estimate is over half the memory available | `a GIF of <frames> frames at <width>x<height> needs about <n> MiB in ffmpeg, more than half of the <m> MiB available` |
| the dialog is up for another capture | `decade is already asking the person about another capture` |
| the person denies | `the person refused a recording of pane <pane>` |
| no answer within 30 seconds | `the person did not answer within 30 seconds, so decade took no recording of pane <pane>` |
| the workspace closed while decade asked | `workspace <id> is closed` |
| the workspace went to sleep while decade asked | `workspace <name> went to sleep` |
| ffmpeg could not start | `starting ffmpeg failed: <error>` |
| the recording ended before its first frame | `the recording of pane <pane> ended before its first frame, as <what ended it>, so decade wrote no file` |
| ffmpeg failed | `ffmpeg failed, <exit status>:` and the last 20 lines of its stderr |

decade checks the pane, ffmpeg, the file and the memory before it asks.
It checks the pane, ffmpeg and the file again when the person allows
the recording.

`record.stop` names the pane in `pane`, by its current name or a former
one, and ends its recording. decade sends the answer to the client that
sent `record`. A `record.stop` gets no reply of its own, unless no
recording of the pane runs:

```json
{"record.stop": {"pane": "code"}}
```

| when | `reason` |
|---|---|
| the pane is not being recorded | `pane <pane> is not being recorded` |

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
| `subagents` | the sub-agents and background tasks the agent started, a list |

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

Each entry of `subagents` has a `name`, an optional `activity` and a
`state` of `working`, `done` or `failed`:

```json
{"agent": {"subagents": [{"name": "tests", "activity": "running cargo test", "state": "working"}, {"name": "review", "state": "failed"}]}}
```

The agent sends the whole list each time it changes, and decade shows it
as sent. The agent decides when a finished entry leaves the list. An
empty list clears it, as `null` does. An agent lists only the sub-agents
it started itself. Work a sub-agent starts in turn goes in that
sub-agent's activity.

An agent whose state is `done` while one of its sub-agents is `working`
counts as `working`. Its card, its workspace's place in the sidebar and
the 10 minute clock all go by `working` until no sub-agent works. A
`failed` sub-agent does not raise the agent to `attention`. The card puts
the number of sub-agents before the activity, and the popover lists them
with a red mark on a failed one. An agent that needs the person reports
`attention` itself.

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
`screenshot`, `record`, `record.stop`, `record.status`, `scratch.open`, `scratch.send`, `scratch.status`, `scratch.rename` and `scratch.input`. decade delivers a pane verb from a tool
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
{"agents": [{"name": "coax", "pane": "chat", "state": "attention", "age": 90, "question": "which branch?", "activity": null, "model": "opus", "context": {"used": 81000, "limit": 200000}, "tokens": {"input": 120000, "output": 9000}, "cost": 1.42, "subagents": [], "connected": false}]}
```

`age` is the number of seconds the agent has held its state. The other
fields are the ones the agent last reported, null when unset, and
`state` is the state the agent reported. `subagents` is its last list,
empty when it sent none, and
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

Each pane has an accept captures mode, which the person turns on and
off from the pane's Ctrl bar or its right click menu. The mode covers
the MCP tools `screenshot` and `record` and the socket's `screenshot`
and `record` messages. It starts off.
decade never saves it. A restored workspace has it off on every pane. With
the mode on, decade captures the pane at once. With it off, decade opens a
dialog in the middle of the window and dims everything behind it. Every
viewer shows the dialog, on whatever workspace it has up. The dialog names
the agent, the workspace and the pane. decade also points at the pane, as
`point` does. The dialog offers allow once, allow and turn the mode on,
and deny. While it is up, every key and click goes to it. None reaches a
pane, decade's chrome or decade's keybinds. The person answers with a
click on a choice, or with y, a or n. Escape denies. Keys and clicks in
the dialog's first half second do nothing. The first answer from any
viewer counts. A deny, or no answer within 30 seconds, ends the call with
a tool error that gives the reason. decade refuses a capture that
would ask while the dialog is up. While
the mode is on, the pane carries a camera tag at the left end of its
outline's top edge. The tag flashes at each capture, whether the mode
was on or the person allowed the capture in the dialog.

`record` takes the fields of the socket's `record`, without `for`.
decade records as for the socket, with the same refusals. The call
returns as soon as the recording starts: at once when the pane accepts
captures, or else once the person allows it. Its answer gives the
video's size, its `fps`, the most frames the recording can hold before
its duration or limit, and for a GIF the memory estimate. An mp4 has no
estimate, as x264 holds a fixed number of frames however long the
recording runs.

`record.stop` and `record.status` take `pane`, by its current name or a
former one. `record.stop` ends the recording, waits for ffmpeg to write
the file, and answers with the file, the frames written, the frames
dropped, the seconds recorded and what ended the recording.
`record.status` answers with the time recorded and the frames so far
while the recording runs, and says so while ffmpeg is still writing the
file. Once ffmpeg has written it, both tools answer with that finished result, or with its error. decade
keeps each pane's last finished result until the pane's next recording
starts. A `record.status` for a pane with no recording fails with
`pane <pane> has not been recorded`.

The person may mark any number of panes in a workspace as scratch panes,
from the Ctrl bar. Marking one leaves the others marked.
`scratch.open`, `scratch.send`, `scratch.status`, `scratch.rename` and `scratch.input` reach them, and every
agent in the workspace reaches every scratch pane. Each tool takes an
optional `pane`, the name of a scratch pane, which may be a name the pane
had before a rename. `pane` may be left out when the workspace has exactly
one scratch pane. decade refuses a call that leaves it out while there are
several, or that names a pane that is not a scratch pane. The error lists
the workspace's scratch panes. Each scratch pane keeps its own record of
the program last started in it and of the call waiting on it.

`scratch.open` takes `program`, optional
`args`, optional `terminal` and optional `wait`. `terminal` defaults to
true. `program` is a
name that decade looks up on its `PATH`, or, when it holds a `/`, a path to
an executable file, relative to the workspace's directory. decade refuses
the call unless `program` names an executable file. With `terminal`
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
the window opens. When a second `scratch.open` for the same scratch pane arrives while the first
still waits, decade answers the first with what it knows so far.

`scratch.send` takes `verb` and
`payload`. decade sends `{"to": "<scratch pane>", "<verb>": payload}` from
`agent`, and the program in the scratch pane receives it like any other
message. The tool's description names `panes` as where the agent finds the
verbs the program takes, under `accepts`.

`scratch.status` answers whether the program `scratch.open` last started
in the scratch pane still runs. Once it has exited, the answer gives its
exit status and the last 20 lines of its stderr.

`scratch.rename` takes `name` and renames the scratch pane `pane` names, as
the person can from the chrome. The old name stays an alias. decade refuses
a name that another pane answers to, or that holds `&`, `=` or a space, and
the result is a tool error giving the reason. No tool renames a pane that is
not a scratch pane.


`scratch.input` plays a script of pointer and key input in a scratch pane,
so that an agent can drive a program there and record it. It takes an
optional `pane`, `steps` and an optional `cell`:

```json
{"cell": {"width": 10, "height": 21, "left": 2, "top": 2}, "steps": [
  {"do": "type", "text": "echo blah", "ms": 60},
  {"do": "key", "key": "Return"},
  {"do": "move", "to": {"col": 2, "row": 4}, "ms": 500},
  {"do": "click", "double": true},
  {"do": "wait", "ms": 700},
  {"do": "drag", "from": {"x": 2, "y": 128}, "to": {"col": 30, "row": 7}, "ms": 900}
]}
```

| step | fields | what decade does |
|---|---|---|
| `move` | `to`, optional `ms` | glides the pointer to `to` over `ms`, 400 by default |
| `click` | optional `at`, `button`, `double`, `ms` | glides to `at` over `ms` when given, then presses and lets go of `button`, twice for `double` |
| `press` | `button` or `key` | presses the button or the one key and holds it |
| `release` | `button` or `key` | lets go of the button or key |
| `drag` | optional `from`, `to`, optional `button`, `ms` | glides to `from` when given, presses `button`, glides to `to` over `ms`, 600 by default, and lets go |
| `key` | `key` | presses a key or chord and lets go |
| `type` | `text`, optional `ms` | types each character, one every `ms`, 60 by default |
| `wait` | `ms` | waits |

`button` is `left`, `right` or `middle`. It is `left` by default. A
`key` is a key's name, such as `Return`, `Escape`, `Tab`, `BackSpace`,
`Up` or `a`. It may also be a chord, such as `ctrl+c` or `ctrl+shift+c`.
The names are the ones `decade script` takes. `type` takes the
characters of a US keyboard. A newline in its text types Return and a
tab types Tab. During a glide, decade moves the pointer about 60 times a
second, slowly at the start and the end. With `ms` 0 the pointer jumps.

A point is either `{"x": .., "y": ..}` or `{"col": .., "row": ..}`.
`x` and `y` are pixels from the top left of the pane's client area. They
are the pixels of a `screenshot` of the pane. `col` and `row` name a
terminal cell, counted from 0. decade aims at the cell's centre. The
cell grid depends on the terminal's font and padding, and decade does
not know it. A cell point therefore needs `cell` in the call. `width`
and `height` give a cell's size in pixels. `left` and `top` give where
column 0 and row 0 start, and are 0 by default. Before the first step,
decade puts the script's pointer where the person's pointer is, moved
inside the pane if it is outside.

decade plays the input through its own input path, as `wl_pointer` and
`wl_keyboard` events to the pane's program. None of it reaches decade's
chrome or keybinds. A chord that is a decade keybind, such as `super+b`,
goes to the program and decade's binding does not run. `record` draws
the pointer the script moves, so a recording shows each move and click.

Each pane has an accept input mode of its own, apart from accept
captures. The person turns it on and off from a scratch pane's Ctrl bar
or its right click menu. It starts off. decade never saves it. A
restored workspace has it off on every pane. Unmarking a scratch pane
turns it off. With the mode on, decade plays the script at once. With it
off, decade asks in the dialog `screenshot` uses. Its question is "coax
asks to control pane scratch". The line under it reads "Escape stops it
· 12 steps over 4.5 seconds". The warning comes first, as a narrow
dialog cuts the end of the line. The choices are allow once, allow and accept input, and
deny. The keys y, a and n answer them. While the mode is on, the pane
carries a mouse tag beside its scratch tag.

While a script plays, decade holds the person's pointer and keyboard as
the dialog does. None of their keys, clicks or scrolls reach a pane or
decade's chrome. Their Escape stops the script. As the script starts,
decade lets go of the keys the person holds, so the program sees none of
them held. decade then moves keyboard focus to the scratch pane. The pane's outline pulses yellow. A yellow
tag on its top edge names the agent and says that Escape stops the
script. decade draws the script's pointer over the pane for the person
to see. A screenshot or recording shows neither the tag nor that drawn
pointer. A recording shows only the pointer `record` draws. When the
script ends, decade lets go of every button and key the script still
holds. Keyboard focus goes back to the pane that had it. The pointer
goes back to where the person's pointer is. decade presses again the
Ctrl, Shift, Alt and Super keys the person still holds. It applies the
Caps Lock and Num Lock presses the person made while the script played.

A script holds at most 500 steps and plays for at most 5 minutes. A
glide takes at most 10 seconds and a wait at most 60. The call returns
when the script ends, with the steps played, the steps in the script,
the seconds it ran and why it ended: "played 5 of 5 steps in pane b over
4.1 s; it finished". A script that stops early ends the call with a
tool error in the same form, for one of these reasons. The answer gives
the pane's name at the end, which a rename during the script changes.

- the person pressed Escape
- the window the script played in left the pane, because it closed or
  another of the pane's tabs took its place
- the pane closed
- the pane's tab stopped being shown
- the workspace left the screen, went to sleep or closed

decade refuses arguments it cannot read, such as an unknown step or
field, as invalid parameters. It refuses a script it can read before it
asks, or once the person answers, with a tool error:

| when | reason |
|---|---|
| `pane` names a pane that is not a scratch pane | `<pane> is not a scratch pane; the scratch panes are: <names>` |
| the workspace has no scratch pane, or several and `pane` is left out | as for the other scratch tools |
| the pane is in a tab that is not shown | `pane <pane> is in tab <n>, which is not shown` |
| the active viewer does not show the workspace | `workspace <name> is not on screen, so the person could not watch or stop the input` |
| the pane holds no window | `pane <pane> has no window to take the input` |
| a script already plays | `an agent's input is already playing in pane <pane>` |
| the dialog is up for something else | `decade is already asking the person about another capture` |
| a step is wrong | `step <n>: <what is wrong>`, such as `step 2: no key types 'é'` or `step 1: a point by col and row needs cell in the call` |
| the script is too long | `the script has <n> steps, more than the 500 decade plays`, or `the script runs past step <n> for longer than 5 minutes` |
| the person denies | `the person refused input to pane <pane>` |
| no answer within 30 seconds | `the person did not answer within 30 seconds, so decade played no input in pane <pane>` |

While a script plays, decade refuses a `screenshot` or `record` that
would ask the person, with `an agent's input is playing in pane <pane>,
and decade asks the person nothing until it ends`. One with accept
captures on goes ahead.

## Version

`welcome` carries `version`, 1 today. The number moves when a message changes
shape or meaning. Adding a verb does not move it.
