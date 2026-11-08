use std::path::PathBuf;

/// The parent of process `pid`.
pub fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    // the command name sits in parentheses and may hold spaces or ')', so
    // the fields are read after the last ')': state, then the parent pid
    let (_, fields) = stat.rsplit_once(')')?;
    fields.split_whitespace().nth(1)?.parse().ok()
}

/// The first child of process `pid`, started by any of its threads.
pub fn first_child(pid: u32) -> Option<u32> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    tasks
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .find_map(|children| children.split_whitespace().next()?.parse().ok())
}

/// The working directory of process `pid`.
pub fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// Run `script` under `sh -c`, and ask `find` about the shell's pid
    /// until it answers or two seconds pass.
    pub(crate) fn under_shell<T>(script: &str, find: impl Fn(u32) -> Option<T>) -> Option<T> {
        let mut shell = Command::new("sh").args(["-c", script]).spawn().unwrap();
        let mut found = None;
        for _ in 0..100 {
            found = find(shell.id());
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = shell.kill();
        let _ = shell.wait();
        found
    }

    #[test]
    fn a_child_has_its_own_cwd_and_names_its_parent() {
        let found = under_shell("(cd / && sleep 5); true", |shell| {
            let child = first_child(shell)?;
            Some((cwd(child)?, parent(child)? == shell))
        });
        assert_eq!(found, Some((Path::new("/").to_path_buf(), true)));
    }
}
