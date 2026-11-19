use std::path::Path;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    ToplevelSurface, XdgToplevelSurfaceData, XdgToplevelSurfaceRoleAttributes,
};

use crate::server::proc;

const RUNNING_EVERY: Duration = Duration::from_secs(2);

/// One window in a pane, shown as one of the pane's tabs.
pub struct Client {
    pub toplevel: ToplevelSurface,
    pub app_id: Option<String>,
    pub pid: Option<u32>,

    /// Whether decade started the client as a terminal tab of the pane.
    pub terminal_tab: bool,

    /// What the client runs, as last read, and when it was read.
    running: Option<(Instant, Option<String>)>,
}

impl Client {
    pub fn new(
        toplevel: ToplevelSurface,
        app_id: Option<String>,
        pid: Option<u32>,
        terminal_tab: bool,
    ) -> Client {
        Client {
            toplevel,
            app_id,
            pid,
            terminal_tab,
            running: None,
        }
    }

    /// The client's pid, when it is a ghostty decade started for the
    /// pane whose ghostty is `pane_pid`.
    fn terminal(&self, pane_pid: Option<u32>) -> Option<u32> {
        let pid = self.pid?;
        (self.terminal_tab || pane_pid == Some(pid)).then_some(pid)
    }

    /// Read what the client runs again if it is a terminal and it was
    /// last read `RUNNING_EVERY` or more before `now`.
    pub fn refresh_running(&mut self, pane_pid: Option<u32>, now: Instant, home: &Path) {
        let Some(ghostty) = self.terminal(pane_pid) else {
            return;
        };
        if let Some((read, _)) = self.running
            && now.duration_since(read) < RUNNING_EVERY
        {
            return;
        }
        self.running = Some((now, running(ghostty, home)));
    }

    /// What the client runs: a terminal's program as last read, or the
    /// last part of another client's app id.
    pub fn running_label(&self, pane_pid: Option<u32>) -> Option<String> {
        if self.terminal(pane_pid).is_some() {
            return self.running.as_ref()?.1.clone();
        }
        let app_id = self.app_id.as_deref()?;
        Some(app_id.rsplit('.').next().unwrap_or(app_id).to_string())
    }

    /// The client's title, or the last part of its app id.
    pub fn label(&self) -> Option<String> {
        let title = toplevel_data(self.toplevel.wl_surface(), |data| data.title.clone());
        title.filter(|title| !title.is_empty()).or_else(|| {
            let app_id = self.app_id.as_deref()?;
            Some(app_id.rsplit('.').next().unwrap_or(app_id).to_string())
        })
    }
}

/// The app id a toplevel's client set.
pub fn app_id(surface: &WlSurface) -> Option<String> {
    toplevel_data(surface, |data| data.app_id.clone())
}

fn toplevel_data(
    surface: &WlSurface,
    read: impl FnOnce(&XdgToplevelSurfaceRoleAttributes) -> Option<String>,
) -> Option<String> {
    with_states(surface, |states| {
        let data = states.data_map.get::<XdgToplevelSurfaceData>()?;
        read(&data.lock().unwrap())
    })
}

/// What a terminal decade started runs: its shell's foreground program,
/// or at the prompt the last directory of the shell's cwd.
fn running(ghostty: u32, home: &Path) -> Option<String> {
    let shell = proc::first_child(ghostty)?;
    match proc::foreground_group(shell) {
        Some(group) => proc::comm(group),
        None => Some(last_directory(&proc::cwd(shell)?, home)),
    }
}

/// The last directory of `cwd`, or `~` for `home`.
fn last_directory(cwd: &Path, home: &Path) -> String {
    if cwd == home {
        return "~".to_string();
    }
    cwd.file_name().map_or_else(
        || "/".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_at_its_prompt_is_named_by_the_last_directory_of_its_shell() {
        let home = Path::new("/home/jack");
        let found =
            proc::tests::under_shell("cd /usr && setsid sleep 5; true", |sh| running(sh, home));
        assert_eq!(found.as_deref(), Some("usr"));
        assert_eq!(last_directory(home, home), "~");
        assert_eq!(last_directory(Path::new("/"), home), "/");
        assert_eq!(
            last_directory(&home.join("projects/decade"), home),
            "decade"
        );
    }
}
