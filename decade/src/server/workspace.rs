use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::RegistrationToken;

use crate::server::channel::{Out, PaneNames, Router};
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

    /// The id of the sidebar group the workspace is in.
    pub group: Option<u32>,

    /// The pane whose screenshot was taken last, and when, while its
    /// screenshot tag flashes.
    pub flash: Option<(String, Instant)>,
}

/// A named set of workspaces, which only the sidebar shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: u32,
    pub name: String,

    /// Whether the open sidebar shows only the group's header.
    pub folded: bool,
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

    /// The name the person gave the tab, shown in place of its label.
    pub name: Option<String>,

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

/// How long a pane's screenshot tag flashes after a capture.
const FLASH_FOR: Duration = Duration::from_millis(700);

/// How opaque the white layer over a captured pane starts.
const SHUTTER_ALPHA: f32 = 0.3;

/// How long the white layer over a captured pane takes to fade out.
const SHUTTER_FOR: Duration = Duration::from_millis(300);

/// The opacity of the white layer over a captured pane `elapsed` after
/// the capture, easing out from `SHUTTER_ALPHA` to 0.
pub fn shutter(elapsed: Duration) -> f32 {
    let left = (1.0 - elapsed.as_secs_f32() / SHUTTER_FOR.as_secs_f32()).max(0.0);
    SHUTTER_ALPHA * left * left
}

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

fn pane_names(tabs: &[Tab]) -> PaneNames<'_> {
    let panes = || tabs.iter().flat_map(|tab| &tab.layout.panes);
    PaneNames {
        current: panes().map(|pane| pane.name.as_str()).collect(),
        former: panes()
            .flat_map(|pane| {
                (pane.aliases.iter()).map(|alias| (alias.as_str(), pane.name.as_str()))
            })
            .collect(),
    }
}

