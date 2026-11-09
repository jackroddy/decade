use std::collections::HashMap;
use std::time::Instant;

use decade_channel::{
    AgentUpdate, Clear, Error, Hello, Highlight, Pane, Reply, Show, Shows, VERSION, Welcome,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::server::agents::Agents;

/// The keys a pane verb may not take: decade's own verbs and the
/// names decade's replies and forwarded messages use.
const RESERVED: [&str; 7] = [
    "hello", "agent", "ended", "panes", "welcome", "error", "from",
];

pub struct Client {
    pub name: String,
    pub accepts: Vec<String>,
    pub pane: Option<String>,
    pub shows: Vec<Shows>,

    /// The session of the agent named in hello, until it sends `ended`.
    pub session: Option<String>,
}

pub enum Out {
    Line(usize, String),

    /// An agent's report changed; `raised` holds the text of the desktop
    /// notification when its pane has just entered attention.
    Agent {
        raised: Option<String>,
    },
}

/// The socket clients of one workspace, by client id, holding only
/// those that have said hello, and the workspace's agents.
#[derive(Default)]
pub struct Router {
    pub clients: HashMap<usize, Client>,
    pub agents: Agents,
}

impl Router {
    /// Drop every client's and agent's claim on a pane that has gone.
    pub fn release(&mut self, pane: &str) {
        for client in self.clients.values_mut() {
            if client.pane.as_deref() == Some(pane) {
                client.pane = None;
            }
        }
        self.agents.release(pane);
    }

    /// Forget client `id`, whose connection has dropped, and mark its
    /// agent disconnected at `now`.
    pub fn disconnect(&mut self, id: usize, now: Instant) {
        let session = self.clients.remove(&id).and_then(|client| client.session);
        if let Some(session) = session {
            self.agents.detach(&session, now);
        }
    }

    pub fn occupant(&self, pane: &str) -> Option<(usize, &Client)> {
        self.clients
            .iter()
            .find(|(_, client)| client.pane.as_deref() == Some(pane))
            .map(|(id, client)| (*id, client))
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
        let client = self.clients.get_mut(&id).unwrap();
        let session = client.session.clone();
        let agent = session.as_deref().and_then(|s| self.agents.get_mut(s));
        if matches!(verb, "agent" | "ended") && agent.is_none() {
            return vec![error_line(
                id,
                &format!("{verb} is for a client whose hello named an agent"),
                value,
            )];
        }
        match verb {
            "agent" => {
                let update: AgentUpdate = match serde_json::from_value(payload) {
                    Ok(update) => update,
                    Err(error) => return malformed(error),
                };
                let Some(pane) = client.pane.as_deref() else {
                    return vec![error_line(
                        id,
                        "agent is for the agent in a pane, and this client claimed none",
                        value,
                    )];
                };
                let agent = agent.unwrap();
                let raised = agent.update(update, Instant::now()).then(|| {
                    (agent.report.message())
                        .map_or_else(|| format!("the agent in {pane}"), str::to_string)
                });
                vec![Out::Agent { raised }]
            }
            "ended" => {
                client.session = None;
                self.agents.end(&session.unwrap());
                vec![Out::Agent { raised: None }]
            }
            "panes" => vec![reply(id, Reply::Panes(self.panes(panes)))],
            _ => vec![error_line(
                id,
                "a message needs \"to\", or one of \"hello\", \"agent\", \"ended\" and \"panes\"",
                value,
            )],
        }
    }

    /// Every pane and the name of the client occupying it.
    pub fn panes(&self, panes: &[&str]) -> Vec<Pane> {
        panes
            .iter()
            .map(|name| {
                let occupant = self.occupant(name).map(|(_, c)| c);
                Pane {
                    name: name.to_string(),
                    client: occupant.map(|c| c.name.clone()),
                    shows: occupant.map(|c| c.shows.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The pane a show, highlight or clear of `kind` goes to: `pane` when
    /// the call names one, or else the one pane whose client shows `kind`.
    pub fn showing(
        &self,
        kind: &str,
        pane: Option<&str>,
        panes: &[&str],
    ) -> Result<String, String> {
        if let Some(pane) = pane {
            return Ok(pane.to_string());
        }
        let list = self.panes(panes);
        let showing: Vec<&str> = list
            .iter()
            .filter(|pane| pane.shows.iter().any(|shows| shows.kind == kind))
            .map(|pane| pane.name.as_str())
            .collect();
        match showing[..] {
            [one] => Ok(one.to_string()),
            [] => {
                let mut kinds: Vec<&str> = Vec::new();
                for shows in list.iter().flat_map(|pane| &pane.shows) {
                    if !kinds.contains(&shows.kind.as_str()) {
                        kinds.push(&shows.kind);
                    }
                }
                Err(if kinds.is_empty() {
                    format!("no pane shows {kind}, and no pane here shows any kind")
                } else {
                    format!(
                        "no pane shows {kind}; the panes here show {}",
                        kinds.join(", ")
                    )
                })
            }
            _ => Err(format!(
                "panes {} all show {kind}; name one in pane",
                showing.join(", ")
            )),
        }
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
        match kind_of(&verb, &payload)? {
            Some((kind, highlights)) => match client.shows.iter().find(|s| s.kind == kind) {
                None => return Err(format!("pane {to} does not show {kind}")),
                Some(shows) if highlights && !shows.highlight => {
                    return Err(format!("pane {to} cannot highlight {kind}"));
                }
                Some(_) => {}
            },
            None if !accepts(client, &verb) => {
                return Err(format!("pane {to} does not accept {verb}"));
            }
            None => {}
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
                out.push(error_line(id, &format!("no pane named {wanted}"), value));
            } else {
                match self.occupant(wanted) {
                    Some((other, occupant)) if other != id => out.push(error_line(
                        id,
                        &format!("pane {wanted} is occupied by {}", occupant.name),
                        value,
                    )),
                    _ => pane = Some(wanted.clone()),
                }
            }
        }
        let now = Instant::now();
        let session = hello.agent.as_ref().map(|agent| agent.session.clone());
        let before = self.clients.remove(&id).and_then(|client| client.session);
        if let Some(before) = before
            && Some(&before) != session.as_ref()
        {
            self.agents.detach(&before, now);
        }
        if let Some(identity) = hello.agent {
            for client in self.clients.values_mut() {
                if client.session == session {
                    client.session = None;
                }
            }
            self.agents.attach(identity, pane.clone(), now);
        }
        self.clients.insert(
            id,
            Client {
                name: hello.name,
                accepts: hello.accepts,
                pane: pane.clone(),
                shows: hello.shows,
                session,
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

/// Whether `client` takes a verb other than show, highlight and clear.
fn accepts(client: &Client, verb: &str) -> bool {
    if client.accepts.is_empty() {
        // a client that shows kinds and lists no verbs takes
        // only the kind verbs, which never reach this check
        return client.shows.is_empty();
    }
    client.accepts.iter().any(|accepted| accepted == verb)
}

/// The kind a `show`, `highlight` or `clear` names, and whether the verb
/// needs the kind to highlight; `None` for every other verb.
fn kind_of(verb: &str, payload: &Value) -> Result<Option<(String, bool)>, String> {
    let malformed = |error: serde_json::Error| format!("malformed {verb}: {error}");
    let kind = match verb {
        "show" => (Show::deserialize(payload).map_err(malformed)?.kind, false),
        "highlight" => (
            Highlight::deserialize(payload).map_err(malformed)?.kind,
            true,
        ),
        "clear" => (Clear::deserialize(payload).map_err(malformed)?.kind, true),
        _ => return Ok(None),
    };
    Ok(Some(kind))
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
    use crate::server::agents::Report;
    use decade_channel::{AgentState, Context, Tokens};
    use std::time::Duration;

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
            r#"{"hello": {"name": "code-pane", "accepts": ["reload"], "pane": "code"}}"#,
            &PANES,
        );
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = lines(r.handle(
            2,
            r#"{"to": "code", "reload": {"path": "src/gpu.rs"}}"#,
            &PANES,
        ));
        assert_eq!(
            out,
            vec![(1, json!({"reload": {"path": "src/gpu.rs"}, "from": "coax"}))]
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
    fn panes_lists_every_pane_and_its_occupant() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = lines(r.handle(1, r#"{"panes": {}}"#, &PANES));
        assert_eq!(
            out[0].1,
            json!({"panes": [
                {"name": "chat", "client": "coax", "shows": []},
                {"name": "terminal", "client": null, "shows": []},
                {"name": "code", "client": null, "shows": []}
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
        for verb in ["agent", "ended", "error", "from"] {
            let line = format!(r#"{{"to": "code", "{verb}": {{"pane": "chat"}}}}"#);
            let out = lines(r.handle(2, &line, &PANES));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].0, 2);
            assert_eq!(out[0].1["error"]["reason"], format!("{verb} is reserved"));
        }
    }

    #[test]
    fn focus_is_not_decades_verb_and_a_pane_may_take_it() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "list", "pane": "code"}}"#, &PANES);
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = lines(r.handle(2, r#"{"focus": {"pane": "code"}}"#, &PANES));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("a message needs \"to\"")
        );
        let out = lines(r.handle(2, r#"{"to": "code", "focus": {"row": 3}}"#, &PANES));
        assert_eq!(out, vec![(1, json!({"focus": {"row": 3}, "from": "coax"}))]);
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
        let out = lines(r.handle(1, r#"{"agent": {"state": "idle"}, "panes": {}}"#, &PANES));
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

    const COAX: &str = r#"{"hello": {"name": "coax", "pane": "chat", "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#;

    fn report(r: &Router) -> &Report {
        &r.agents.list[0].report
    }

    fn reason(out: Vec<Out>) -> String {
        let out = lines(out);
        out[0].1["error"]["reason"].as_str().unwrap().to_string()
    }

    fn raised(out: &[Out]) -> Option<&str> {
        match out {
            [Out::Agent { raised }] => raised.as_deref(),
            _ => panic!("expected one agent change"),
        }
    }

    #[test]
    fn a_hello_with_an_agent_block_records_the_agent_and_one_without_does_not() {
        let mut r = Router::default();
        let out = lines(r.handle(1, COAX, &PANES));
        assert_eq!(out[0].1["welcome"]["pane"], "chat");
        assert_eq!(r.clients[&1].session.as_deref(), Some("s1"));
        let agent = &r.agents.list[0];
        assert_eq!(
            (
                agent.name.as_str(),
                agent.version.as_str(),
                agent.session.as_str()
            ),
            ("coax", "0.4.0", "s1")
        );
        assert_eq!(agent.pane.as_deref(), Some("chat"));
        assert!(agent.connected);
        r.handle(2, r#"{"hello": {"name": "tool"}}"#, &PANES);
        assert!(r.clients[&2].session.is_none());
        assert_eq!(r.agents.list.len(), 1);
    }

    #[test]
    fn agent_updates_merge_and_leave_absent_fields_as_they_were() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(
            1,
            r#"{"agent": {"state": "working", "activity": "cargo test", "model": "opus", "context": {"used": 10, "limit": 100}}}"#,
            &PANES,
        );
        r.handle(
            1,
            r#"{"agent": {"tokens": {"input": 5, "output": 7}, "cost": 0.25}}"#,
            &PANES,
        );
        let report = report(&r);
        assert_eq!(report.state, Some(AgentState::Working));
        assert_eq!(report.activity.as_deref(), Some("cargo test"));
        assert_eq!(report.model.as_deref(), Some("opus"));
        assert_eq!(
            report.context,
            Some(Context {
                used: 10,
                limit: 100
            })
        );
        assert_eq!(
            report.tokens,
            Some(Tokens {
                input: 5,
                output: 7
            })
        );
        assert_eq!(report.cost, Some(0.25));
        assert_eq!(report.question, None);
    }

    #[test]
    fn a_null_field_clears_it_and_the_question_is_shown_over_the_activity_while_set() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(
            1,
            r#"{"agent": {"activity": "reading", "question": "which branch?"}}"#,
            &PANES,
        );
        assert_eq!(report(&r).message(), Some("which branch?"));
        r.handle(1, r#"{"agent": {"question": null}}"#, &PANES);
        assert_eq!(report(&r).question, None);
        assert_eq!(report(&r).message(), Some("reading"));
    }

    #[test]
    fn entering_attention_raises_with_the_question_and_staying_there_does_not() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        let out = r.handle(
            1,
            r#"{"agent": {"state": "attention", "question": "which branch?"}}"#,
            &PANES,
        );
        assert_eq!(raised(&out), Some("which branch?"));
        let out = r.handle(1, r#"{"agent": {"activity": "waiting"}}"#, &PANES);
        assert_eq!(raised(&out), None);
        r.handle(
            1,
            r#"{"agent": {"state": "working", "question": null, "activity": null}}"#,
            &PANES,
        );
        let out = r.handle(1, r#"{"agent": {"state": "attention"}}"#, &PANES);
        assert_eq!(raised(&out), Some("the agent in chat"));
        assert_eq!(r.agents.states().max(), Some(AgentState::Attention));
    }

    #[test]
    fn a_new_state_restarts_the_clients_clock_and_a_repeated_state_keeps_it() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        let update = |r: &mut Router, line: &str| {
            let past = Instant::now() - Duration::from_secs(60);
            r.agents.list[0].since = past;
            r.handle(1, line, &PANES);
            r.agents.list[0].since > past
        };
        assert!(update(&mut r, r#"{"agent": {"state": "working"}}"#));
        assert!(!update(&mut r, r#"{"agent": {"activity": "cargo test"}}"#));
        assert!(!update(&mut r, r#"{"agent": {"state": "working"}}"#));
        assert!(update(&mut r, r#"{"agent": {"state": "done"}}"#));
    }

    #[test]
    fn ended_drops_the_report_and_the_agent_and_later_updates_are_refused() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(1, r#"{"agent": {"state": "working"}}"#, &PANES);
        let out = r.handle(1, r#"{"ended": {}}"#, &PANES);
        assert_eq!(raised(&out), None);
        assert!(r.agents.list.is_empty());
        assert!(r.clients[&1].session.is_none());
        assert_eq!(r.occupant("chat").unwrap().0, 1);
        let out = r.handle(1, r#"{"agent": {"state": "working"}}"#, &PANES);
        assert_eq!(
            reason(out),
            "agent is for a client whose hello named an agent"
        );
    }

    #[test]
    fn agent_and_ended_from_a_client_whose_hello_named_no_agent_are_refused() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "list", "pane": "code"}}"#, &PANES);
        for verb in ["agent", "ended"] {
            let line = format!(r#"{{"{verb}": {{}}}}"#);
            assert_eq!(
                reason(r.handle(1, &line, &PANES)),
                format!("{verb} is for a client whose hello named an agent")
            );
        }
    }

    #[test]
    fn an_agent_update_from_a_client_without_a_pane_is_an_error() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "coax", "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
            &PANES,
        );
        let out = r.handle(1, r#"{"agent": {"state": "working"}}"#, &PANES);
        assert!(reason(out).contains("claimed none"));
    }

    #[test]
    fn status_is_no_longer_decades_verb_and_goes_to_a_pane_like_any_other() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(2, r#"{"hello": {"name": "list", "pane": "code"}}"#, &PANES);
        let out = r.handle(1, r#"{"status": {"state": "working"}}"#, &PANES);
        assert!(reason(out).starts_with("a message needs \"to\""));
        let out = lines(r.handle(1, r#"{"to": "code", "status": {}}"#, &PANES));
        assert_eq!(out, vec![(2, json!({"status": {}, "from": "coax"}))]);
    }

    #[test]
    fn a_second_hello_with_a_new_session_leaves_the_first_agent_disconnected() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(1, r#"{"agent": {"state": "working"}}"#, &PANES);
        r.handle(1, COAX, &PANES);
        assert_eq!(r.agents.list.len(), 1);
        assert_eq!(report(&r).state, Some(AgentState::Working));
        r.handle(1, &COAX.replace("s1", "s2"), &PANES);
        let sessions: Vec<_> = (r.agents.list.iter())
            .map(|agent| (agent.session.as_str(), agent.connected))
            .collect();
        assert_eq!(sessions, [("s2", true)]);
    }

    #[test]
    fn a_dropped_connection_keeps_the_agent_disconnected_with_its_report() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(1, r#"{"agent": {"state": "attention"}}"#, &PANES);
        let dropped = Instant::now();
        r.disconnect(1, dropped);
        assert!(r.occupant("chat").is_none());
        let agent = &r.agents.list[0];
        assert!(!agent.connected);
        assert_eq!(agent.last_seen, dropped);
        assert_eq!(agent.report.state, Some(AgentState::Attention));
        assert_eq!(r.agents.states().max(), None);
    }

    #[test]
    fn a_reconnect_with_the_same_session_reattaches_and_keeps_the_history() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(
            1,
            r#"{"agent": {"state": "working", "model": "opus"}}"#,
            &PANES,
        );
        r.handle(1, r#"{"agent": {"state": "attention"}}"#, &PANES);
        r.disconnect(1, Instant::now());
        r.handle(2, COAX, &PANES);
        r.handle(2, r#"{"agent": {"state": "working"}}"#, &PANES);
        assert_eq!(r.agents.list.len(), 1);
        let agent = &r.agents.list[0];
        assert!(agent.connected);
        assert_eq!(agent.report.model.as_deref(), Some("opus"));
        let states: Vec<_> = agent.history.iter().map(|(state, _)| *state).collect();
        assert_eq!(
            states,
            [
                Some(AgentState::Working),
                Some(AgentState::Attention),
                Some(AgentState::Working)
            ]
        );
    }

    #[test]
    fn a_reconnect_that_claims_another_pane_moves_the_agent_there() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(1, r#"{"agent": {"state": "done"}}"#, &PANES);
        r.disconnect(1, Instant::now());
        r.handle(2, &COAX.replace("\"chat\"", "\"code\""), &PANES);
        assert_eq!(r.agents.list.len(), 1);
        assert_eq!(r.agents.list[0].pane.as_deref(), Some("code"));
        assert_eq!(report(&r).state, Some(AgentState::Done));
        assert_eq!(r.occupant("code").unwrap().0, 2);
    }

    #[test]
    fn a_second_client_naming_a_connected_session_takes_the_agent_over() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(2, &COAX.replace("\"chat\"", "\"code\""), &PANES);
        assert!(r.clients[&1].session.is_none());
        r.disconnect(1, Instant::now());
        assert!(r.agents.list[0].connected);
        assert_eq!(r.agents.list[0].pane.as_deref(), Some("code"));
    }

    #[test]
    fn a_pane_that_goes_clears_the_agents_pane_and_keeps_the_agent() {
        let mut r = Router::default();
        r.handle(1, COAX, &PANES);
        r.handle(1, r#"{"agent": {"state": "working"}}"#, &PANES);
        r.release("chat");
        assert_eq!(r.agents.list[0].pane, None);
        assert_eq!(report(&r).state, Some(AgentState::Working));
    }

    const CODE: &str = r#"{"hello": {"name": "decade-code", "accepts": ["reload"], "pane": "code", "shows": [{"kind": "file", "highlight": true}]}}"#;

    #[test]
    fn panes_lists_the_kinds_each_occupant_declared_at_hello() {
        let mut r = Router::default();
        r.handle(1, CODE, &PANES);
        let out = lines(r.handle(1, r#"{"panes": {}}"#, &PANES));
        assert_eq!(
            out[0].1["panes"][2],
            json!({"name": "code", "client": "decade-code", "shows": [{"kind": "file", "highlight": true}]})
        );
        assert_eq!(out[0].1["panes"][0]["shows"], json!([]));
    }

    #[test]
    fn show_highlight_and_clear_reach_a_pane_that_shows_the_kind_without_being_in_accepts() {
        let mut r = Router::default();
        r.handle(1, CODE, &PANES);
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        for message in [
            json!({"show": {"kind": "file", "target": "a.rs", "at": 4}}),
            json!({"highlight": {"kind": "file", "target": "a.rs", "ranges": [[1, 2]]}}),
            json!({"clear": {"kind": "file"}}),
        ] {
            let mut sent = message.clone();
            sent["to"] = json!("code");
            let out = lines(r.handle(2, &sent.to_string(), &PANES));
            let mut forwarded = message;
            forwarded["from"] = json!("coax");
            assert_eq!(out, vec![(1, forwarded)]);
        }
    }

    #[test]
    fn a_show_is_refused_by_a_pane_that_did_not_declare_its_kind_or_cannot_highlight_it() {
        let mut r = Router::default();
        r.handle(1, CODE, &PANES);
        r.handle(
            2,
            r#"{"hello": {"name": "viewer", "pane": "terminal", "shows": [{"kind": "image"}]}}"#,
            &PANES,
        );
        let reason = |r: &mut Router, line: &str| {
            let out = lines(r.handle(1, line, &PANES));
            assert_eq!(out[0].0, 1);
            out[0].1["error"]["reason"].as_str().unwrap().to_string()
        };
        let url = r#"{"to": "code", "show": {"kind": "url", "target": "https://example.org"}}"#;
        assert_eq!(reason(&mut r, url), "pane code does not show url");
        let image = r#"{"to": "terminal", "highlight": {"kind": "image", "target": "a.png", "ranges": []}}"#;
        assert_eq!(
            reason(&mut r, image),
            "pane terminal cannot highlight image"
        );
        let clear = r#"{"to": "terminal", "clear": {"kind": "image"}}"#;
        assert_eq!(
            reason(&mut r, clear),
            "pane terminal cannot highlight image"
        );
        let malformed = r#"{"to": "code", "show": {"path": "a.rs"}}"#;
        assert!(reason(&mut r, malformed).starts_with("malformed show: missing field `kind`"));
    }

    #[test]
    fn a_kind_goes_to_the_one_pane_that_shows_it() {
        let mut r = Router::default();
        r.handle(1, CODE, &PANES);
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        assert_eq!(r.showing("file", None, &PANES), Ok("code".to_string()));
    }

    #[test]
    fn a_kind_several_panes_show_needs_a_pane_named_and_the_named_pane_is_used() {
        let mut r = Router::default();
        r.handle(1, CODE, &PANES);
        r.handle(
            2,
            r#"{"hello": {"name": "other", "pane": "terminal", "shows": [{"kind": "file"}]}}"#,
            &PANES,
        );
        assert_eq!(
            r.showing("file", None, &PANES),
            Err("panes terminal, code all show file; name one in pane".to_string())
        );
        assert_eq!(
            r.showing("file", Some("terminal"), &PANES),
            Ok("terminal".to_string())
        );
    }

    #[test]
    fn a_named_pane_that_does_not_show_the_kind_is_refused_when_sent_to() {
        let mut r = Router::default();
        r.handle(1, CODE, &PANES);
        r.handle(2, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let to = r.showing("file", Some("chat"), &PANES).unwrap();
        let payload = json!({"kind": "file", "target": "a.rs"});
        let sent = r.send("agent", &to, "show".to_string(), payload, &PANES);
        assert_eq!(sent, Err("pane chat does not show file".to_string()));
    }

    #[test]
    fn a_kind_no_pane_shows_is_refused_with_the_kinds_that_are_shown() {
        let mut r = Router::default();
        assert_eq!(
            r.showing("file", None, &PANES),
            Err("no pane shows file, and no pane here shows any kind".to_string())
        );
        r.handle(
            1,
            r#"{"hello": {"name": "web", "pane": "terminal", "shows": [{"kind": "url"}, {"kind": "image"}]}}"#,
            &PANES,
        );
        assert_eq!(
            r.showing("file", None, &PANES),
            Err("no pane shows file; the panes here show url, image".to_string())
        );
    }

    #[test]
    fn a_client_that_shows_kinds_and_lists_no_verbs_takes_only_the_kind_verbs() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "viewer", "pane": "code", "shows": [{"kind": "file"}]}}"#,
            &PANES,
        );
        r.handle(
            2,
            r#"{"hello": {"name": "anything", "pane": "terminal"}}"#,
            &PANES,
        );
        r.handle(3, r#"{"hello": {"name": "coax", "pane": "chat"}}"#, &PANES);
        let out = lines(r.handle(3, r#"{"to": "code", "reload": {}}"#, &PANES));
        assert_eq!(out[0].0, 3);
        assert_eq!(
            out[0].1["error"]["reason"],
            "pane code does not accept reload"
        );
        let show = r#"{"to": "code", "show": {"kind": "file", "target": "a.rs"}}"#;
        assert_eq!(lines(r.handle(3, show, &PANES))[0].0, 1);
        let out = lines(r.handle(3, r#"{"to": "terminal", "reload": {}}"#, &PANES));
        assert_eq!(out, vec![(2, json!({"reload": {}, "from": "coax"}))]);
    }
}
