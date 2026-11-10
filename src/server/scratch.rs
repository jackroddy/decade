use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::server::proc;

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

/// The arguments of `scratch.open`.
#[derive(Deserialize)]
pub struct Launch {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "in_terminal")]
    pub terminal: bool,
}

fn in_terminal() -> bool {
    true
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

/// The program to start for `program`: a name looked up on `PATH`, or a
/// path with a `/` in it, made absolute against `cwd`.
pub fn resolve_program(program: &str, path: &OsStr, cwd: &Path) -> Result<String, String> {
    if program.contains('/') {
        let file: PathBuf = cwd.join(program).components().collect();
        if !is_executable(&file) {
            return Err(format!("{} is not an executable file", file.display()));
        }
        return file
            .into_os_string()
            .into_string()
            .map_err(|file| format!("{} is not a UTF-8 path", file.display()));
    }
    std::env::split_paths(path)
        .any(|dir| is_executable(&dir.join(program)))
        .then(|| program.to_string())
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
        match proc::parent(pid) {
            Some(parent) if parent > 1 => pid = parent,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_found_on_the_path_or_by_a_path_from_the_workspace() {
        let dir = std::env::temp_dir().join(format!("decade-scratch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let program = dir.join("bin/acre-gui");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("notes"), "").unwrap();
        let bin = dir.join("bin");
        let path = std::env::join_paths(["/nonexistent", bin.to_str().unwrap()]).unwrap();
        let absolute = program.to_str().unwrap().to_string();

        assert_eq!(
            resolve_program("acre-gui", &path, &dir),
            Ok("acre-gui".into())
        );
        assert!(resolve_program("missing", &path, &dir).is_err());
        assert_eq!(
            resolve_program("./bin/acre-gui", &path, &dir),
            Ok(absolute.clone())
        );
        assert_eq!(
            resolve_program(&absolute, &path, Path::new("/")),
            Ok(absolute)
        );
        assert!(resolve_program("./notes", &path, &dir).is_err());
        assert!(resolve_program("./bin/missing", &path, &dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_grandchild_descends_from_the_process_that_started_it() {
        // the subshell forks sleep rather than becoming it, since a
        // command follows it
        let found = proc::tests::under_shell("(sleep 5; true) & wait", |shell| {
            let grandchild = proc::first_child(proc::first_child(shell)?)?;
            Some((
                descends_from(grandchild, shell),
                descends_from(std::process::id(), shell),
            ))
        });
        assert_eq!(found, Some((true, false)));
    }
}
