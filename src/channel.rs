use std::collections::HashMap;

use decade_channel::{AgentState, Error, Focus, Hello, Pane, Reply, Status, VERSION, Welcome};
use serde_json::{Map, Value, json};

/// The keys a pane verb may not take: decade's own verbs and the
/// names decade's replies and forwarded messages use.
const RESERVED: [&str; 7] = [
    "hello", "focus", "status", "panes", "welcome", "error", "from",
];

pub struct Client {
    pub name: String,
    pub accepts: Vec<String>,
    pub pane: Option<String>,

    /// The agent state this client last reported for its pane.
    pub status: Option<Status>,
}

pub enum Out {
    Line(usize, String),
    Focus(String),

    /// A client reported its agent's state; `raised` when the pane has
    /// just entered attention.
    Status {
        pane: String,
        message: Option<String>,
        raised: bool,
    },
}

/// The socket clients of one workspace, by client id, holding only
/// those that have said hello.
#[derive(Default)]
pub struct Router {
    pub clients: HashMap<usize, Client>,
}

impl Router {
    /// Drop every client's claim on a pane that has gone, and the agent
    /// state reported for it.
    pub fn release(&mut self, pane: &str) {
        for client in self.clients.values_mut() {
            if client.pane.as_deref() == Some(pane) {
                client.pane = None;
                client.status = None;
            }
        }
    }

    pub fn occupant(&self, pane: &str) -> Option<(usize, &Client)> {
        self.clients
            .iter()
            .find(|(_, client)| client.pane.as_deref() == Some(pane))
            .map(|(id, client)| (*id, client))
    }

    /// The agent states reported in the workspace.
    pub fn statuses(&self) -> impl Iterator<Item = &Status> {
        self.clients
            .values()
            .filter_map(|client| client.status.as_ref())
    }

