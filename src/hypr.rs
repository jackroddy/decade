use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const RECOUNT: [&str; 7] = [
    "openwindow",
    "closewindow",
    "movewindow",
    "movewindowv2",
    "workspace",
    "workspacev2",
    "changefloatingmode",
];

pub struct Hyprland {
    dir: PathBuf,
}

impl Hyprland {
    pub fn find() -> Option<Hyprland> {
        let runtime = env::var_os("XDG_RUNTIME_DIR")?;
        let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
        let dir = PathBuf::from(runtime).join("hypr").join(signature);
        dir.join(".socket2.sock")
            .exists()
            .then_some(Hyprland { dir })
    }

    pub fn events(&self) -> std::io::Result<UnixStream> {
        let stream = UnixStream::connect(self.dir.join(".socket2.sock"))?;
        stream.set_nonblocking(true)?;
        Ok(stream)
    }

    pub fn alone(&self) -> Option<bool> {
        let mut stream = UnixStream::connect(self.dir.join(".socket.sock")).ok()?;
        stream.write_all(b"j/clients").ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        alone_in(&reply, std::process::id())
    }
}

pub fn wants_recount(line: &str) -> bool {
    line.split_once(">>")
        .is_some_and(|(event, _)| RECOUNT.contains(&event))
}

fn alone_in(clients: &str, pid: u32) -> Option<bool> {
    let clients: Vec<serde_json::Value> = serde_json::from_str(clients).ok()?;
    let workspace = |client: &serde_json::Value| client["workspace"]["id"].as_i64();
    let mapped = |client: &&serde_json::Value| client["mapped"].as_bool() == Some(true);
    let ours = clients
        .iter()
        .filter(mapped)
        .find(|client| client["pid"].as_u64() == Some(pid as u64))?;
    let here = workspace(ours)?;
    let count = clients
        .iter()
        .filter(mapped)
        .filter(|client| workspace(client) == Some(here))
        .count();
    Some(count == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENTS: &str = r#"[
        {"pid": 10, "mapped": true, "workspace": {"id": 1}},
        {"pid": 20, "mapped": true, "workspace": {"id": 2}},
        {"pid": 30, "mapped": true, "workspace": {"id": 2}},
        {"pid": 40, "mapped": true, "workspace": {"id": -98}}
    ]"#;

    #[test]
    fn decade_is_alone_when_no_other_window_shares_its_workspace() {
        assert_eq!(alone_in(CLIENTS, 10), Some(true));
        assert_eq!(alone_in(CLIENTS, 20), Some(false));
        assert_eq!(alone_in(CLIENTS, 99), None);
    }

    #[test]
    fn only_window_and_workspace_events_trigger_a_recount() {
        assert!(wants_recount("openwindow>>abc,2,foot,foot"));
        assert!(wants_recount("workspacev2>>3,3"));
        assert!(!wants_recount("activewindow>>foot,title"));
    }
}
