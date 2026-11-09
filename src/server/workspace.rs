use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::RegistrationToken;

use crate::server::channel::{Out, Router};
use crate::server::panes::{CellRect, Layout, Pane};
use crate::server::scratch::Scratch;
use decade_channel::AgentState;
use serde_json::Value;

pub struct Workspace {
    pub id: u32,
    pub name: String,
    pub cwd: PathBuf,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub router: Router,

    /// The event source accepting connections on this workspace's socket.
    pub listener: Option<RegistrationToken>,
    pub scratch: Option<Scratch>,

    /// The git branch checked out at `cwd`, as last read.
    pub branch: Option<String>,
    branch_read: Option<Instant>,

    /// The pane the workspace's agent points at.
    pub point: Option<Point>,
}

/// An agent pointing at a pane, from the MCP tool `point`.
pub struct Point {
    pub pane: String,
    pub note: Option<String>,
    pub since: Instant,
}

/// One split tree of panes in a workspace.
pub struct Tab {
    pub layout: Layout,

    /// Whether the tab goes once its last client has.
    pub closing: bool,
}

/// The name a pane sees in `from` on a verb sent over MCP.
const AGENT: &str = "agent";

const BRANCH_EVERY: Duration = Duration::from_secs(2);

/// The time from one peak of a point's pulse to the next.
const PULSE_PERIOD: Duration = Duration::from_secs(1);

/// How long a point lasts unless something ends it sooner.
const POINT_FOR: Duration = Duration::from_secs(30);

/// How far a point's pulse is toward its peak `elapsed` after it began,
/// from 0 to 1.
pub fn pulse(elapsed: Duration) -> f32 {
    let phase = elapsed.as_secs_f32() / PULSE_PERIOD.as_secs_f32();
    (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0
}

pub fn expired(elapsed: Duration) -> bool {
    elapsed >= POINT_FOR
}

impl Point {
    pub fn pulse(&self, now: Instant) -> f32 {
        pulse(now.saturating_duration_since(self.since))
    }
}

fn pane_names(tabs: &[Tab]) -> Vec<&str> {
    (tabs.iter())
        .flat_map(|tab| &tab.layout.panes)
        .map(|pane| pane.name.as_str())
        .collect()
}

/// The app id prefix of a terminal started for one of the workspace's panes.
pub fn app_id_prefix(workspace: u32) -> String {
    format!("decade.w{workspace}.")
}

/// The branch checked out in the git repository holding `cwd`, or the
/// first 7 characters of the commit when HEAD is detached.
pub fn branch(cwd: &Path) -> Option<String> {
    let dot_git = cwd
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|git| git.exists())?;
    let git_dir = if dot_git.is_file() {
        let link = std::fs::read_to_string(&dot_git).ok()?;
        let target = link.strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(target)
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            Some(name.to_string())
        }
        None => Some(head.chars().take(7).collect()),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `base` if `free` accepts it, or else `base`, `separator` and the first
/// count from 2 that `free` accepts.
pub fn unique_name(base: &str, separator: &str, free: impl Fn(&str) -> bool) -> String {
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}{separator}{n}"))
        .find(|name| free(name))
        .unwrap()
}

