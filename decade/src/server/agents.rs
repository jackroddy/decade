use std::collections::VecDeque;
use std::time::{Duration, Instant};

use decade_channel::{AgentState, AgentUpdate, Context, Tokens};
use serde_json::{Value, json};

/// How many state changes an agent keeps.
const HISTORY: usize = 32;

/// How long a disconnected agent is kept for its session to come back.
const KEEP_FOR: Duration = Duration::from_secs(10 * 60);

/// How long a done agent stays done once its workspace has been shown.
const DONE_FOR: Duration = Duration::from_secs(10 * 60);

/// The fields an agent has reported and not cleared.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub state: Option<AgentState>,
    pub activity: Option<String>,
    pub question: Option<String>,
    pub model: Option<String>,
    pub context: Option<Context>,
    pub tokens: Option<Tokens>,
    pub cost: Option<f64>,
}

impl Report {
    fn merge(&mut self, update: AgentUpdate) {
        let AgentUpdate {
            state,
            activity,
            question,
            model,
            context,
            tokens,
            cost,
        } = update;
        if let Some(state) = state {
            self.state = state;
        }
        if let Some(activity) = activity {
            self.activity = activity;
        }
        if let Some(question) = question {
            self.question = question;
        }
        if let Some(model) = model {
            self.model = model;
        }
        if let Some(context) = context {
            self.context = context;
        }
        if let Some(tokens) = tokens {
            self.tokens = tokens;
        }
        if let Some(cost) = cost {
            self.cost = cost;
        }
    }

    /// The line shown beside the state: the question, or else the activity.
    pub fn message(&self) -> Option<&str> {
        self.question.as_deref().or(self.activity.as_deref())
    }
}

/// An agent a socket client named in hello, kept across reconnects.
#[derive(Debug)]
pub struct Agent {
    pub session: String,
    pub name: String,
    pub version: String,
    pub pane: Option<String>,

    /// What the agent has reported, every update merged.
    pub report: Report,

    /// When the agent first said hello or last changed state.
    pub since: Instant,

    /// The last `HISTORY` state changes, oldest first.
    pub history: VecDeque<(Option<AgentState>, Instant)>,

    /// When the agent last said something or its client disconnected.
    pub last_seen: Instant,
    pub connected: bool,

    /// When a done agent returns to idle, set once its workspace is shown.
    pub idle_at: Option<Instant>,
}

impl Agent {
    /// Whether a client holds the agent and its pane is still there.
    pub fn live(&self) -> bool {
        self.connected && self.pane.is_some()
    }

    /// Merge an update, and return whether it entered attention.
    pub fn update(&mut self, update: AgentUpdate, now: Instant) -> bool {
        let before = self.report.state;
        self.report.merge(update);
        self.last_seen = now;
        self.idle_at = None;
        let after = self.report.state;
        if after != before {
            self.changed(now);
        }
        after == Some(AgentState::Attention) && before != Some(AgentState::Attention)
    }

    fn idle(&mut self, now: Instant) {
        self.report.state = Some(AgentState::Idle);
        self.idle_at = None;
        self.changed(now);
    }

    fn changed(&mut self, now: Instant) {
        self.since = now;
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back((self.report.state, now));
    }
}

/// The agents of one workspace, in the order they first said hello.
#[derive(Default)]
pub struct Agents {
    pub list: Vec<Agent>,
}

impl Agents {
    pub fn get_mut(&mut self, session: &str) -> Option<&mut Agent> {
        self.list.iter_mut().find(|agent| agent.session == session)
    }

    /// Connect the agent of a hello in pane `pane`: the session's agent
    /// when the workspace holds it, or else a new one. A disconnected
    /// agent left in that pane goes.
    pub fn attach(&mut self, identity: decade_channel::Agent, pane: Option<String>, now: Instant) {
        let decade_channel::Agent {
            name,
            version,
            session,
        } = identity;
        if pane.is_some() {
            self.list
                .retain(|agent| agent.connected || agent.session == session || agent.pane != pane);
        }
        match self.get_mut(&session) {
            Some(agent) => {
                agent.name = name;
                agent.version = version;
                agent.pane = pane;
                agent.connected = true;
                agent.last_seen = now;
            }
            None => self.list.push(Agent {
                session,
                name,
                version,
                pane,
                report: Report::default(),
                since: now,
                history: VecDeque::new(),
                last_seen: now,
                connected: true,
                idle_at: None,
            }),
        }
    }

    /// Mark the session's agent disconnected at `now`.
    pub fn detach(&mut self, session: &str, now: Instant) {
        if let Some(agent) = self.get_mut(session) {
            agent.connected = false;
            agent.last_seen = now;
        }
    }