    /// Answer one line from client `id`, whose workspace holds `panes`.
    pub fn handle(&mut self, id: usize, line: &str, panes: &[&str]) -> Vec<Out> {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => return vec![error_line(id, &format!("not JSON: {error}"), json!(line))],
        };
        let Value::Object(mut object) = value.clone() else {
            return vec![error_line(id, "a message is a JSON object", value)];
        };
        let to = object.remove("to");
        let mut keys = object.keys();
        let (Some(verb), None) = (keys.next(), keys.next()) else {
            return vec![error_line(id, "a message carries exactly one verb", value)];
        };
        let verb = verb.clone();
        let payload = object.remove(&verb).unwrap();
        if to.is_none() && verb == "hello" {
            return match serde_json::from_value(payload) {
                Ok(hello) => self.hello(id, hello, value, panes),
                Err(error) => vec![error_line(id, &format!("malformed hello: {error}"), value)],
            };
        }
        if !self.clients.contains_key(&id) {
            return vec![error_line(id, "say hello first", value)];
        }
        match to {
            Some(Value::String(to)) => {
                let from = self.clients[&id].name.clone();
                match self.send(&from, &to, verb, payload, panes) {
                    Ok((target, line)) => vec![Out::Line(target, line)],
                    Err(reason) => vec![error_line(id, &reason, value)],
                }
            }
            Some(_) => vec![error_line(id, "\"to\" names a pane", value)],
            None => self.command(id, &verb, payload, value, panes),
        }
    }

    /// Answer a message to decade itself.
    fn command(
        &mut self,
        id: usize,
        verb: &str,
        payload: Value,
        value: Value,
        panes: &[&str],
    ) -> Vec<Out> {
        let malformed = |error: serde_json::Error| {
            vec![error_line(
                id,
                &format!("malformed {verb}: {error}"),
                value.clone(),
            )]
        };
        match verb {
            "focus" => {
                let focus: Focus = match serde_json::from_value(payload) {
                    Ok(focus) => focus,
                    Err(error) => return malformed(error),
                };
                if panes.contains(&focus.pane.as_str()) {
                    vec![Out::Focus(focus.pane)]
                } else {
                    let reason = format!("no pane named {}", focus.pane);
                    vec![error_line(id, &reason, value)]
                }
            }
            "status" => {
                let status: Status = match serde_json::from_value(payload) {
                    Ok(status) => status,
                    Err(error) => return malformed(error),
                };
                let client = self.clients.get_mut(&id).unwrap();
                let Some(pane) = client.pane.clone() else {
                    return vec![error_line(
                        id,
                        "status is for the agent in a pane, and this client claimed none",
                        value,
                    )];
                };
                let attention = |status: Option<&Status>| {
                    status.is_some_and(|status| status.state == AgentState::Attention)
                };
                let raised = attention(Some(&status)) && !attention(client.status.as_ref());
                let message = status.message.clone();
                client.status = Some(status);
                vec![Out::Status {
                    pane,
                    message,
                    raised,
                }]
            }
            "panes" => vec![reply(id, Reply::Panes(self.panes(panes)))],
            _ => vec![error_line(
                id,
                "a message needs \"to\", or one of \"hello\", \"focus\", \"status\" and \"panes\"",
                value,
            )],
        }
    }

    /// Every pane and the name of the client occupying it.
    pub fn panes(&self, panes: &[&str]) -> Vec<Pane> {
        panes
            .iter()
            .map(|name| Pane {
                name: name.to_string(),
                client: self.occupant(name).map(|(_, c)| c.name.clone()),
            })
            .collect()
    }

    /// The id of the client occupying pane `to` and the line that carries
    /// it a pane verb from `from`, or the reason it cannot go.
    pub fn send(
        &self,
        from: &str,
        to: &str,
        verb: String,
        payload: Value,
        panes: &[&str],
    ) -> Result<(usize, String), String> {
        if RESERVED.contains(&verb.as_str()) {
            return Err(format!("{verb} is reserved"));
        }
        if !panes.contains(&to) {
            return Err(format!("no pane named {to}"));
        }
        let (target, client) = self
            .occupant(to)
            .ok_or_else(|| format!("no client occupies pane {to}"))?;
        if !client.accepts.is_empty() && !client.accepts.contains(&verb) {
            return Err(format!("pane {to} does not accept {verb}"));
        }
        let mut forwarded = Map::new();
        forwarded.insert(verb, payload);
        forwarded.insert("from".to_string(), Value::String(from.to_string()));
        Ok((target, Value::Object(forwarded).to_string()))
    }

    fn hello(&mut self, id: usize, hello: Hello, value: Value, panes: &[&str]) -> Vec<Out> {
        let mut out = Vec::new();
        let mut pane = None;
        if let Some(wanted) = &hello.pane {
            if !panes.contains(&wanted.as_str()) {
                out.push(error_line(
                    id,
                    &format!("no pane named {wanted}"),
                    value.clone(),
                ));
            } else {
                match self.occupant(wanted) {
                    Some((other, occupant)) if other != id => out.push(error_line(
                        id,
                        &format!("pane {wanted} is occupied by {}", occupant.name),
                        value.clone(),
                    )),
                    _ => pane = Some(wanted.clone()),
                }
            }
        }
        let status = self
            .clients
            .remove(&id)
            .filter(|client| client.pane == pane)
            .and_then(|client| client.status);
        self.clients.insert(
            id,
            Client {
                name: hello.name,
                accepts: hello.accepts,
                pane: pane.clone(),
                status,
            },
        );
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

    const PANES: [&str; 3] = ["chat", "terminal", "code"];

    fn lines(out: Vec<Out>) -> Vec<(usize, Value)> {
        out.into_iter()
            .map(|o| match o {
                Out::Line(id, s) => (id, serde_json::from_str(&s).unwrap()),
                _ => panic!("a host command where a line was expected"),
            })
            .collect()
    }

    #[test]
    fn hello_claims_the_pane_named_in_it_and_is_welcomed() {
        let mut r = Router::default();
        let out = lines(r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show"], "pane": "code"}}"#,
            &PANES,
        ));
        assert_eq!(
            out,
            vec![(1, json!({"welcome": {"pane": "code", "version": 1}}))]
        );
        assert_eq!(r.occupant("code").unwrap().1.name, "code-pane");
    }

    #[test]
    fn a_second_client_cannot_take_an_occupied_pane() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "first", "pane": "code"}}"#, &PANES);
        let out = lines(r.handle(
            2,
            r#"{"hello": {"name": "second", "pane": "code"}}"#,
            &PANES,
        ));
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
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show", "mark"], "pane": "code"}}"#,
            &PANES,
        );
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = lines(r.handle(
            2,
            r#"{"to": "code", "show": {"path": "src/gpu.rs", "line": 40}}"#,
            &PANES,
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
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show"], "pane": "code"}}"#,
            &PANES,
        );
        r.handle(2, r#"{"hello": {"name": "coax"}}"#, &PANES);
        let out = lines(r.handle(2, r#"{"to": "code", "draw": {}}"#, &PANES));
        assert_eq!(
            out[0].1["error"]["reason"],
            "pane code does not accept draw"
        );
    }

    #[test]
    fn messages_before_hello_and_bad_json_get_errors_back() {
        let mut r = Router::default();
        let out = lines(r.handle(1, r#"{"to": "code", "show": {}}"#, &PANES));
        assert_eq!(out[0].1["error"]["reason"], "say hello first");
        let out = lines(r.handle(1, "not json", &PANES));
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
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = r.handle(1, r#"{"focus": {"pane": "terminal"}}"#, &PANES);
        assert!(matches!(&out[0], Out::Focus(pane) if pane == "terminal"));
        let out = lines(r.handle(1, r#"{"panes": {}}"#, &PANES));
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
    fn a_reserved_name_sent_to_a_pane_is_refused_and_not_forwarded() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "pane": "code"}}"#,
            &PANES,
        );
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        for verb in ["focus", "status", "error", "from"] {
            let line = format!(r#"{{"to": "code", "{verb}": {{"pane": "chat"}}}}"#);
            let out = lines(r.handle(2, &line, &PANES));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].0, 2);
            assert_eq!(out[0].1["error"]["reason"], format!("{verb} is reserved"));
        }
    }

    #[test]
    fn a_message_without_to_must_be_one_of_decades_verbs() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "coax"}}"#, &PANES);
        let out = lines(r.handle(1, r#"{"show": {}}"#, &PANES));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("a message needs \"to\"")
        );
        let out = lines(r.handle(1, r#"{"focus": {"pane": "chat"}, "panes": {}}"#, &PANES));
        assert_eq!(
            out[0].1["error"]["reason"],
            "a message carries exactly one verb"
        );
    }

    #[test]
    fn a_second_hello_renames_the_client_and_moves_its_claim() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "first", "pane": "code"}}"#, &PANES);
        let out = lines(r.handle(
            1,
            r#"{"hello": {"name": "second", "pane": "chat"}}"#,
            &PANES,
        ));
        assert_eq!(out[0].1["welcome"]["pane"], "chat");
        assert!(r.occupant("code").is_none());
        assert_eq!(r.occupant("chat").unwrap().1.name, "second");
        r.handle(1, r#"{"hello": {"name": "third"}}"#, &PANES);
        assert!(r.occupant("chat").is_none());
    }

    #[test]
    fn a_disconnected_client_frees_its_pane() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        r.clients.remove(&1);
        assert!(r.occupant("chat").is_none());
    }

    #[test]
    fn a_status_applies_to_the_pane_its_client_claimed() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = r.handle(
            1,
            r#"{"status": {"state": "attention", "message": "a question"}}"#,
            &PANES,
        );
        assert!(matches!(
            &out[..],
            [Out::Status { pane, message: Some(m), raised: true }]
                if pane == "chat" && m == "a question"
        ));
        let state = r.statuses().map(|status| status.state).max();
        assert_eq!(state, Some(AgentState::Attention));
    }

    #[test]
    fn a_status_from_a_client_without_a_pane_is_an_error() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "tool"}}"#, &PANES);
        let out = lines(r.handle(1, r#"{"status": {"state": "working"}}"#, &PANES));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .contains("claimed none")
        );
    }
}