/// Refuse an empty name, and one that `taken` says something else holds.
pub fn check_name(name: &str, taken: bool) -> Result<(), String> {
    if name.is_empty() {
        return Err("a name cannot be empty".to_string());
    }
    if taken {
        return Err(format!("{name} is taken"));
    }
    Ok(())
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
                name: None,
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
            group: None,
            flash: None,
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

    /// Give each pane the name of the live agent in it, or `None`.
    pub fn name_agents(&mut self) {
        let agents = &self.router.agents.list;
        for pane in self.tabs.iter_mut().flat_map(|tab| &mut tab.layout.panes) {
            pane.agent = (agents.iter())
                .find(|agent| agent.live() && agent.pane.as_deref() == Some(pane.name.as_str()))
                .map(|agent| agent.name.clone());
        }
    }

    /// Answer one line from socket client `id`.
    pub fn channel_line(&mut self, id: usize, line: &str) -> Vec<Out> {
        let scratch = self.scratch.as_ref().map(|scratch| scratch.pane.as_str());
        self.router
            .handle(id, line, &pane_names(&self.tabs), scratch)
    }

    /// The id of the client in pane `to` and the line carrying it a pane
    /// verb from the agent, or the reason it cannot go.
    pub fn agent_send(
        &self,
        to: &str,
        verb: String,
        payload: Value,
    ) -> Result<(usize, String), String> {
        self.router
            .send(AGENT, to, verb, payload, &pane_names(&self.tabs))
    }

    /// The pane an agent's show, highlight or clear of `kind` goes to.
    pub fn showing(&self, kind: &str, pane: Option<&str>) -> Result<String, String> {
        self.router.showing(kind, pane, &pane_names(&self.tabs))
    }

    /// Every pane in tab order and the name of the client occupying it,
    /// marking the scratch pane and the pane `you`.
    pub fn pane_list(&self, you: Option<&str>) -> Vec<decade_channel::Pane> {
        let names = pane_names(&self.tabs);
        let you = you.and_then(|you| names.resolve(you));
        self.router.panes(&names, self.scratch_pane(), you)
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
            name: None,
            closing: false,
        });
        (self.tabs.len() - 1, name)
    }

    /// Rename pane `id` of tab `tab` to `name`, which no other pane may
    /// answer to. Its old name stays an alias, and what the workspace keeps
    /// by pane name follows it.
    pub fn rename_pane(&mut self, tab: usize, id: usize, name: &str) -> Result<(), String> {
        let old = (self.tabs.get(tab))
            .and_then(|entry| entry.layout.pane(id))
            .map(|pane| pane.name.clone())
            .ok_or("the pane has gone")?;
        if name == old {
            return Ok(());
        }
        if name.contains(['&', '=', ' ']) {
            return Err("a pane name holds no &, = or space".to_string());
        }
        let taken = self.by_name(name).is_some_and(|found| found != (tab, id));
        check_name(name, taken)?;
        if let Some(pane) = self.tabs[tab].layout.pane_mut(id) {
            pane.rename(name.to_string());
        }
        self.router.rename(&old, name);
        if let Some(scratch) = &mut self.scratch
            && scratch.pane == old
        {
            scratch.pane = name.to_string();
        }
        if let Some(point) = &mut self.point
            && point.pane == old
        {
            point.pane = name.to_string();
        }
        Ok(())
    }

    /// Name tab `tab` `name`, which no other tab may hold.
    pub fn rename_tab(&mut self, tab: usize, name: &str) -> Result<(), String> {
        if tab >= self.tabs.len() {
            return Err("the tab has gone".to_string());
        }
        let taken = (self.tabs.iter().enumerate())
            .any(|(index, other)| index != tab && other.name.as_deref() == Some(name));
        check_name(name, taken)?;
        self.tabs[tab].name = Some(name.to_string());
        Ok(())
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
    /// client's and agent's claim, its scratch mark and a point at it.
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
        let Some((tab, id)) = self.by_name(pane) else {
            return Err(format!("no pane named {pane}"));
        };
        let pane = &self.tabs[tab].layout.pane(id).unwrap().name;
        self.point = Some(Point {
            pane: pane.clone(),
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

    /// The pane whose screenshot tag flashes at `now`, and how bright it
    /// is, fading from 1 to 0 over `FLASH_FOR`.
    pub fn flash_at(&self, now: Instant) -> Option<(&str, f32)> {
        let (pane, since) = self.flash.as_ref()?;
        let elapsed = now.saturating_duration_since(*since);
        let left = 1.0 - elapsed.as_secs_f32() / FLASH_FOR.as_secs_f32();
        (left > 0.0).then_some((pane.as_str(), left))
    }

    /// The pane under the white layer of a capture at `now`, and the
    /// layer's opacity.
    pub fn shutter_at(&self, now: Instant) -> Option<(&str, f32)> {
        let (pane, since) = self.flash.as_ref()?;
        let alpha = shutter(now.saturating_duration_since(*since));
        (alpha > 0.0).then_some((pane.as_str(), alpha))
    }

    /// End the flash if it has faded by `now`, and return whether one is
    /// left.
    pub fn keep_flash(&mut self, now: Instant) -> bool {
        if self.flash_at(now).is_none() {
            self.flash = None;
        }
        self.flash.is_some()
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

    /// The most urgent state of the workspace's live agents, or `None`
    /// when no live agent has reported.
    pub fn agent_state(&self) -> Option<AgentState> {
        self.router.agents.states().max()
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

    #[test]
    fn the_white_layer_of_a_capture_starts_at_its_alpha_and_eases_out_to_nothing() {
        assert_eq!(shutter(Duration::ZERO), SHUTTER_ALPHA);
        let halfway = shutter(SHUTTER_FOR / 2);
        assert!((halfway - SHUTTER_ALPHA / 4.0).abs() < 1e-6);
        assert_eq!(shutter(SHUTTER_FOR), 0.0);
        assert_eq!(shutter(SHUTTER_FOR * 2), 0.0);
    }

    #[test]
    fn a_flash_keeps_the_timer_running_until_it_has_faded() {
        let mut shown = at_home();
        let now = Instant::now();
        assert!(!shown.keep_flash(now));
        shown.flash = Some(("terminal".to_string(), now));
        assert!(shown.keep_flash(now + SHUTTER_FOR));
        assert!(shown.shutter_at(now + SHUTTER_FOR).is_none());
        assert!(shown.keep_flash(now + FLASH_FOR / 2));
        assert!(!shown.keep_flash(now + FLASH_FOR));
        assert!(shown.flash.is_none());
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
    fn a_pane_that_goes_takes_its_clients_claim_and_leaves_its_agent_without_a_pane() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(
            1,
            r#"{"hello": {"name": "coax", "pane": "terminal2", "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
        );
        workspace.channel_line(1, r#"{"agent": {"state": "working"}}"#);
        assert_eq!(workspace.agent_state(), Some(AgentState::Working));
        workspace.remove_tab(1);
        assert_eq!(workspace.agent_state(), None);
        assert_eq!(workspace.router.agents.list[0].pane, None);
        assert!(workspace.router.occupant("terminal2").is_none());
    }

    #[test]
    fn a_pane_takes_the_name_of_its_live_agent_and_loses_it_when_the_agent_disconnects() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(
            1,
            r#"{"hello": {"name": "coax", "pane": "terminal2", "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
        );
        workspace.name_agents();
        let agent =
            |workspace: &Workspace, tab: usize| workspace.tabs[tab].layout.panes[0].agent.clone();
        assert_eq!(agent(&workspace, 0), None);
        assert_eq!(agent(&workspace, 1).as_deref(), Some("coax"));
        workspace.router.disconnect(1, Instant::now());
        workspace.name_agents();
        assert_eq!(agent(&workspace, 1), None);
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
        let panes: Vec<_> = (workspace.pane_list(None).into_iter())
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

    #[test]
    fn a_renamed_pane_keeps_its_claims_and_its_old_name_still_reaches_it() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.channel_line(
            1,
            r#"{"hello": {"name": "coax", "pane": "terminal2", "accepts": ["ping"], "agent": {"name": "coax", "version": "0.4.0", "session": "s1"}}}"#,
        );
        workspace.toggle_scratch("terminal2");
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        workspace.rename_pane(1, 0, "build").unwrap();

        assert_eq!(workspace.by_name("build"), Some((1, 0)));
        assert_eq!(workspace.by_name("terminal2"), Some((1, 0)));
        assert_eq!(workspace.scratch_pane(), Some("build"));
        assert_eq!(workspace.point.as_ref().unwrap().pane, "build");
        assert_eq!(
            workspace.router.agents.list[0].pane.as_deref(),
            Some("build")
        );
        assert!(workspace.router.occupant("build").is_some());

        let (client, _) = (workspace.agent_send("terminal2", "ping".into(), Value::Null)).unwrap();
        assert_eq!(client, 1);
        let listed: Vec<_> = (workspace.pane_list(Some("terminal2")).into_iter())
            .map(|pane| (pane.name, pane.you))
            .collect();
        assert_eq!(
            listed,
            [("terminal".to_string(), false), ("build".to_string(), true)]
        );
        workspace
            .point_at("terminal2", None, Instant::now())
            .unwrap();
        assert_eq!(workspace.point.as_ref().unwrap().pane, "build");
    }

    #[test]
    fn a_pane_name_must_be_new_to_the_workspace_and_hold_no_address_characters() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.rename_pane(1, 0, "build").unwrap();
        for name in ["", "a b", "a&b", "a=b", "build"] {
            assert!(workspace.rename_pane(0, 0, name).is_err(), "{name:?}");
        }
        assert!(workspace.rename_pane(0, 0, "terminal2").is_err());
        assert_eq!(workspace.tabs[0].layout.panes[0].name, "terminal");

        // a pane may take back a name it had
        workspace.rename_pane(1, 0, "terminal2").unwrap();
        assert_eq!(workspace.tabs[1].layout.panes[0].aliases, ["build"]);
        workspace.rename_pane(1, 0, "build").unwrap();
        assert_eq!(workspace.tabs[1].layout.panes[0].aliases, ["terminal2"]);
    }

    #[test]
    fn a_new_pane_does_not_take_a_name_a_renamed_pane_answers_to() {
        let mut workspace = at_home();
        workspace.rename_pane(0, 0, "shell").unwrap();
        let id = workspace.add_pane(0, "terminal", None);
        assert_eq!(workspace.tabs[0].layout.pane(id).unwrap().name, "terminal2");
    }

    #[test]
    fn a_tab_name_must_be_new_among_the_tabs_and_not_empty() {
        let mut workspace = at_home();
        workspace.add_tab(crate::server::panes::tests::AREA);
        workspace.rename_tab(0, "work").unwrap();
        assert!(workspace.rename_tab(1, "work").is_err());
        assert!(workspace.rename_tab(1, "").is_err());
        workspace.rename_tab(0, "work").unwrap();
        workspace.rename_tab(1, "play").unwrap();
        assert_eq!(workspace.tabs[1].name.as_deref(), Some("play"));
    }
}
