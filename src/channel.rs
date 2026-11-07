use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const VERSION: u32 = 1;

#[derive(Deserialize)]
pub struct Hello {
    pub name: String,
    #[serde(default)]
    pub accepts: Vec<String>,
    #[serde(default)]
    pub pane: Option<String>,
}

#[derive(Deserialize)]
pub struct Focus {
    pub pane: String,
}

#[derive(Deserialize)]
struct Envelope {
    hello: Option<Hello>,
    to: Option<String>,
    focus: Option<Focus>,
    panes: Option<Value>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

#[derive(Serialize)]
struct PaneInfo<'a> {
    name: &'a str,
    client: Option<&'a str>,
}

pub struct Client {
    pub name: Option<String>,
    pub accepts: Vec<String>,
    pub pane: Option<usize>,
}

pub enum Host {
    Focus(usize),
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

    pub fn occupant(&self, pane: usize) -> Option<(usize, &Client)> {
        self.clients
            .iter()
            .find(|(_, client)| client.pane == Some(pane))
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
            return match self.pane_index(&focus.pane) {
                Some(pane) => vec![Out::Host(Host::Focus(pane))],
                None => vec![error_line(
                    id,
                    &format!("no pane named {}", focus.pane),
                    value,
                )],
            };
        }
        if envelope.panes.is_some() {
            let panes: Vec<PaneInfo> = self
                .panes
                .iter()
                .enumerate()
                .map(|(i, name)| PaneInfo {
                    name,
                    client: self.occupant(i).and_then(|(_, c)| c.name.as_deref()),
                })
                .collect();
            return vec![Out::Line(id, json!({ "panes": panes }).to_string())];
        }
        let Some(to) = envelope.to else {
            return vec![error_line(
                id,
                "a message needs \"to\", \"focus\", \"panes\" or \"hello\"",
                value,
            )];
        };
        let Some(pane) = self.pane_index(&to) else {
            return vec![error_line(id, &format!("no pane named {to}"), value)];
        };
        let Some((target, client)) = self.occupant(pane) else {
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
            match self.pane_index(wanted) {
                None => out.push(error_line(
                    id,
                    &format!("no pane named {wanted}"),
                    value.clone(),
                )),
                Some(index) => match self.occupant(index) {
                    Some((other, occupant)) if other != id => out.push(error_line(
                        id,
                        &format!(
                            "pane {wanted} is occupied by {}",
                            occupant.name.as_deref().unwrap_or("another client")
                        ),
                        value.clone(),
                    )),
                    _ => pane = Some(index),
                },
            }
        }
        let client = self.clients.entry(id).or_insert(Client {
            name: None,
            accepts: Vec::new(),
            pane: None,
        });
        client.name = Some(hello.name);
        client.accepts = hello.accepts;
        client.pane = pane;
        let welcome = json!({
            "welcome": { "pane": pane.map(|p| self.panes[p].as_str()), "version": VERSION }
        });
        out.push(Out::Line(id, welcome.to_string()));
        out
    }

    fn pane_index(&self, name: &str) -> Option<usize> {
        self.panes.iter().position(|pane| pane == name)
    }
}

fn error_line(id: usize, reason: &str, message: Value) -> Out {
    Out::Line(
        id,
        json!({ "error": { "reason": reason, "message": message } }).to_string(),
    )
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
        assert_eq!(r.occupant(2).unwrap().1.name.as_deref(), Some("code-pane"));
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
        assert!(matches!(out[0], Out::Host(Host::Focus(1))));
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
        assert!(r.occupant(0).is_none());
    }
}