    pub fn end(&mut self, session: &str) {
        self.list.retain(|agent| agent.session != session);
    }

    /// Clear the pane of every agent in pane `pane`, which has gone.
    pub fn release(&mut self, pane: &str) {
        for agent in &mut self.list {
            if agent.pane.as_deref() == Some(pane) {
                agent.pane = None;
            }
        }
    }

    /// Move every agent in pane `old` to its new name `new`.
    pub fn rename(&mut self, old: &str, new: &str) {
        for agent in &mut self.list {
            if agent.pane.as_deref() == Some(old) {
                agent.pane = Some(new.to_string());
            }
        }
    }

    /// Drop the agents disconnected for `KEEP_FOR` by `now`, and return
    /// whether any went.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.list.len();
        self.list.retain(|agent| {
            agent.connected || now.saturating_duration_since(agent.last_seen) < KEEP_FOR
        });
        self.list.len() != before
    }

    /// Return every agent that is done or needs attention to idle, as
    /// the person at the screen.
    pub fn clear(&mut self, now: Instant) {
        for agent in &mut self.list {
            if agent.report.state > Some(AgentState::Working) {
                agent.idle(now);
            }
        }
    }

    /// Start the clock on each done agent without one, for a workspace
    /// shown at `now`.
    pub fn shown(&mut self, now: Instant) {
        for agent in &mut self.list {
            if agent.report.state == Some(AgentState::Done) && agent.idle_at.is_none() {
                agent.idle_at = Some(now + DONE_FOR);
            }
        }
    }

    /// Return the done agents whose clock has run out by `now` to idle,
    /// and return whether any did.
    pub fn settle(&mut self, now: Instant) -> bool {
        let mut settled = false;
        for agent in &mut self.list {
            if agent.report.state == Some(AgentState::Done)
                && agent.idle_at.is_some_and(|at| at <= now)
            {
                agent.idle(now);
                settled = true;
            }
        }
        settled
    }

    /// The states the workspace's live agents have reported.
    pub fn states(&self) -> impl Iterator<Item = AgentState> {
        (self.list.iter())
            .filter(|agent| agent.live())
            .filter_map(|agent| agent.report.state)
    }

    /// The answer to the MCP tool `agents`.
    pub fn listing(&self, now: Instant) -> Value {
        let agents: Vec<Value> = (self.list.iter())
            .map(|agent| {
                let report = &agent.report;
                json!({
                    "name": agent.name,
                    "pane": agent.pane,
                    "state": report.state,
                    "age": now.saturating_duration_since(agent.since).as_secs(),
                    "question": report.question,
                    "activity": report.activity,
                    "model": report.model,
                    "context": report.context,
                    "tokens": report.tokens,
                    "cost": report.cost,
                    "connected": agent.connected,
                })
            })
            .collect();
        json!({ "agents": agents })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(session: &str) -> decade_channel::Agent {
        decade_channel::Agent {
            name: "coax".into(),
            version: "0.4.0".into(),
            session: session.into(),
        }
    }

    fn state(state: AgentState) -> AgentUpdate {
        AgentUpdate {
            state: Some(Some(state)),
            ..AgentUpdate::default()
        }
    }

    #[test]
    fn a_disconnected_agent_is_dropped_ten_minutes_after_it_was_last_seen() {
        let start = Instant::now();
        let mut agents = Agents::default();
        agents.attach(identity("s1"), Some("chat".into()), start);
        agents.attach(identity("s2"), Some("code".into()), start);
        agents.detach("s1", start);
        assert!(!agents.expire(start + Duration::from_secs(599)));
        assert_eq!(agents.list.len(), 2);
        assert!(agents.expire(start + Duration::from_secs(600)));
        assert_eq!(agents.list.len(), 1);
        assert_eq!(agents.list[0].session, "s2");
        assert!(!agents.expire(start + Duration::from_secs(6000)));
    }

    #[test]
    fn the_history_keeps_the_last_32_state_changes() {
        let start = Instant::now();
        let mut agents = Agents::default();
        agents.attach(identity("s1"), Some("chat".into()), start);
        let agent = agents.get_mut("s1").unwrap();
        for second in 0..40 {
            let next = [AgentState::Working, AgentState::Idle][second % 2];
            agent.update(state(next), start + Duration::from_secs(second as u64));
        }
        assert_eq!(agent.history.len(), 32);
        assert_eq!(
            agent.history.front(),
            Some(&(Some(AgentState::Working), start + Duration::from_secs(8)))
        );
        assert_eq!(
            agent.history.back(),
            Some(&(Some(AgentState::Idle), start + Duration::from_secs(39)))
        );
    }

    #[test]
    fn clearing_returns_done_and_attention_to_idle_and_records_the_change() {
        let start = Instant::now();
        let later = start + Duration::from_secs(5);
        let mut agents = Agents::default();
        for (session, reported) in [
            ("s1", AgentState::Attention),
            ("s2", AgentState::Done),
            ("s3", AgentState::Working),
        ] {
            agents.attach(identity(session), None, start);
            agents
                .get_mut(session)
                .unwrap()
                .update(state(reported), start);
        }
        agents.clear(later);
        let states: Vec<_> = (agents.list.iter())
            .map(|agent| agent.report.state)
            .collect();
        assert_eq!(
            states,
            [
                Some(AgentState::Idle),
                Some(AgentState::Idle),
                Some(AgentState::Working)
            ]
        );
        assert_eq!(agents.list[0].since, later);
        assert_eq!(agents.list[0].history.len(), 2);
        assert_eq!(agents.list[2].since, start);
    }

    #[test]
    fn a_done_agent_goes_idle_ten_minutes_after_its_workspace_is_first_shown() {
        let start = Instant::now();
        let minutes = |n: u64| start + Duration::from_secs(n * 60);
        let mut agents = Agents::default();
        for (session, reported) in [("s1", AgentState::Done), ("s2", AgentState::Attention)] {
            agents.attach(identity(session), None, start);
            agents
                .get_mut(session)
                .unwrap()
                .update(state(reported), start);
        }
        assert!(!agents.settle(minutes(60)));
        agents.shown(minutes(60));
        agents.shown(minutes(65));
        assert!(!agents.settle(minutes(69)));
        assert!(agents.settle(minutes(70)));
        let states: Vec<_> = (agents.list.iter())
            .map(|agent| agent.report.state)
            .collect();
        assert_eq!(
            states,
            [Some(AgentState::Idle), Some(AgentState::Attention)]
        );
        assert_eq!(agents.list[0].since, minutes(70));
        agents.shown(minutes(80));
        assert!(!agents.settle(minutes(200)));
        assert_eq!(agents.list[1].report.state, Some(AgentState::Attention));
    }

    #[test]
    fn a_report_from_a_done_agent_stops_its_clock_until_the_workspace_is_shown_again() {
        let start = Instant::now();
        let minutes = |n: u64| start + Duration::from_secs(n * 60);
        let mut agents = Agents::default();
        agents.attach(identity("s1"), None, start);
        let agent = agents.get_mut("s1").unwrap();
        agent.update(state(AgentState::Done), start);
        agents.shown(start);
        let activity = AgentUpdate {
            activity: Some(Some("summing up".into())),
            ..AgentUpdate::default()
        };
        agents.get_mut("s1").unwrap().update(activity, minutes(5));
        assert!(!agents.settle(minutes(30)));
        agents.shown(minutes(30));
        assert!(!agents.settle(minutes(39)));
        assert!(agents.settle(minutes(40)));
    }

    #[test]
    fn a_new_agent_in_a_pane_replaces_a_disconnected_one_there() {
        let start = Instant::now();
        let mut agents = Agents::default();
        agents.attach(identity("old"), Some("chat".into()), start);
        agents.attach(identity("other"), Some("code".into()), start);
        agents.detach("old", start);
        agents.detach("other", start);
        agents.attach(identity("new"), Some("chat".into()), start);
        let sessions: Vec<_> = agents.list.iter().map(|a| a.session.as_str()).collect();
        assert_eq!(sessions, ["other", "new"]);
    }

    #[test]
    fn the_listing_gives_each_agents_report_and_the_seconds_its_state_has_held() {
        let start = Instant::now();
        let mut agents = Agents::default();
        agents.attach(identity("s1"), Some("chat".into()), start);
        agents.get_mut("s1").unwrap().update(
            AgentUpdate {
                state: Some(Some(AgentState::Attention)),
                question: Some(Some("which branch?".into())),
                model: Some(Some("opus".into())),
                context: Some(Some(Context {
                    used: 10,
                    limit: 100,
                })),
                tokens: Some(Some(Tokens {
                    input: 5,
                    output: 7,
                })),
                cost: Some(Some(0.25)),
                ..AgentUpdate::default()
            },
            start,
        );
        agents.detach("s1", start);
        assert_eq!(
            agents.listing(start + Duration::from_secs(90)),
            json!({"agents": [{
                "name": "coax",
                "pane": "chat",
                "state": "attention",
                "age": 90,
                "question": "which branch?",
                "activity": null,
                "model": "opus",
                "context": {"used": 10, "limit": 100},
                "tokens": {"input": 5, "output": 7},
                "cost": 0.25,
                "connected": false
            }]})
        );
    }
}
