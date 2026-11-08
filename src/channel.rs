use std::collections::HashMap;

use decade_channel::{AgentState, Error, Focus, Hello, Pane, Reply, Status, VERSION, Welcome};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The client id under which MCP tool calls reach a router.
pub const AGENT: usize = usize::MAX;

#[derive(Deserialize)]
struct Envelope {
    hello: Option<Hello>,
    to: Option<String>,
    focus: Option<Focus>,
    status: Option<Status>,
    panes: Option<Value>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

pub struct Client {
    pub name: Option<String>,
    pub accepts: Vec<String>,
    pub pane: Option<String>,
}

pub enum Host {
    Focus(String),
    Status {
        client: usize,
        pane: String,
        state: AgentState,
        message: Option<String>,
    },
}

pub enum Out {
    Line(usize, String),
    Host(Host),
}

pub struct Router {
    pub panes: Vec<String>,
    pub clients: HashMap<usize, Client>,
}

impl Router {
    pub fn new(panes: Vec<String>) -> Router {
        Router {
            panes,
            clients: HashMap::new(),
        }
    }

    pub fn connected(&mut self, id: usize) {
        self.clients.insert(
            id,
            Client {
                name: None,
                accepts: Vec::new(),
                pane: None,
            },
        );
    }

    pub fn disconnected(&mut self, id: usize) {
        self.clients.remove(&id);
    }

    pub fn add_pane(&mut self, name: String) {
        if !self.panes.contains(&name) {
            self.panes.push(name);
        }
    }

    pub fn remove_pane(&mut self, name: &str) {
        self.panes.retain(|pane| pane != name);
        for client in self.clients.values_mut() {
            if client.pane.as_deref() == Some(name) {
                client.pane = None;
            }
        }
    }

    pub fn occupant(&self, pane: &str) -> Option<(usize, &Client)> {
        self.clients
            .iter()
            .find(|(_, client)| client.pane.as_deref() == Some(pane))
            .map(|(id, client)| (*id, client))
    }

    pub fn handle(&mut self, id: usize, line: &str) -> Vec<Out> {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => return vec![error_line(id, &format!("not JSON: {error}"), json!(line))],
        };
        let envelope: Envelope = match serde_json::from_value(value.clone()) {
            Ok(envelope) => envelope,
            Err(error) => return vec![error_line(id, &format!("malformed: {error}"), value)],
        };
        if let Some(hello) = envelope.hello {
            return self.hello(id, hello, value);
        }
        if self
            .clients
            .get(&id)
            .and_then(|c| c.name.as_ref())
            .is_none()
        {
            return vec![error_line(id, "say hello first", value)];
        }
        if let Some(focus) = envelope.focus {
            return if self.panes.contains(&focus.pane) {
                vec![Out::Host(Host::Focus(focus.pane))]
            } else {
                vec![error_line(
                    id,
                    &format!("no pane named {}", focus.pane),
                    value,
                )]
            };
        }
        if let Some(status) = envelope.status {
            let Some(pane) = self.clients[&id].pane.clone() else {
                return vec![error_line(
                    id,
                    "status is for the agent in a pane, and this client claimed none",
                    value,
                )];
            };
            return vec![Out::Host(Host::Status {
                client: id,
                pane,
                state: status.state,
                message: status.message,
            })];
        }
        if envelope.panes.is_some() {
            let panes = self
                .panes
                .iter()
                .map(|name| Pane {
                    name: name.clone(),
                    client: self.occupant(name).and_then(|(_, c)| c.name.clone()),
                })
                .collect();
            return vec![reply(id, Reply::Panes(panes))];
        }
        let Some(to) = envelope.to else {
            return vec![error_line(
                id,
                "a message needs \"to\", \"focus\", \"status\", \"panes\" or \"hello\"",
                value,
            )];
        };
        if !self.panes.contains(&to) {
            return vec![error_line(id, &format!("no pane named {to}"), value)];
        }
        let Some((target, client)) = self.occupant(&to) else {
            return vec![error_line(
                id,
                &format!("no client occupies pane {to}"),
                value,
            )];
        };
        let mut keys = envelope.rest.keys();
        let (Some(verb), None) = (keys.next(), keys.next()) else {
            return vec![error_line(id, "a message carries exactly one verb", value)];
        };
        if !client.accepts.is_empty() && !client.accepts.iter().any(|a| a == verb) {
            return vec![error_line(
                id,
                &format!("pane {to} does not accept {verb}"),
                value,
            )];
        }
        let from = self.clients[&id].name.clone().unwrap_or_default();
        let mut forwarded = envelope.rest;
        forwarded.insert("from".to_string(), Value::String(from));
        vec![Out::Line(target, Value::Object(forwarded).to_string())]
    }

    fn hello(&mut self, id: usize, hello: Hello, value: Value) -> Vec<Out> {
        let mut out = Vec::new();
        let mut pane = None;
        if let Some(wanted) = &hello.pane {
            if !self.panes.contains(wanted) {
                out.push(error_line(
                    id,
                    &format!("no pane named {wanted}"),
                    value.clone(),
                ));
            } else {
                match self.occupant(wanted) {
                    Some((other, occupant)) if other != id => out.push(error_line(
                        id,
                        &format!(
                            "pane {wanted} is occupied by {}",
                            occupant.name.as_deref().unwrap_or("another client")
                        ),
                        value.clone(),
                    )),
                    _ => pane = Some(wanted.clone()),
                }
            }
        }
        let client = self.clients.entry(id).or_insert(Client {
            name: None,
            accepts: Vec::new(),
            pane: None,
        });
        client.name = Some(hello.name);
        client.accepts = hello.accepts;
        client.pane = pane.clone();
        out.push(reply(
            id,
            Reply::Welcome(Welcome {
                pane,
                version: VERSION,
            }),
        ));
        out
    }
}

