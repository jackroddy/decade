use std::collections::HashMap;
use std::time::Instant;

use decade_channel::{
    AgentUpdate, Clear, Error, Hello, Highlight, Pane, Reply, Restore, Screenshot, Show, Shows,
    VERSION, Verb, Welcome,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::server::agents::Agents;

/// The keys a pane verb may not take: decade's own verbs and the
/// names decade's replies and forwarded messages use.
const RESERVED: [&str; 9] = [
    "hello",
    "agent",
    "ended",
    "panes",
    "restore",
    "screenshot",
    "welcome",
    "error",
    "from",
];

pub struct Client {
    pub name: String,
    pub accepts: Vec<Verb>,
    pub pane: Option<String>,
    pub shows: Vec<Shows>,

    /// The session of the agent named in hello, until it sends `ended`.
    pub session: Option<String>,

    /// How decade starts the client again, from hello or `restore`.
    pub restore: Option<Restore>,
}

/// A workspace's pane names: each pane's current name, and the names
/// panes had before a rename with the current name each now reaches.
pub struct PaneNames<'a> {
    pub current: Vec<&'a str>,

    /// Pairs of a former name and the pane's current name.
    pub former: Vec<(&'a str, &'a str)>,
}

impl<'a> PaneNames<'a> {
    /// The current name of the pane that `name` names, now or formerly.
    pub fn resolve(&self, name: &str) -> Option<&'a str> {
        let current = self.current.iter().copied().find(|&pane| pane == name);
        current.or_else(|| {
            (self.former.iter())
                .find(|&&(former, _)| former == name)
                .map(|&(_, pane)| pane)
        })
    }
}

pub enum Out {
    Line(usize, String),

    /// An agent's report changed; `raised` holds the text of the desktop
    /// notification when its pane has just entered attention.
    Agent {
        raised: Option<String>,
    },

    /// Client `client` asks to save a screenshot; `message` is its line,
    /// for the error reply.
    Screenshot {
        client: usize,
        screenshot: Screenshot,
        message: Value,
    },
}

/// The socket clients of one workspace, by client id, holding only
/// those that have said hello, and the workspace's agents.
#[derive(Default)]
pub struct Router {
    pub clients: HashMap<usize, Client>,
    pub agents: Agents,

    /// The pid of the process at the other end of each client's socket,
    /// from the time it connected.
    pub peers: HashMap<usize, u32>,
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

    /// Move every client's and agent's claim on pane `old` to its new
    /// name `new`.
    pub fn rename(&mut self, old: &str, new: &str) {
        for client in self.clients.values_mut() {
            if client.pane.as_deref() == Some(old) {
                client.pane = Some(new.to_string());
            }
        }
        self.agents.rename(old, new);
    }

    /// Forget client `id`, whose connection has dropped, and mark its
    /// agent disconnected at `now`.
    pub fn disconnect(&mut self, id: usize, now: Instant) {
        self.peers.remove(&id);
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
    pub fn handle(
        &mut self,
        id: usize,
        line: &str,
        panes: &PaneNames,
        scratch: &[&str],
    ) -> Vec<Out> {
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
            None => self.command(id, &verb, payload, value, panes, scratch),
        }
    }

