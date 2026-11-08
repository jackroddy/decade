use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// How many parents up from a window's client to look for the program
/// `scratch.open` started.
const ANCESTRY_DEPTH: usize = 8;

/// The pane agents show programs in, one per workspace at most.
pub struct Scratch {
    pub pane: String,

    /// The program `scratch.open` started outside a terminal, whose window
    /// has not arrived yet.
    pub expected: Option<u32>,

    /// The program that starts once the pane's client has gone.
    pub next: Option<Launch>,
}

pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub terminal: bool,
}

impl Scratch {
    pub fn new(pane: String) -> Scratch {
        Scratch {
            pane,
            expected: None,
            next: None,
        }
    }
}

/// Check that `program` is a bare name starting with `decade-` and an
/// executable file in one of the `PATH` directories.
pub fn check_program(program: &str, path: &OsStr) -> Result<(), String> {
    if !program.starts_with("decade-") || program.contains('/') {
        return Err(format!(
            "{program} is not a bare program name starting with decade-"
        ));
    }
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .any(|candidate| is_executable(&candidate))
        .then_some(())
        .ok_or_else(|| format!("{program} is not on decade's PATH"))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Whether `pid` is `ancestor` or a process under it.
pub fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut pid = pid;
    for _ in 0..ANCESTRY_DEPTH {
        if pid == ancestor {
            return true;
        }
        match parent(pid) {
            Some(parent) if parent > 1 => pid = parent,
            _ => return false,
        }
    }
    false
}

fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    // the command name sits in parentheses and may hold spaces or ')', so
    // the fields are read after the last ')': state, then the parent pid
    let (_, fields) = stat.rsplit_once(')')?;
    fields.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn only_a_bare_decade_program_on_the_path_passes() {
        let dir = std::env::temp_dir().join(format!("decade-scratch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("decade-code");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(["/nonexistent", dir.to_str().unwrap()]).unwrap();

        assert_eq!(check_program("decade-code", &path), Ok(()));
        assert!(check_program("decade-missing", &path).is_err());
        assert!(check_program("/bin/sh", &path).is_err());
        assert!(check_program("bash", OsStr::new("/usr/bin:/bin")).is_err());
        assert!(check_program("decade-x/y", &path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn first_child(pid: u32) -> Option<u32> {
        let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
        children.split_whitespace().next()?.parse().ok()
    }

    #[test]
    fn a_grandchild_descends_from_the_process_that_started_it() {
        // the subshell forks sleep rather than becoming it, since a
        // command follows it
        let mut parent = Command::new("sh")
            .args(["-c", "(sleep 5; true) & wait"])
            .spawn()
            .unwrap();
        let mut grandchild = None;
        for _ in 0..100 {
            grandchild = first_child(parent.id()).and_then(first_child);
            if grandchild.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let descends = grandchild.is_some_and(|pid| descends_from(pid, parent.id()));
        let unrelated = descends_from(std::process::id(), parent.id());
        let _ = parent.kill();
        let _ = parent.wait();
        assert!(descends);
        assert!(!unrelated);
    }
}
