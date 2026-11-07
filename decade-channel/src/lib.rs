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
    Verb {
        from: String,
        verb: String,
        payload: Value,
    },
    Closed,
}

/// What the agent in this client's pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Attention,
    Done,
    Working,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub name: String,
    pub client: Option<String>,
}

pub mod code {
    use std::path::PathBuf;

    use serde::{Deserialize, Serialize};

    pub const SHOW: &str = "show";
    pub const MARK: &str = "mark";
    pub const CLEAR: &str = "clear";

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Show {
        pub path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub line: Option<u32>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Mark {
        pub path: PathBuf,
        pub lines: Vec<(u32, u32)>,
    }
}

pub fn parse(line: &str) -> Result<Message, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let Value::Object(mut object) = value else {
        return Err("not an object".to_string());
    };
    if let Some(welcome) = object.remove("welcome") {
        let pane = welcome["pane"].as_str().map(str::to_string);
        let version = welcome["version"].as_u64().unwrap_or(0) as u32;
        return Ok(Message::Welcome { pane, version });
    }
    if let Some(error) = object.remove("error") {
        let reason = error["reason"].as_str().unwrap_or("").to_string();
        let message = error.get("message").cloned().unwrap_or(Value::Null);
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
    match (rest.next(), rest.next()) {
        (Some((verb, payload)), None) => Ok(Message::Verb {
            from,
            verb,
            payload,
        }),
        _ => Err("expected exactly one verb".to_string()),
    }
}

pub struct Channel {
    writer: UnixStream,
}

impl Channel {
    pub fn from_env(
        name: &str,
        accepts: &[&str],
    ) -> Option<io::Result<(Channel, Receiver<Message>)>> {
        let path = std::env::var_os("DECADE_SOCKET")?;
        let pane = std::env::var("DECADE_PANE").ok();
        Some(Channel::connect(
            Path::new(&path),
            name,
            accepts,
            pane.as_deref(),
        ))
    }

    pub fn connect(
        path: &Path,
        name: &str,
        accepts: &[&str],
        pane: Option<&str>,
    ) -> io::Result<(Channel, Receiver<Message>)> {
        let stream = UnixStream::connect(path)?;
        let reader = stream.try_clone()?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || read_lines(reader, tx));
        let mut channel = Channel { writer: stream };
        let mut hello = Map::new();
        hello.insert("name".into(), Value::String(name.to_string()));
        hello.insert(
            "accepts".into(),
            Value::Array(
                accepts
                    .iter()
                    .map(|a| Value::String(a.to_string()))
                    .collect(),
            ),
        );
        hello.insert(
            "pane".into(),
            pane.map(|p| Value::String(p.to_string()))
                .unwrap_or(Value::Null),
        );
        channel.write(object("hello", Value::Object(hello)))?;
        Ok((channel, rx))
    }

    pub fn send(&mut self, to: &str, verb: &str, payload: impl Serialize) -> io::Result<()> {
        let payload = serde_json::to_value(payload).map_err(io::Error::other)?;
        let mut message = Map::new();
        message.insert("to".into(), Value::String(to.to_string()));
        message.insert(verb.to_string(), payload);
        self.write(Value::Object(message))
    }

    pub fn focus(&mut self, pane: &str) -> io::Result<()> {
        let mut focus = Map::new();
        focus.insert("pane".into(), Value::String(pane.to_string()));
        self.write(object("focus", Value::Object(focus)))
    }

    /// Report the state of the agent in the pane this client claimed.
    pub fn status(&mut self, state: AgentState, message: Option<&str>) -> io::Result<()> {
        let mut status = Map::new();
        status.insert(
            "state".into(),
            serde_json::to_value(state).map_err(io::Error::other)?,
        );
        if let Some(message) = message {
            status.insert("message".into(), Value::String(message.to_string()));
        }
        self.write(object("status", Value::Object(status)))
    }

    pub fn panes(&mut self) -> io::Result<()> {
        self.write(object("panes", Value::Object(Map::new())))
    }

    fn write(&mut self, value: Value) -> io::Result<()> {
        let mut line = value.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())
    }
}

fn object(key: &str, value: Value) -> Value {
    let mut map = Map::new();
    map.insert(key.to_string(), value);
    Value::Object(map)
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
        let message = parse(r#"{"from":"coax","show":{"path":"src/gpu.rs","line":40}}"#).unwrap();
        let Message::Verb {
            from,
            verb,
            payload,
        } = message
        else {
            panic!("not a verb");
        };
        assert_eq!(from, "coax");
        assert_eq!(verb, "show");
        let show: code::Show = serde_json::from_value(payload).unwrap();
        assert_eq!(show.line, Some(40));
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
            parse(r#"{"panes":[{"name":"chat","client":"coax"},{"name":"code","client":null}]}"#)
                .unwrap();
        assert_eq!(
            message,
            Message::Panes(vec![
                Pane {
                    name: "chat".into(),
                    client: Some("coax".into())
                },
                Pane {
                    name: "code".into(),
                    client: None
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
                .write_all(b"{\"from\":\"coax\",\"clear\":{}}\n")
                .unwrap();
            let mut sent = String::new();
            reader.read_line(&mut sent).unwrap();
            (hello, sent)
        });
        let (mut channel, rx) = Channel::connect(
            &path,
            "decade-code",
            &["show", "mark", "clear"],
            Some("code"),
        )
        .unwrap();
        channel
            .send(
                "chat",
                "show",
                code::Show {
                    path: "a.rs".into(),
                    line: Some(3),
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
        assert!(matches!(rx.recv().unwrap(), Message::Verb { verb, .. } if verb == "clear"));
        let (hello, sent) = server.join().unwrap();
        let hello: Value = serde_json::from_str(&hello).unwrap();
        assert_eq!(hello["hello"]["name"], "decade-code");
        assert_eq!(hello["hello"]["pane"], "code");
        assert_eq!(hello["hello"]["accepts"], json!(["show", "mark", "clear"]));
        let sent: Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(
            sent,
            json!({"to": "chat", "show": {"path": "a.rs", "line": 3}})
        );
        drop(channel);
        assert_eq!(rx.recv().unwrap(), Message::Closed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
