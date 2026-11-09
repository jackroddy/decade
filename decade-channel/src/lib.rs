use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Welcome {
        pane: Option<String>,
        version: u32,
    },
    Error {
        reason: String,
        message: Value,
    },
    Panes(Vec<Pane>),
    Show {
        from: String,
        show: Show,
    },
    Highlight {
        from: String,
        highlight: Highlight,
    },
    Clear {
        from: String,
        clear: Clear,
    },
    Verb {
        from: String,
        verb: String,
        payload: Value,
    },
    Closed,
}

/// What the agent in a pane is doing, least urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Idle,
    Working,
    Done,
    Attention,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub name: String,
    pub client: Option<String>,

    /// The kinds the client in the pane can show.
    #[serde(default)]
    pub shows: Vec<Shows>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub name: String,
    #[serde(default)]
    pub accepts: Vec<String>,
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub shows: Vec<Shows>,
}

/// A kind of thing a program can show, such as `file` or `url`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shows {
    pub kind: String,

    /// Whether the program takes `highlight` and `clear` for this kind.
    #[serde(default)]
    pub highlight: bool,
}

/// The payload of `show`: bring `target` into view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Show {
    pub kind: String,
    pub target: String,

    /// Where in the target, in the form the kind defines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Value>,
}

/// The payload of `highlight`: mark parts of `target`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    pub kind: String,
    pub target: String,

    /// The parts to mark, in the form the kind defines.
    pub ranges: Value,
}

/// The payload of `clear`: drop every highlight of the kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clear {
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub pane: Option<String>,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Error {
    pub reason: String,
    #[serde(default)]
    pub message: Value,
}

/// A line decade writes to a client, other than a forwarded verb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reply {
    Welcome(Welcome),
    Error(Error),
    Panes(Vec<Pane>),
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Request {
    Hello(Hello),
    Status(Status),
    Panes {},
}

pub fn parse(line: &str) -> Result<Message, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let Value::Object(mut object) = value else {
        return Err("not an object".to_string());
    };
    if let Some(welcome) = object.remove("welcome") {
        let Welcome { pane, version } =
            serde_json::from_value(welcome).map_err(|e| format!("bad welcome: {e}"))?;
        return Ok(Message::Welcome { pane, version });
    }
    if let Some(error) = object.remove("error") {
        let Error { reason, message } =
            serde_json::from_value(error).map_err(|e| format!("bad error: {e}"))?;
        return Ok(Message::Error { reason, message });
    }
    if let Some(panes) = object.remove("panes") {
        let panes = serde_json::from_value(panes).map_err(|e| format!("bad panes: {e}"))?;
        return Ok(Message::Panes(panes));
    }
    let from = object
        .remove("from")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut rest = object.into_iter();
    let (Some((verb, payload)), None) = (rest.next(), rest.next()) else {
        return Err("expected exactly one verb".to_string());
    };
    let bad = |e: serde_json::Error| format!("bad {verb}: {e}");
    match verb.as_str() {
        "show" => serde_json::from_value(payload)
            .map(|show| Message::Show { from, show })
            .map_err(bad),
        "highlight" => serde_json::from_value(payload)
            .map(|highlight| Message::Highlight { from, highlight })
            .map_err(bad),
        "clear" => serde_json::from_value(payload)
            .map(|clear| Message::Clear { from, clear })
            .map_err(bad),
        _ => Ok(Message::Verb {
            from,
            verb,
            payload,
        }),
    }
}

pub struct Channel {
    writer: UnixStream,
}

impl Channel {
    pub fn from_env(
        name: &str,
        accepts: &[&str],
        shows: &[Shows],
    ) -> Option<io::Result<(Channel, Receiver<Message>)>> {
        let path = std::env::var_os("DECADE_SOCKET")?;
        let pane = std::env::var("DECADE_PANE").ok();
        Some(Channel::connect(
            Path::new(&path),
            name,
            accepts,
            shows,
            pane.as_deref(),
        ))
    }

