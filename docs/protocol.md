# The decade socket

decade listens on a unix socket while it runs. Programs it starts find the
path in `DECADE_SOCKET` and the name of the pane they were started in, if
any, in `DECADE_PANE`. Each direction carries one JSON object per line.

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

`focus` moves keyboard focus to the pane and has no reply. `panes` is
answered with every pane and its occupant:

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

## Version

`welcome` carries `version`, 1 today. The number moves when a message changes
shape or meaning. Adding a verb does not move it.