impl Workspace {
    pub fn new(id: u32, cwd: PathBuf, area: CellRect) -> Workspace {
        let name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.display().to_string());
        let mut layout = Layout::new(area, "terminal".to_string());
        layout.panes[0].cwd = Some(cwd.clone());
        Workspace::with_tabs(id, name, cwd, vec![layout], 0)
    }

    pub fn with_tabs(
        id: u32,
        name: String,
        cwd: PathBuf,
        layouts: Vec<Layout>,
        active_tab: usize,
    ) -> Workspace {
        let tabs: Vec<Tab> = layouts
            .into_iter()
            .map(|layout| Tab {
                layout,
                closing: false,
            })
            .collect();
        Workspace {
            id,
            name,
            cwd,
            active_tab: active_tab.min(tabs.len() - 1),
            tabs,
            router: Router::default(),
            listener: None,
            scratch: None,
            branch: None,
            branch_read: None,
            point: None,
        }
    }

    /// Read the branch again if it was last read `BRANCH_EVERY` or more
    /// before `now`.
    pub fn refresh_branch(&mut self, now: Instant) {
        if self
            .branch_read
            .is_some_and(|read| now.duration_since(read) < BRANCH_EVERY)
        {
            return;
        }
        self.branch = branch(&self.cwd);
        self.branch_read = Some(now);
    }

    /// Read again what runs in each pane of the active tab and in each
    /// other tab's focused pane, where it was last read long enough
    /// before `now`.
    pub fn refresh_running(&mut self, now: Instant, home: &Path) {
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let focused = tab.layout.focused;
            let active = index == self.active_tab;
            for pane in &mut tab.layout.panes {
                if active || pane.id == focused {
                    pane.refresh_running(now, home);
                }
            }
        }
    }

    /// Answer one line from socket client `id`.
    pub fn channel_line(&mut self, id: usize, line: &str) -> Vec<Out> {
        self.router.handle(id, line, &pane_names(&self.tabs))
    }

    /// The id of the client in pane `to` and the line carrying it a pane
    /// verb from the agent, or the reason it cannot go.
    pub fn agent_send(
        &self,
        to: &str,
        verb: String,
        payload: Value,
    ) -> Result<(usize, String), String> {
        let panes = pane_names(&self.tabs);
        self.router.send(AGENT, to, verb, payload, &panes)
    }

    /// The pane an agent's show, highlight or clear of `kind` goes to.
    pub fn showing(&self, kind: &str, pane: Option<&str>) -> Result<String, String> {
        self.router.showing(kind, pane, &pane_names(&self.tabs))
    }

    /// Every pane in tab order and the name of the client occupying it.
    pub fn pane_list(&self) -> Vec<decade_channel::Pane> {
        self.router.panes(&pane_names(&self.tabs))
    }

    /// The layout of the active tab.
    pub fn layout(&self) -> &Layout {
        &self.tabs[self.active_tab].layout
    }

    pub fn layout_mut(&mut self) -> &mut Layout {
        &mut self.tabs[self.active_tab].layout
    }

    /// The panes of every tab.
    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.tabs.iter().flat_map(|tab| &tab.layout.panes)
    }

    /// The tab and id of the pane with this name.
    pub fn by_name(&self, name: &str) -> Option<(usize, usize)> {
        self.tabs
            .iter()
            .enumerate()
            .find_map(|(index, tab)| Some((index, tab.layout.by_name(name)?)))
    }

    /// The base name, or the base name with the first free count after it,
    /// free across every tab.
    fn unique_pane_name(&self, base: &str) -> String {
        unique_name(base, "", |name| self.by_name(name).is_none())
    }

    /// Add an empty pane named after `base` beside the focused pane of tab
    /// `tab`, and return its id.
    pub fn add_pane(&mut self, tab: usize, base: &str, cwd: Option<PathBuf>) -> usize {
        let name = self.unique_pane_name(base);
        let layout = &mut self.tabs[tab].layout;
        let id = layout.add_transient(name);
        if let Some(pane) = layout.pane_mut(id) {
            pane.cwd = cwd;
        }
        id
    }

    /// Add a tab holding one terminal in the workspace's cwd, and return
    /// its index and the terminal's name.
    pub fn add_tab(&mut self, area: CellRect) -> (usize, String) {
        let name = self.unique_pane_name("terminal");
        let mut layout = Layout::new(area, name.clone());
        layout.panes[0].cwd = Some(self.cwd.clone());
        self.tabs.push(Tab {
            layout,
            closing: false,
        });
        (self.tabs.len() - 1, name)
    }

    /// Whether every tab goes once its last client has.
    pub fn closing(&self) -> bool {
        self.tabs.iter().all(|tab| tab.closing)
    }

    /// The closing tabs that no client is left in.
    pub fn closed_tabs(&self) -> impl Iterator<Item = usize> + '_ {
        self.tabs.iter().enumerate().filter_map(|(index, tab)| {
            let clients = tab.layout.panes.iter().any(|pane| !pane.clients.is_empty());
            (tab.closing && !clients).then_some(index)
        })
    }

    /// Remove a tab and its panes, keeping the active tab where it was
    /// unless it was the one removed. Return whether it was.
    pub fn remove_tab(&mut self, tab: usize) -> bool {
        let removed = self.tabs.remove(tab);
        for pane in &removed.layout.panes {
            self.forget_pane(&pane.name);
        }
        let was_active = self.active_tab == tab;
        if self.active_tab > tab || self.active_tab == self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        was_active
    }

    /// Drop what the workspace keeps about a pane that has gone: its
    /// client's claim, its agent state, its scratch mark and a point at it.
    pub fn forget_pane(&mut self, name: &str) {
        self.router.release(name);
        if self.scratch_pane() == Some(name) {
            self.scratch = None;
        }
        self.end_point_at(name);
    }

    /// Point at pane `pane` from `now`, in place of any earlier point.
    pub fn point_at(
        &mut self,
        pane: &str,
        note: Option<String>,
        now: Instant,
    ) -> Result<(), String> {
        if self.by_name(pane).is_none() {
            return Err(format!("no pane named {pane}"));
        }
        self.point = Some(Point {
            pane: pane.to_string(),
            note,
            since: now,
        });
        Ok(())
    }

    /// End the point if it is at pane `pane`.
    pub fn end_point_at(&mut self, pane: &str) {
        if self.point.as_ref().is_some_and(|point| point.pane == pane) {
            self.point = None;
        }
    }

    /// End the point if it has expired by `now`, and return whether one is
    /// left.
    pub fn keep_point(&mut self, now: Instant) -> bool {
        if (self.point.as_ref())
            .is_some_and(|point| expired(now.saturating_duration_since(point.since)))
        {
            self.point = None;
        }
        self.point.is_some()
    }

    /// Whether the pane stays when it has no client: it is pinned or it is
    /// the scratch pane.
    pub fn keeps_empty(&self, pane: &Pane) -> bool {
        pane.pinned || self.is_scratch(pane)
    }

    /// Whether tab `tab` has nothing left in it and closes: no pane has a
    /// client or a running program, and none is kept empty. The
    /// workspace's only tab never does.
    pub fn tab_is_spent(&self, tab: usize, mut running: impl FnMut(u32) -> bool) -> bool {
        self.tabs.len() > 1
            && self.tabs[tab].layout.panes.iter().all(|pane| {
                pane.clients.is_empty()
                    && !self.keeps_empty(pane)
                    && !pane.pid.is_some_and(&mut running)
            })
    }

    pub fn is_scratch(&self, pane: &Pane) -> bool {
        self.scratch_pane() == Some(pane.name.as_str())
    }

    /// The most urgent state of the workspace's agents, or `None` when no
    /// agent has reported.
    pub fn agent_state(&self) -> Option<AgentState> {
        self.router.statuses().map(|status| status.state).max()
    }

    /// The name of the scratch pane, if the workspace has one.
    pub fn scratch_pane(&self) -> Option<&str> {
        self.scratch.as_ref().map(|scratch| scratch.pane.as_str())
    }

    /// Mark the pane as the scratch pane, or unmark it if it is already.
    pub fn toggle_scratch(&mut self, name: &str) {
        self.scratch = if self.scratch_pane() == Some(name) {
            None
        } else {
            Some(Scratch::new(name.to_string()))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_home() -> Workspace {
        Workspace::new(
            0,
            PathBuf::from("/home/jack"),
            crate::server::panes::tests::AREA,
        )
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("decade-branch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_branch_is_read_from_head_in_the_cwd_or_a_directory_above_it() {
        let repo = fresh_dir("plain");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/server")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/dev\n").unwrap();
        assert_eq!(branch(&repo).as_deref(), Some("dev"));
        assert_eq!(branch(&repo.join("src/server")).as_deref(), Some("dev"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn a_git_file_points_at_the_directory_holding_head() {
        let root = fresh_dir("worktree");
        let git_dir = root.join("repo/.git/worktrees/feature");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::create_dir_all(root.join("feature")).unwrap();
        let link = "gitdir: ../repo/.git/worktrees/feature\n";
        std::fs::write(root.join("feature/.git"), link).unwrap();
        assert_eq!(branch(&root.join("feature")).as_deref(), Some("feature"));

        let absolute = format!("gitdir: {}\n", git_dir.display());
        std::fs::write(root.join("feature/.git"), absolute).unwrap();
        assert_eq!(branch(&root.join("feature")).as_deref(), Some("feature"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_detached_head_shows_seven_characters_of_its_commit() {
        let repo = fresh_dir("detached");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let commit = "b142ad7c0ffee0000000000000000000000000000\n";
        std::fs::write(repo.join(".git/HEAD"), commit).unwrap();
        assert_eq!(branch(&repo).as_deref(), Some("b142ad7"));
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn a_directory_outside_any_repository_has_no_branch() {
        let dir = fresh_dir("none");
        if dir.ancestors().all(|above| !above.join(".git").exists()) {
            assert_eq!(branch(&dir), None);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_taken_name_gets_the_first_free_count() {
        let free_of = |taken: &'static [&str]| move |name: &str| !taken.contains(&name);
        assert_eq!(unique_name("decade", " ", free_of(&["foam"])), "decade");
        assert_eq!(unique_name("decade", " ", free_of(&["decade"])), "decade 2");
        let taken = &["decade", "decade 2", "decade 3"];
        assert_eq!(unique_name("decade", " ", free_of(taken)), "decade 4");
    }

    #[test]
    fn a_workspace_is_named_after_its_directory() {
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack/projects/decade"),
            crate::server::panes::tests::AREA,
        );
        assert_eq!(workspace.name, "decade");
    }

    #[test]
    fn a_workspace_at_the_root_is_named_after_the_path() {
        let workspace = Workspace::new(0, PathBuf::from("/"), crate::server::panes::tests::AREA);
        assert_eq!(workspace.name, "/");
    }

    #[test]
    fn a_new_workspace_holds_one_terminal_pane() {
        let workspace = Workspace::new(
            0,
            PathBuf::from("/home/jack"),
            crate::server::panes::tests::AREA,
        );
        let names: Vec<&str> = workspace
            .layout()
            .panes
            .iter()
            .map(|pane| pane.name.as_str())
            .collect();
        assert_eq!(names, ["terminal"]);
    }

    #[test]
    fn pane_names_stay_unique_across_tabs() {
        let mut workspace = at_home();
        let (second, name) = workspace.add_tab(crate::server::panes::tests::AREA);
        assert_eq!((second, name.as_str()), (1, "terminal2"));
        let (_, name) = workspace.add_tab(crate::server::panes::tests::AREA);
        assert_eq!(name, "terminal3");
        assert_eq!(workspace.unique_pane_name("terminal"), "terminal4");
        assert_eq!(workspace.unique_pane_name("nautilus"), "nautilus");
        assert_eq!(workspace.by_name("terminal2"), Some((1, 0)));
        let cwd = workspace.tabs[1].layout.panes[0].cwd.as_deref();
        assert_eq!(cwd, Some(std::path::Path::new("/home/jack")));
    }

    #[test]
    fn an_added_pane_takes_a_free_name() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        let id = workspace.add_pane(0, "terminal", Some(PathBuf::from("/tmp")));
        let pane = workspace.tabs[0].layout.pane(id).unwrap();
        assert_eq!(pane.name, "terminal3");
        assert_eq!(pane.cwd.as_deref(), Some(std::path::Path::new("/tmp")));
        assert_eq!(workspace.tabs[0].layout.focused, id);
    }

    #[test]
    fn an_empty_pane_is_kept_while_it_is_pinned_or_the_scratch_pane() {
        let mut workspace = at_home();
        assert!(!workspace.keeps_empty(&workspace.layout().panes[0]));
        workspace.toggle_scratch("terminal");
        assert!(workspace.keeps_empty(&workspace.layout().panes[0]));
        workspace.toggle_scratch("terminal");
        workspace.layout_mut().pin(0, "decade.terminal".to_string());
        assert!(workspace.keeps_empty(&workspace.layout().panes[0]));
    }

    #[test]
    fn a_tab_with_no_client_running_program_or_kept_pane_is_spent_unless_it_is_the_only_tab() {
        let mut workspace = at_home();
        assert!(!workspace.tab_is_spent(0, |_| false));
        workspace.add_tab(crate::server::panes::tests::AREA);
        assert!(workspace.tab_is_spent(1, |_| false));
        workspace.tabs[1].layout.panes[0].pid = Some(7);
        assert!(!workspace.tab_is_spent(1, |pid| pid == 7));
        assert!(workspace.tab_is_spent(1, |_| false));
        let name = workspace.tabs[1].layout.panes[0].name.clone();
        workspace.toggle_scratch(&name);
        assert!(!workspace.tab_is_spent(1, |_| false));
        workspace.toggle_scratch(&name);
        let id = workspace.tabs[1].layout.panes[0].id;
        workspace.tabs[1]
            .layout
            .pin(id, "decade.terminal".to_string());
        assert!(!workspace.tab_is_spent(1, |_| false));
    }

    #[test]
    fn a_closing_tab_with_no_client_is_closed_and_the_workspace_closes_with_its_last_tab() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        assert_eq!(workspace.closed_tabs().count(), 0);
        workspace.tabs[1].closing = true;
        assert_eq!(workspace.closed_tabs().collect::<Vec<_>>(), [1]);
        assert!(!workspace.closing());
        workspace.tabs[0].closing = true;
        assert!(workspace.closing());
    }

    #[test]
    fn removing_the_active_last_tab_makes_the_one_before_it_active() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.active_tab = 2;
        assert!(workspace.remove_tab(2));
        assert_eq!(workspace.active_tab, 1);
        assert!(!workspace.remove_tab(0));
        assert_eq!(workspace.active_tab, 0);
        assert_eq!(workspace.by_name("terminal3"), None);
        assert_eq!(workspace.by_name("terminal2"), Some((0, 0)));
    }

    #[test]
    fn removing_the_tab_that_holds_the_scratch_pane_leaves_no_scratch_pane() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.toggle_scratch("terminal2");
        workspace.remove_tab(1);
        assert_eq!(workspace.scratch_pane(), None);
    }

    #[test]
    fn marking_a_pane_scratch_unmarks_the_one_before_it() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.toggle_scratch("terminal");
        assert_eq!(workspace.scratch_pane(), Some("terminal"));
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_pane(), Some("terminal2"));
        workspace.toggle_scratch("terminal2");
        assert_eq!(workspace.scratch_pane(), None);
    }

    #[test]
    fn a_pane_that_goes_takes_its_clients_claim_and_agent_state_with_it() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(1, r#"{"hello": {"name": "coax", "pane": "terminal2"}}"#);
        workspace.channel_line(1, r#"{"status": {"state": "working"}}"#);
        assert_eq!(workspace.agent_state(), Some(AgentState::Working));
        workspace.remove_tab(1);
        assert_eq!(workspace.agent_state(), None);
        assert!(workspace.router.occupant("terminal2").is_none());
    }

    #[test]
    fn a_verb_from_the_agent_reaches_the_pane_occupant_from_agent() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(
            4,
            r#"{"hello": {"name": "code-pane", "pane": "terminal2", "shows": [{"kind": "file"}]}}"#,
        );
        let to = workspace.showing("file", None).unwrap();
        assert_eq!(to, "terminal2");
        let show = serde_json::json!({"kind": "file", "target": "a.rs"});
        let (client, line) = workspace.agent_send(&to, "show".into(), show).unwrap();
        assert_eq!(client, 4);
        assert_eq!(
            line,
            r#"{"from":"agent","show":{"kind":"file","target":"a.rs"}}"#
        );
        let refused = workspace.agent_send("terminal", "show".into(), Value::Null);
        assert_eq!(refused, Err("no client occupies pane terminal".to_string()));
        let panes: Vec<_> = (workspace.pane_list().into_iter())
            .map(|pane| (pane.name, pane.client))
            .collect();
        let occupant = Some("code-pane".to_string());
        assert_eq!(
            panes,
            [("terminal".into(), None), ("terminal2".into(), occupant)]
        );
    }

    #[test]
    fn a_pulse_rises_from_nothing_to_its_peak_and_back_each_period() {
        let at = |ms| pulse(Duration::from_millis(ms));
        assert!(at(0) < 1e-6);
        assert!((at(250) - 0.5).abs() < 1e-3);
        assert!((at(500) - 1.0).abs() < 1e-6);
        assert!((at(750) - 0.5).abs() < 1e-3);
        assert!(at(1000) < 1e-6);
        assert!(at(100) < at(200) && at(600) > at(700));
    }

    #[test]
    fn a_point_expires_after_thirty_seconds() {
        assert!(!expired(Duration::from_millis(29_999)));
        assert!(expired(Duration::from_secs(30)));
        let mut workspace = at_home();
        let start = Instant::now();
        workspace.point_at("terminal", None, start).unwrap();
        assert!(workspace.keep_point(start + Duration::from_secs(29)));
        assert!(!workspace.keep_point(start + Duration::from_secs(30)));
        assert!(workspace.point.is_none());
    }

    #[test]
    fn a_new_point_replaces_the_one_before_and_an_unknown_pane_is_refused() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        let now = Instant::now();
        workspace.point_at("terminal", None, now).unwrap();
        let note = Some("this one".to_string());
        workspace.point_at("terminal2", note.clone(), now).unwrap();
        let point = workspace.point.as_ref().unwrap();
        assert_eq!((point.pane.as_str(), &point.note), ("terminal2", &note));
        let refused = workspace.point_at("chat", None, now);
        assert_eq!(refused, Err("no pane named chat".to_string()));
        assert_eq!(workspace.point.as_ref().unwrap().pane, "terminal2");
    }

    #[test]
    fn a_point_ends_when_its_pane_is_focused_or_goes_and_not_for_another_pane() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        workspace.end_point_at("terminal");
        assert!(workspace.point.is_some());
        workspace.end_point_at("terminal2");
        assert!(workspace.point.is_none());
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        workspace.remove_tab(1);
        assert!(workspace.point.is_none());
    }
}