    pub fn connect(
        path: &Path,
        name: &str,
        accepts: &[&str],
        shows: &[Shows],
        pane: Option<&str>,
    ) -> io::Result<(Channel, Receiver<Message>)> {
        let stream = UnixStream::connect(path)?;
        let reader = stream.try_clone()?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || read_lines(reader, tx));
        let mut channel = Channel { writer: stream };
        channel.write(&Request::Hello(Hello {
            name: name.to_string(),
            accepts: accepts.iter().map(|a| a.to_string()).collect(),
            pane: pane.map(str::to_string),
            shows: shows.to_vec(),
        }))?;
        Ok((channel, rx))
    }

    pub fn send(&mut self, to: &str, verb: &str, payload: impl Serialize) -> io::Result<()> {
        let payload = serde_json::to_value(payload).map_err(io::Error::other)?;
        let mut message = Map::new();
        message.insert("to".into(), Value::String(to.to_string()));
        message.insert(verb.to_string(), payload);
        self.write(&message)
    }

    /// Report the state of the agent in the pane this client claimed.
    pub fn status(&mut self, state: AgentState, message: Option<&str>) -> io::Result<()> {
        self.write(&Request::Status(Status {
            state,
            message: message.map(str::to_string),
        }))
    }

    pub fn panes(&mut self) -> io::Result<()> {
        self.write(&Request::Panes {})
    }

    fn write(&mut self, message: &impl Serialize) -> io::Result<()> {
        let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())
    }
}