    /// Answer a message to decade itself.
    fn command(
        &mut self,
        id: usize,
        verb: &str,
        payload: Value,
        value: Value,
        panes: &PaneNames,
        scratch: &[&str],
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
            "restore" => {
                let restore: Restore = match serde_json::from_value(payload) {
                    Ok(restore) => restore,
                    Err(error) => return malformed(error),
                };
                client.restore = Some(restore).filter(|restore| !restore.argv.is_empty());
                Vec::new()
            }
            "panes" => {
                let caller = self.clients[&id].pane.clone();
                let list = self.panes(panes, scratch, caller.as_deref());
                vec![reply(id, Reply::Panes(list))]
            }
            "screenshot" => match serde_json::from_value(payload) {
                Ok(screenshot) => vec![Out::Screenshot {
                    client: id,
                    screenshot,
                    message: value,
                }],
                Err(error) => malformed(error),
            },
            _ => vec![error_line(
                id,
                "a message needs \"to\", or one of \"hello\", \"agent\", \"ended\", \"panes\", \"restore\" and \"screenshot\"",
                value,
            )],
        }
    }

    /// Every pane and the name of the client occupying it, marking the
    /// scratch panes and the pane `you`.
    pub fn panes(&self, panes: &PaneNames, scratch: &[&str], you: Option<&str>) -> Vec<Pane> {
        (panes.current.iter())
            .map(|name| {
                let occupant = self.occupant(name).map(|(_, c)| c);
                Pane {
                    name: name.to_string(),
                    client: occupant.map(|c| c.name.clone()),
                    shows: occupant.map(|c| c.shows.clone()).unwrap_or_default(),
                    accepts: occupant.map(|c| c.accepts.clone()).unwrap_or_default(),
                    scratch: scratch.contains(name),
                    you: you == Some(*name),
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
        panes: &PaneNames,
    ) -> Result<String, String> {
        if let Some(pane) = pane {
            return (panes.resolve(pane))
                .map(str::to_string)
                .ok_or_else(|| format!("no pane named {pane}"));
        }
        let list = self.panes(panes, &[], None);
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
        panes: &PaneNames,
    ) -> Result<(usize, String), String> {
        if RESERVED.contains(&verb.as_str()) {
            return Err(format!("{verb} is reserved"));
        }
        let to = panes
            .resolve(to)
            .ok_or_else(|| format!("no pane named {to}"))?;
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

    fn hello(&mut self, id: usize, hello: Hello, value: Value, panes: &PaneNames) -> Vec<Out> {
        let mut out = Vec::new();
        let mut pane = None;
        if let Some(wanted) = &hello.pane {
            match panes.resolve(wanted) {
                None => out.push(error_line(id, &format!("no pane named {wanted}"), value)),
                Some(current) => match self.occupant(current) {
                    Some((other, occupant)) if other != id => out.push(error_line(
                        id,
                        &format!("pane {current} is occupied by {}", occupant.name),
                        value,
                    )),
                    _ => pane = Some(current.to_string()),
                },
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
                restore: hello.restore.filter(|restore| !restore.argv.is_empty()),
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
    client.accepts.iter().any(|accepted| accepted.name == verb)
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

    fn names() -> PaneNames<'static> {
        PaneNames {
            current: PANES.to_vec(),
            former: Vec::new(),
        }
    }

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
            &names(),
            &[],
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
        r.handle(
            1,
            r#"{"hello": {"name": "first", "pane": "code"}}"#,
            &names(),
            &[],
        );
        let out = lines(r.handle(
            2,
            r#"{"hello": {"name": "second", "pane": "code"}}"#,
            &names(),
            &[],
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
            &names(),
            &[],
        );
        r.handle(
            2,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        let out = lines(r.handle(
            2,
            r#"{"to": "code", "reload": {"path": "src/gpu.rs"}}"#,
            &names(),
            &[],
        ));
        assert_eq!(
            out,
            vec![(1, json!({"reload": {"path": "src/gpu.rs"}, "from": "coax"}))]
        );
    }

    #[test]
    fn a_verb_listed_with_a_description_is_accepted_like_a_bare_name() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": [{"name": "reload", "description": "Read the file again."}], "pane": "code"}}"#,
            &names(),
            &[],
        );
        r.handle(2, r#"{"hello": {"name": "coax"}}"#, &names(), &[]);
        let out = lines(r.handle(2, r#"{"to": "code", "reload": {}}"#, &names(), &[]));
        assert_eq!(out, vec![(1, json!({"reload": {}, "from": "coax"}))]);
        let out = lines(r.handle(2, r#"{"to": "code", "draw": {}}"#, &names(), &[]));
        assert_eq!(
            out[0].1["error"]["reason"],
            "pane code does not accept draw"
        );
    }

    #[test]
    fn a_verb_the_occupant_does_not_accept_is_refused() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "accepts": ["show"], "pane": "code"}}"#,
            &names(),
            &[],
        );
        r.handle(2, r#"{"hello": {"name": "coax"}}"#, &names(), &[]);
        let out = lines(r.handle(2, r#"{"to": "code", "draw": {}}"#, &names(), &[]));
        assert_eq!(
            out[0].1["error"]["reason"],
            "pane code does not accept draw"
        );
    }

    #[test]
    fn messages_before_hello_and_bad_json_get_errors_back() {
        let mut r = Router::default();
        let out = lines(r.handle(1, r#"{"to": "code", "show": {}}"#, &names(), &[]));
        assert_eq!(out[0].1["error"]["reason"], "say hello first");
        let out = lines(r.handle(1, "not json", &names(), &[]));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("not JSON")
        );
        assert_eq!(out[0].1["error"]["message"], "not json");
    }

    #[test]
    fn panes_lists_every_pane_its_occupant_the_scratch_panes_and_the_caller() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "coax", "pane": "chat", "accepts": ["ping", {"name": "ask", "description": "Put a question to the agent."}]}}"#,
            &names(),
            &[],
        );
        let out = lines(r.handle(1, r#"{"panes": {}}"#, &names(), &["code"]));
        let accepts =
            json!(["ping", {"name": "ask", "description": "Put a question to the agent."}]);
        assert_eq!(
            out[0].1,
            json!({"panes": [
                {"name": "chat", "client": "coax", "shows": [], "accepts": accepts, "scratch": false, "you": true},
                {"name": "terminal", "client": null, "shows": [], "accepts": [], "scratch": false, "you": false},
                {"name": "code", "client": null, "shows": [], "accepts": [], "scratch": true, "you": false}
            ]})
        );
    }

    #[test]
    fn a_screenshot_message_goes_to_decade_with_its_pane_file_and_caller() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "decade screenshot"}}"#,
            &names(),
            &[],
        );
        let line = r#"{"screenshot": {"pane": "code", "file": "shot.png", "for": "chat"}}"#;
        let out = r.handle(1, line, &names(), &[]);
        let [
            Out::Screenshot {
                client,
                screenshot,
                message,
            },
        ] = &out[..]
        else {
            panic!("one screenshot request");
        };
        assert_eq!(*client, 1);
        assert_eq!(screenshot.pane, "code");
        assert_eq!(screenshot.file, "shot.png");
        assert_eq!(screenshot.caller.as_deref(), Some("chat"));
        assert_eq!(*message, serde_json::from_str::<Value>(line).unwrap());
        let out = r.handle(
            1,
            r#"{"screenshot": {"pane": "code", "file": "shot.png"}}"#,
            &names(),
            &[],
        );
        assert!(
            matches!(&out[..], [Out::Screenshot { screenshot, .. }] if screenshot.caller.is_none())
        );
        let out = lines(r.handle(1, r#"{"screenshot": {"pane": "code"}}"#, &names(), &[]));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("malformed screenshot: missing field `file`")
        );
    }

    #[test]
    fn a_reserved_name_sent_to_a_pane_is_refused_and_not_forwarded() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "code-pane", "pane": "code"}}"#,
            &names(),
            &[],
        );
        r.handle(
            2,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        for verb in ["agent", "ended", "restore", "screenshot", "error", "from"] {
            let line = format!(r#"{{"to": "code", "{verb}": {{"pane": "chat"}}}}"#);
            let out = lines(r.handle(2, &line, &names(), &[]));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].0, 2);
            assert_eq!(out[0].1["error"]["reason"], format!("{verb} is reserved"));
        }
    }

    #[test]
    fn focus_is_not_decades_verb_and_a_pane_may_take_it() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "list", "pane": "code"}}"#,
            &names(),
            &[],
        );
        r.handle(
            2,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        let out = lines(r.handle(2, r#"{"focus": {"pane": "code"}}"#, &names(), &[]));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("a message needs \"to\"")
        );
        let out = lines(r.handle(2, r#"{"to": "code", "focus": {"row": 3}}"#, &names(), &[]));
        assert_eq!(out, vec![(1, json!({"focus": {"row": 3}, "from": "coax"}))]);
    }

    #[test]
    fn a_message_without_to_must_be_one_of_decades_verbs() {
        let mut r = Router::default();
        r.handle(1, r#"{"hello": {"name": "coax"}}"#, &names(), &[]);
        let out = lines(r.handle(1, r#"{"show": {}}"#, &names(), &[]));
        assert!(
            out[0].1["error"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("a message needs \"to\"")
        );
        let out = lines(r.handle(
            1,
            r#"{"agent": {"state": "idle"}, "panes": {}}"#,
            &names(),
            &[],
        ));
        assert_eq!(
            out[0].1["error"]["reason"],
            "a message carries exactly one verb"
        );
    }

    #[test]
    fn a_second_hello_renames_the_client_and_moves_its_claim() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "first", "pane": "code"}}"#,
            &names(),
            &[],
        );
        let out = lines(r.handle(
            1,
            r#"{"hello": {"name": "second", "pane": "chat"}}"#,
            &names(),
            &[],
        ));
        assert_eq!(out[0].1["welcome"]["pane"], "chat");
        assert!(r.occupant("code").is_none());
        assert_eq!(r.occupant("chat").unwrap().1.name, "second");
        r.handle(1, r#"{"hello": {"name": "third"}}"#, &names(), &[]);
        assert!(r.occupant("chat").is_none());
    }

    #[test]
    fn a_disconnected_client_frees_its_pane() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
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
        let out = lines(r.handle(1, COAX, &names(), &[]));
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
        r.handle(2, r#"{"hello": {"name": "tool"}}"#, &names(), &[]);
        assert!(r.clients[&2].session.is_none());
        assert_eq!(r.agents.list.len(), 1);
    }

    #[test]
    fn agent_updates_merge_and_leave_absent_fields_as_they_were() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(
            1,
            r#"{"agent": {"state": "working", "activity": "cargo test", "model": "opus", "context": {"used": 10, "limit": 100}}}"#,
            &names(),
            &[],
        );
        r.handle(
            1,
            r#"{"agent": {"tokens": {"input": 5, "output": 7}, "cost": 0.25}}"#,
            &names(),
            &[],
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
    fn a_done_agent_with_a_working_subagent_counts_as_working() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(
            1,
            r#"{"agent": {"state": "done", "subagents": [{"name": "tests", "activity": "cargo test", "state": "working"}]}}"#,
            &names(),
            &[],
        );
        assert_eq!(
            report(&r).subagents[0].activity.as_deref(),
            Some("cargo test")
        );
        assert_eq!(r.agents.states().max(), Some(AgentState::Working));
        r.handle(1, r#"{"agent": {"subagents": []}}"#, &names(), &[]);
        assert_eq!(r.agents.states().max(), Some(AgentState::Done));
    }

    #[test]
    fn a_null_field_clears_it_and_the_question_is_shown_over_the_activity_while_set() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(
            1,
            r#"{"agent": {"activity": "reading", "question": "which branch?"}}"#,
            &names(),
            &[],
        );
        assert_eq!(report(&r).message(), Some("which branch?"));
        r.handle(1, r#"{"agent": {"question": null}}"#, &names(), &[]);
        assert_eq!(report(&r).question, None);
        assert_eq!(report(&r).message(), Some("reading"));
    }

    #[test]
    fn entering_attention_raises_with_the_question_and_staying_there_does_not() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        let out = r.handle(
            1,
            r#"{"agent": {"state": "attention", "question": "which branch?"}}"#,
            &names(),
            &[],
        );
        assert_eq!(raised(&out), Some("which branch?"));
        let out = r.handle(1, r#"{"agent": {"activity": "waiting"}}"#, &names(), &[]);
        assert_eq!(raised(&out), None);
        r.handle(
            1,
            r#"{"agent": {"state": "working", "question": null, "activity": null}}"#,
            &names(),
            &[],
        );
        let out = r.handle(1, r#"{"agent": {"state": "attention"}}"#, &names(), &[]);
        assert_eq!(raised(&out), Some("the agent in chat"));
        assert_eq!(r.agents.states().max(), Some(AgentState::Attention));
    }

    #[test]
    fn a_new_state_restarts_the_clients_clock_and_a_repeated_state_keeps_it() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        let update = |r: &mut Router, line: &str| {
            let past = Instant::now() - Duration::from_secs(60);
            r.agents.list[0].since = past;
            r.handle(1, line, &names(), &[]);
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
        r.handle(1, COAX, &names(), &[]);
        r.handle(1, r#"{"agent": {"state": "working"}}"#, &names(), &[]);
        let out = r.handle(1, r#"{"ended": {}}"#, &names(), &[]);
        assert_eq!(raised(&out), None);
        assert!(r.agents.list.is_empty());
        assert!(r.clients[&1].session.is_none());
        assert_eq!(r.occupant("chat").unwrap().0, 1);
        let out = r.handle(1, r#"{"agent": {"state": "working"}}"#, &names(), &[]);
        assert_eq!(
            reason(out),
            "agent is for a client whose hello named an agent"
        );
    }

    #[test]
    fn agent_and_ended_from_a_client_whose_hello_named_no_agent_are_refused() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "list", "pane": "code"}}"#,
            &names(),
            &[],
        );
        for verb in ["agent", "ended"] {
            let line = format!(r#"{{"{verb}": {{}}}}"#);
            assert_eq!(
                reason(r.handle(1, &line, &names(), &[])),
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
            &names(),
            &[],
        );
        let out = r.handle(1, r#"{"agent": {"state": "working"}}"#, &names(), &[]);
        assert!(reason(out).contains("claimed none"));
    }

    #[test]
    fn status_is_no_longer_decades_verb_and_goes_to_a_pane_like_any_other() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(
            2,
            r#"{"hello": {"name": "list", "pane": "code"}}"#,
            &names(),
            &[],
        );
        let out = r.handle(1, r#"{"status": {"state": "working"}}"#, &names(), &[]);
        assert!(reason(out).starts_with("a message needs \"to\""));
        let out = lines(r.handle(1, r#"{"to": "code", "status": {}}"#, &names(), &[]));
        assert_eq!(out, vec![(2, json!({"status": {}, "from": "coax"}))]);
    }

    #[test]
    fn a_second_hello_with_a_new_session_leaves_the_first_agent_disconnected() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(1, r#"{"agent": {"state": "working"}}"#, &names(), &[]);
        r.handle(1, COAX, &names(), &[]);
        assert_eq!(r.agents.list.len(), 1);
        assert_eq!(report(&r).state, Some(AgentState::Working));
        r.handle(1, &COAX.replace("s1", "s2"), &names(), &[]);
        let sessions: Vec<_> = (r.agents.list.iter())
            .map(|agent| (agent.session.as_str(), agent.connected))
            .collect();
        assert_eq!(sessions, [("s2", true)]);
    }

    #[test]
    fn a_dropped_connection_keeps_the_agent_disconnected_with_its_report() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(1, r#"{"agent": {"state": "attention"}}"#, &names(), &[]);
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
        r.handle(1, COAX, &names(), &[]);
        r.handle(
            1,
            r#"{"agent": {"state": "working", "model": "opus"}}"#,
            &names(),
            &[],
        );
        r.handle(1, r#"{"agent": {"state": "attention"}}"#, &names(), &[]);
        r.disconnect(1, Instant::now());
        r.handle(2, COAX, &names(), &[]);
        r.handle(2, r#"{"agent": {"state": "working"}}"#, &names(), &[]);
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
        r.handle(1, COAX, &names(), &[]);
        r.handle(1, r#"{"agent": {"state": "done"}}"#, &names(), &[]);
        r.disconnect(1, Instant::now());
        r.handle(2, &COAX.replace("\"chat\"", "\"code\""), &names(), &[]);
        assert_eq!(r.agents.list.len(), 1);
        assert_eq!(r.agents.list[0].pane.as_deref(), Some("code"));
        assert_eq!(report(&r).state, Some(AgentState::Done));
        assert_eq!(r.occupant("code").unwrap().0, 2);
    }

    #[test]
    fn a_second_client_naming_a_connected_session_takes_the_agent_over() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(2, &COAX.replace("\"chat\"", "\"code\""), &names(), &[]);
        assert!(r.clients[&1].session.is_none());
        r.disconnect(1, Instant::now());
        assert!(r.agents.list[0].connected);
        assert_eq!(r.agents.list[0].pane.as_deref(), Some("code"));
    }

    #[test]
    fn a_pane_that_goes_clears_the_agents_pane_and_keeps_the_agent() {
        let mut r = Router::default();
        r.handle(1, COAX, &names(), &[]);
        r.handle(1, r#"{"agent": {"state": "working"}}"#, &names(), &[]);
        r.release("chat");
        assert_eq!(r.agents.list[0].pane, None);
        assert_eq!(report(&r).state, Some(AgentState::Working));
    }

    const CODE: &str = r#"{"hello": {"name": "decade-code", "accepts": ["reload"], "pane": "code", "shows": [{"kind": "file", "highlight": true}]}}"#;

    #[test]
    fn panes_lists_the_kinds_each_occupant_declared_at_hello() {
        let mut r = Router::default();
        r.handle(1, CODE, &names(), &[]);
        let out = lines(r.handle(1, r#"{"panes": {}}"#, &names(), &[]));
        assert_eq!(
            out[0].1["panes"][2],
            json!({"name": "code", "client": "decade-code", "shows": [{"kind": "file", "highlight": true}], "accepts": ["reload"], "scratch": false, "you": true})
        );
        assert_eq!(out[0].1["panes"][0]["shows"], json!([]));
    }

    #[test]
    fn show_highlight_and_clear_reach_a_pane_that_shows_the_kind_without_being_in_accepts() {
        let mut r = Router::default();
        r.handle(1, CODE, &names(), &[]);
        r.handle(
            2,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        for message in [
            json!({"show": {"kind": "file", "target": "a.rs", "at": 4}}),
            json!({"highlight": {"kind": "file", "target": "a.rs", "ranges": [[1, 2]]}}),
            json!({"clear": {"kind": "file"}}),
        ] {
            let mut sent = message.clone();
            sent["to"] = json!("code");
            let out = lines(r.handle(2, &sent.to_string(), &names(), &[]));
            let mut forwarded = message;
            forwarded["from"] = json!("coax");
            assert_eq!(out, vec![(1, forwarded)]);
        }
    }

    #[test]
    fn a_show_is_refused_by_a_pane_that_did_not_declare_its_kind_or_cannot_highlight_it() {
        let mut r = Router::default();
        r.handle(1, CODE, &names(), &[]);
        r.handle(
            2,
            r#"{"hello": {"name": "viewer", "pane": "terminal", "shows": [{"kind": "image"}]}}"#,
            &names(),
            &[],
        );
        let reason = |r: &mut Router, line: &str| {
            let out = lines(r.handle(1, line, &names(), &[]));
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
        r.handle(1, CODE, &names(), &[]);
        r.handle(
            2,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        assert_eq!(r.showing("file", None, &names()), Ok("code".to_string()));
    }

    #[test]
    fn a_kind_several_panes_show_needs_a_pane_named_and_the_named_pane_is_used() {
        let mut r = Router::default();
        r.handle(1, CODE, &names(), &[]);
        r.handle(
            2,
            r#"{"hello": {"name": "other", "pane": "terminal", "shows": [{"kind": "file"}]}}"#,
            &names(),
            &[],
        );
        assert_eq!(
            r.showing("file", None, &names()),
            Err("panes terminal, code all show file; name one in pane".to_string())
        );
        assert_eq!(
            r.showing("file", Some("terminal"), &names()),
            Ok("terminal".to_string())
        );
    }

    #[test]
    fn a_named_pane_that_does_not_show_the_kind_is_refused_when_sent_to() {
        let mut r = Router::default();
        r.handle(1, CODE, &names(), &[]);
        r.handle(
            2,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        let to = r.showing("file", Some("chat"), &names()).unwrap();
        let payload = json!({"kind": "file", "target": "a.rs"});
        let sent = r.send("agent", &to, "show".to_string(), payload, &names());
        assert_eq!(sent, Err("pane chat does not show file".to_string()));
    }

    #[test]
    fn a_kind_no_pane_shows_is_refused_with_the_kinds_that_are_shown() {
        let mut r = Router::default();
        assert_eq!(
            r.showing("file", None, &names()),
            Err("no pane shows file, and no pane here shows any kind".to_string())
        );
        r.handle(
            1,
            r#"{"hello": {"name": "web", "pane": "terminal", "shows": [{"kind": "url"}, {"kind": "image"}]}}"#,
            &names(),
            &[],
        );
        assert_eq!(
            r.showing("file", None, &names()),
            Err("no pane shows file; the panes here show url, image".to_string())
        );
    }

    #[test]
    fn a_client_that_shows_kinds_and_lists_no_verbs_takes_only_the_kind_verbs() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "viewer", "pane": "code", "shows": [{"kind": "file"}]}}"#,
            &names(),
            &[],
        );
        r.handle(
            2,
            r#"{"hello": {"name": "anything", "pane": "terminal"}}"#,
            &names(),
            &[],
        );
        r.handle(
            3,
            r#"{"hello": {"name": "coax", "pane": "chat"}}"#,
            &names(),
            &[],
        );
        let out = lines(r.handle(3, r#"{"to": "code", "reload": {}}"#, &names(), &[]));
        assert_eq!(out[0].0, 3);
        assert_eq!(
            out[0].1["error"]["reason"],
            "pane code does not accept reload"
        );
        let show = r#"{"to": "code", "show": {"kind": "file", "target": "a.rs"}}"#;
        assert_eq!(lines(r.handle(3, show, &names(), &[]))[0].0, 1);
        let out = lines(r.handle(3, r#"{"to": "terminal", "reload": {}}"#, &names(), &[]));
        assert_eq!(out, vec![(2, json!({"reload": {}, "from": "coax"}))]);
    }

    #[test]
    fn a_former_pane_name_reaches_the_renamed_pane_and_a_hello_naming_it_claims_the_pane() {
        let renamed = PaneNames {
            current: vec!["chat", "build"],
            former: vec![("terminal", "build")],
        };
        let mut r = Router::default();
        let out = lines(r.handle(
            1,
            r#"{"hello": {"name": "shell", "accepts": ["reload"], "pane": "terminal"}}"#,
            &renamed,
            &[],
        ));
        assert_eq!(out[0].1["welcome"]["pane"], "build");
        assert_eq!(r.clients[&1].pane.as_deref(), Some("build"));
        r.handle(2, r#"{"hello": {"name": "coax"}}"#, &renamed, &[]);
        let out = lines(r.handle(2, r#"{"to": "terminal", "reload": {}}"#, &renamed, &[]));
        assert_eq!(out, vec![(1, json!({"reload": {}, "from": "coax"}))]);
        let listed: Vec<String> = (r.panes(&renamed, &[], None).into_iter())
            .map(|pane| pane.name)
            .collect();
        assert_eq!(listed, ["chat", "build"]);
    }

    #[test]
    fn a_restore_block_from_hello_is_replaced_by_restore_and_cleared_by_an_empty_argv() {
        let mut r = Router::default();
        let out = r.handle(
            1,
            r#"{"hello": {"name": "decade-notes", "pane": "code", "restore": {"argv": ["decade-notes", "--file", "todo.md"]}}}"#,
            &names(),
            &[],
        );
        assert_eq!(out.len(), 1);
        let restore = r.clients[&1].restore.clone().unwrap();
        assert_eq!(restore.argv, ["decade-notes", "--file", "todo.md"]);
        assert!(!restore.terminal);
        let out = r.handle(
            1,
            r#"{"restore": {"argv": ["htop"], "terminal": true}}"#,
            &names(),
            &[],
        );
        assert!(out.is_empty());
        let restore = r.clients[&1].restore.clone().unwrap();
        assert_eq!(
            (restore.argv, restore.terminal),
            (vec!["htop".into()], true)
        );
        r.handle(1, r#"{"restore": {"argv": []}}"#, &names(), &[]);
        assert!(r.clients[&1].restore.is_none());
        let out = r.handle(1, r#"{"restore": {}}"#, &names(), &[]);
        assert!(reason(out).starts_with("malformed restore"));
    }

    #[test]
    fn a_hello_with_an_empty_argv_holds_no_restore_block() {
        let mut r = Router::default();
        r.handle(
            1,
            r#"{"hello": {"name": "list", "pane": "code", "restore": {"argv": []}}}"#,
            &names(),
            &[],
        );
        assert!(r.clients[&1].restore.is_none());
    }
}
