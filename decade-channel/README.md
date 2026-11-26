# decade-channel

## about

decade-channel is the client side of
[decade](https://github.com/jackroddy/decade)'s socket. decade is a Wayland
compositor whose panes are other programs. A program running in a decade pane
can use decade-channel to:

- claim its pane, and say which verbs it accepts and which kinds of thing it
  shows
- send verbs to the programs in other panes, and receive the ones sent to it
- list the panes of its workspace
- report the state of a coding agent it runs
- tell decade how to start it again when the workspace is restored
- save a screenshot of a pane

The wire itself is specified in decade's
[`docs/protocol.md`](https://github.com/jackroddy/decade/blob/main/docs/protocol.md).

## installation

    cargo add decade-channel

## usage

Each example below is a file in this crate's `examples/`, which you can run
from a terminal in a decade pane. The output shown comes from a workspace with
two panes, named `a` and `b`.

### listing the panes

decade starts every program in a pane with `DECADE_SOCKET` and `DECADE_PANE`
in its environment. `Channel::from_env` connects to that socket under the name
you give it and claims the pane. It returns `None` when `DECADE_SOCKET` isn't
set, as in a program decade didn't start.

You get back a `Channel` to send with and a `Receiver` of the `Message`s decade
sends. This program asks decade for the panes and prints the answer:

```rust
use decade_channel::{Channel, Message};

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.panes()?;
    for message in messages {
        if let Message::Panes(panes) = message {
            for pane in panes {
                println!("{pane:?}");
            }
            break;
        }
    }
    Ok(())
}
```

Run in pane `a`, it prints:

```text
Pane { name: "a", client: Some("blah"), shows: [], accepts: [], scratch: false, you: true }
Pane { name: "b", client: None, shows: [], accepts: [], scratch: false, you: false }
```

### sending a verb to another pane

A verb is a name two programs agree on, with any payload that serializes to
JSON. `Channel::send` sends one to the program in another pane. decade
doesn't answer a message it delivers. When it can't deliver one, it answers
with a `Message::Error`:

```rust
use std::time::Duration;

use decade_channel::Channel;

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.send("b", "blah", 42)?;
    while let Ok(message) = messages.recv_timeout(Duration::from_secs(1)) {
        println!("{message:?}");
    }
    Ok(())
}
```

Nothing is running in pane `b` yet:

```text
Welcome { pane: Some("a"), version: 1 }
Error { reason: "no client occupies pane b", message: Object {"blah": Number(42), "to": String("b")} }
```

### receiving verbs

A program that lists no verbs and no kinds takes every verb. To say what yours
takes, pass a `Verb` for each one, with a sentence saying what it does. Agents
read that sentence in the pane list. decade refuses any verb you didn't list:

```rust
use decade_channel::{Channel, Verb};

fn main() -> std::io::Result<()> {
    let accepts = [Verb::new("blah", "Print the payload.")];
    let (_channel, messages) =
        Channel::from_env("blah", &accepts, &[], None).expect("DECADE_SOCKET is not set")?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
```

Run in pane `b`, it prints the verb that the previous example sends from pane
`a`, as a `Message::Verb`:

```text
Welcome { pane: Some("b"), version: 1 }
Verb { from: "blah", verb: "blah", payload: Number(42) }
```

### showing files

`show`, `highlight` and `clear` are standard verbs. They're how agents bring a
file into view, mark lines of it and drop the marks, through decade's MCP
tools of the same names. A program takes them for each kind of thing it lists
in `Shows`. They arrive as `Message::Show`, `Message::Highlight` and
`Message::Clear`:

```rust
use decade_channel::{Channel, Shows};

fn main() -> std::io::Result<()> {
    let shows = [Shows {
        kind: "file".to_string(),
        highlight: true,
    }];
    let (_channel, messages) =
        Channel::from_env("blah", &[], &shows, None).expect("DECADE_SOCKET is not set")?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
```

Run in pane `b`, it prints what an agent's `show`, `highlight` and `clear`
calls on `blah.txt` deliver:

```text
Welcome { pane: Some("b"), version: 1 }
Show { from: "agent", show: Show { kind: "file", target: "blah.txt", at: Some(Number(42)) } }
Highlight { from: "agent", highlight: Highlight { kind: "file", target: "blah.txt", ranges: Array [Array [Number(1), Number(2)]] } }
Clear { from: "agent", clear: Clear { kind: "file" } }
```

The kind defines what `at` and `ranges` hold. For `file`, `at` is a line and
`ranges` is a list of `[from, to]` line pairs.

### reporting an agent's state

A program that runs a coding agent passes an `Agent` when it connects, then
sends an `AgentUpdate` whenever the agent's state changes. decade shows the
state in its sidebar. In an `AgentUpdate`, `None` leaves a field as decade has
it and `Some(None)` clears it:

```rust
use decade_channel::{Agent, AgentState, AgentUpdate, Channel};

fn main() -> std::io::Result<()> {
    let agent = Agent {
        name: "blah".to_string(),
        version: "0.1.0".to_string(),
        session: "42".to_string(),
        args: Vec::new(),
    };
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], Some(&agent)).expect("DECADE_SOCKET is not set")?;
    channel.agent(&AgentUpdate {
        state: Some(Some(AgentState::Working)),
        activity: Some(Some("blah blah blah".to_string())),
        ..AgentUpdate::default()
    })?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
```

Other agents in the workspace see the report through decade's `agents` MCP
tool:

```json
{"agents":[{"activity":"blah blah blah","age":1,"connected":true,"context":null,"cost":null,"model":null,"name":"blah","pane":"b","question":null,"state":"working","subagents":[],"tokens":null}]}
```

### starting your program again

decade saves each workspace's panes and restores them when it starts or wakes
a workspace from sleep. It starts a program again only if the program told it
how. `Channel::restore` sends that as a `Restore`: the command, whether it
runs in a terminal and the directory to start it in. decade keeps it only
while the program is connected. An empty `argv` clears it:

```rust
use decade_channel::{Channel, Restore};

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.restore(&Restore {
        argv: std::env::args().collect(),
        terminal: true,
        cwd: std::env::current_dir().ok(),
    })?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
```

### saving a screenshot of a pane

`Channel::screenshot` asks decade for a PNG of a pane, which your program
writes to the file it names. decade asks the person at the screen first, in a
dialog that shows the file's path. The PNG arrives as `Message::Screenshot`, or
a refusal as `Message::Error`:

```rust
use decade_channel::{Channel, Message, Screenshot};

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.screenshot(&Screenshot {
        pane: "b".to_string(),
        file: "blah.png".to_string(),
        caller: None,
    })?;
    for message in messages {
        match message {
            Message::Screenshot { png } => {
                std::fs::write("blah.png", &png)?;
                println!("wrote {} bytes to blah.png", png.len());
                break;
            }
            Message::Error { reason, .. } => {
                println!("{reason}");
                break;
            }
            _ => {}
        }
    }
    Ok(())
}
```

Once the person allows it:

```text
wrote 1630520 bytes to blah.png
```

The rest of the API is documented on [docs.rs](https://docs.rs/decade-channel).

## license

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