fn read_lines(stream: UnixStream, tx: Sender<Message>) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let message = parse(&line).unwrap_or_else(|reason| Message::Error {
            reason,
            message: Value::String(line),
        });
        if tx.send(message).is_err() {
            return;
        }
    }
    let _ = tx.send(Message::Closed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_welcome_line_parses_with_its_pane_and_version() {
        let message = parse(r#"{"welcome":{"pane":"code","version":1}}"#).unwrap();
        assert_eq!(
            message,
            Message::Welcome {
                pane: Some("code".into()),
                version: 1
            }
        );
        let message = parse(r#"{"welcome":{"pane":null,"version":1}}"#).unwrap();
        assert_eq!(
            message,
            Message::Welcome {
                pane: None,
                version: 1
            }
        );
    }

    #[test]
    fn a_forwarded_verb_parses_with_its_sender() {
        let message = parse(r#"{"from":"coax","focus":{"row":3}}"#).unwrap();
        assert_eq!(
            message,
            Message::Verb {
                from: "coax".into(),
                verb: "focus".into(),
                payload: json!({"row": 3})
            }
        );
    }

    #[test]
    fn show_highlight_and_clear_parse_into_their_payloads() {
        let message =
            parse(r#"{"from":"agent","show":{"kind":"file","target":"src/gpu.rs","at":40}}"#)
                .unwrap();
        assert_eq!(
            message,
            Message::Show {
                from: "agent".into(),
                show: Show {
                    kind: "file".into(),
                    target: "src/gpu.rs".into(),
                    at: Some(json!(40))
                }
            }
        );
        let message = parse(
            r#"{"from":"agent","highlight":{"kind":"file","target":"a.rs","ranges":[[10,14]]}}"#,
        )
        .unwrap();
        assert_eq!(
            message,
            Message::Highlight {
                from: "agent".into(),
                highlight: Highlight {
                    kind: "file".into(),
                    target: "a.rs".into(),
                    ranges: json!([[10, 14]])
                }
            }
        );
        let message = parse(r#"{"from":"agent","clear":{"kind":"file"}}"#).unwrap();
        assert_eq!(
            message,
            Message::Clear {
                from: "agent".into(),
                clear: Clear {
                    kind: "file".into()
                }
            }
        );
        let error = parse(r#"{"from":"agent","show":{"path":"a.rs"}}"#).unwrap_err();
        assert!(error.starts_with("bad show: missing field `kind`"));
    }

    #[test]
    fn a_show_without_at_is_written_without_it_and_reads_back_the_same() {
        let show = Show {
            kind: "url".into(),
            target: "https://example.org".into(),
            at: None,
        };
        let value = serde_json::to_value(&show).unwrap();
        assert_eq!(
            value,
            json!({"kind": "url", "target": "https://example.org"})
        );
        assert_eq!(serde_json::from_value::<Show>(value).unwrap(), show);
        let shows: Shows = serde_json::from_value(json!({"kind": "image"})).unwrap();
        assert!(!shows.highlight);
    }

    #[test]
    fn errors_and_pane_lists_parse_and_junk_is_refused() {
        let message =
            parse(r#"{"error":{"reason":"say hello first","message":{"to":"x"}}}"#).unwrap();
        assert_eq!(
            message,
            Message::Error {
                reason: "say hello first".into(),
                message: json!({"to": "x"})
            }
        );
        let message =
            parse(r#"{"panes":[{"name":"chat","client":"coax"},{"name":"code","client":"decade-code","shows":[{"kind":"file","highlight":true}]}]}"#)
                .unwrap();
        assert_eq!(
            message,
            Message::Panes(vec![
                Pane {
                    name: "chat".into(),
                    client: Some("coax".into()),
                    shows: Vec::new()
                },
                Pane {
                    name: "code".into(),
                    client: Some("decade-code".into()),
                    shows: vec![Shows {
                        kind: "file".into(),
                        highlight: true
                    }]
                }
            ])
        );
        assert!(parse("nope").is_err());
        assert!(parse(r#"{"a":1,"b":2}"#).is_err());
    }

    #[test]
    fn connect_sends_hello_and_receives_what_the_server_writes() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("decade-channel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("decade.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut hello = String::new();
            reader.read_line(&mut hello).unwrap();
            let mut writer = &stream;
            writer
                .write_all(b"{\"welcome\":{\"pane\":\"code\",\"version\":1}}\n")
                .unwrap();
            writer
                .write_all(b"{\"from\":\"agent\",\"clear\":{\"kind\":\"file\"}}\n")
                .unwrap();
            let mut sent = String::new();
            reader.read_line(&mut sent).unwrap();
            (hello, sent)
        });
        let (mut channel, rx) = Channel::connect(
            &path,
            "decade-code",
            &["reload"],
            &[Shows {
                kind: "file".into(),
                highlight: true,
            }],
            Some("code"),
        )
        .unwrap();
        channel
            .send(
                "chat",
                "show",
                Show {
                    kind: "file".into(),
                    target: "a.rs".into(),
                    at: Some(json!(3)),
                },
            )
            .unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            Message::Welcome {
                pane: Some("code".into()),
                version: 1
            }
        );
        assert!(matches!(rx.recv().unwrap(), Message::Clear { clear, .. } if clear.kind == "file"));
        let (hello, sent) = server.join().unwrap();
        let hello: Value = serde_json::from_str(&hello).unwrap();
        assert_eq!(hello["hello"]["name"], "decade-code");
        assert_eq!(hello["hello"]["pane"], "code");
        assert_eq!(hello["hello"]["accepts"], json!(["reload"]));
        assert_eq!(
            hello["hello"]["shows"],
            json!([{"kind": "file", "highlight": true}])
        );
        let sent: Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(
            sent,
            json!({"to": "chat", "show": {"kind": "file", "target": "a.rs", "at": 3}})
        );
        drop(channel);
        assert_eq!(rx.recv().unwrap(), Message::Closed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attention_is_the_most_urgent_state_and_idle_the_least() {
        let mut states = [
            AgentState::Done,
            AgentState::Idle,
            AgentState::Attention,
            AgentState::Working,
        ];
        states.sort();
        assert_eq!(
            states,
            [
                AgentState::Idle,
                AgentState::Working,
                AgentState::Done,
                AgentState::Attention
            ]
        );
    }

    #[test]
    fn requests_are_written_as_one_key_objects() {
        let line = |request: Request| serde_json::to_value(request).unwrap();
        assert_eq!(
            line(Request::Status(Status {
                state: AgentState::Working,
                message: None
            })),
            json!({"status": {"state": "working"}})
        );
        assert_eq!(line(Request::Panes {}), json!({"panes": {}}));
    }

    #[test]
    fn a_reply_parses_back_into_the_message_it_was_written_from() {
        let reply = Reply::Welcome(Welcome {
            pane: Some("code".into()),
            version: VERSION,
        });
        assert_eq!(
            parse(&serde_json::to_string(&reply).unwrap()).unwrap(),
            Message::Welcome {
                pane: Some("code".into()),
                version: VERSION
            }
        );
    }
}