fn error_line(id: usize, reason: &str, message: Value) -> Out {
    reply(
        id,
        Reply::Error(Error {
            reason: reason.to_string(),
            message,
        }),
    )
}

fn reply(id: usize, reply: Reply) -> Out {
    Out::Line(id, serde_json::to_string(&reply).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::new(vec!["chat".into(), "terminal".into(), "code".into()])
    }

    fn lines(out: Vec<Out>) -> Vec<(usize, Value)> {
        out.into_iter()
            .map(|o| match o {
                Out::Line(id, s) => (id, serde_json::from_str(&s).unwrap()),
                Out::Host(_) => panic!("host command where a line was expected"),
            })
            .collect()
    }

    #[test]
    fn hello_claims_the_pane_named_in_it_and_is_welcomed() {
        let mut r = router();
        r.connected(1);
        let out = lines(r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show"], "pane": "code"}}"#,
        ));
        assert_eq!(
            out,
            vec![(1, json!({"welcome": {"pane": "code", "version": 1}}))]
        );
        assert_eq!(
            r.occupant("code").unwrap().1.name.as_deref(),
            Some("code-pane")
        );
    }

    #[test]
    fn a_second_client_cannot_take_an_occupied_pane() {
        let mut r = router();
        r.connected(1);
        r.connected(2);
        r.handle(1, r#"{"hello": {"name": "first", "pane": "code"}}"#);
        let out = lines(r.handle(2, r#"{"hello": {"name": "second", "pane": "code"}}"#));
        assert_eq!(out.len(), 2);
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("occupied by first")
        );
        assert_eq!(out[1].1["welcome"]["pane"], Value::Null);
    }

    #[test]
    fn a_message_is_forwarded_to_the_pane_occupant_with_the_sender_named() {
        let mut r = router();
        r.connected(1);
        r.connected(2);
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show", "mark"], "pane": "code"}}"#,
        );
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#);
        let out = lines(r.handle(
            2,
            r#"{"to": "code", "show": {"path": "src/gpu.rs", "line": 40}}"#,
        ));
        assert_eq!(
            out,
            vec![(
                1,
                json!({"show": {"path": "src/gpu.rs", "line": 40}, "from": "coax"})
            )]
        );
    }

    #[test]
    fn a_verb_the_occupant_does_not_accept_is_refused() {
        let mut r = router();
        r.connected(1);
        r.connected(2);
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show"], "pane": "code"}}"#,
        );
        r.handle(2, r#"{"hello": {"name": "coax"}}"#);
        let out = lines(r.handle(2, r#"{"to": "code", "draw": {}}"#));
        assert_eq!(
            out[0].1["error"]["reason"],
            "pane code does not accept draw"
        );
    }

    #[test]
    fn messages_before_hello_and_bad_json_get_errors_back() {
        let mut r = router();
        r.connected(1);
        let out = lines(r.handle(1, r#"{"to": "code", "show": {}}"#));
        assert_eq!(out[0].1["error"]["reason"], "say hello first");
        let out = lines(r.handle(1, "not json"));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("not JSON")
        );
        assert_eq!(out[0].1["error"]["message"], "not json");
    }

    #[test]
    fn focus_becomes_a_host_command_and_panes_lists_occupants() {
        let mut r = router();
        r.connected(1);
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#);
        let out = r.handle(1, r#"{"focus": {"pane": "terminal"}}"#);
        assert!(matches!(&out[0], Out::Host(Host::Focus(pane)) if pane == "terminal"));
        let out = lines(r.handle(1, r#"{"panes": {}}"#));
        assert_eq!(
            out[0].1,
            json!({"panes": [
                {"name": "chat", "client": "coax"},
                {"name": "terminal", "client": null},
                {"name": "code", "client": null}
            ]})
        );
    }

    #[test]
    fn a_disconnected_client_frees_its_pane() {
        let mut r = router();
        r.connected(1);
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#);
        r.disconnected(1);
        assert!(r.occupant("chat").is_none());
    }

    #[test]
    fn a_status_applies_to_the_pane_its_client_claimed() {
        let mut r = router();
        r.connected(1);
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#);
        let out = r.handle(
            1,
            r#"{"status": {"state": "attention", "message": "a question"}}"#,
        );
        assert!(matches!(
            &out[..],
            [Out::Host(Host::Status { client: 1, pane, state: AgentState::Attention, message: Some(m) })]
                if pane == "chat" && m == "a question"
        ));
    }

    #[test]
    fn a_status_from_a_client_without_a_pane_is_an_error() {
        let mut r = router();
        r.connected(1);
        r.handle(1, r#"{"hello": {"name": "tool"}}"#);
        let out = lines(r.handle(1, r#"{"status": {"state": "working"}}"#));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("claimed none")
        );
    }
}
