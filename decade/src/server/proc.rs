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

/// The foreground process group of the terminal process `pid` runs on,
/// when it is not `pid`'s own group.
pub fn foreground_group(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    foreground_in(&stat)
}

fn foreground_in(stat: &str) -> Option<u32> {
    // after the last ')': state, parent, process group, session, terminal,
    // then the terminal's foreground group, -1 when there is no terminal
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let (group, foreground) = (fields.get(2)?, fields.get(5)?);
    if group == foreground {
        return None;
    }
    foreground.parse().ok()
}

/// The command name of process `pid`.
pub fn comm(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end_matches('\n').to_string())
}

/// The working directory of process `pid`.
pub fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(test)]
pub(super) mod tests {
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

    #[test]
    fn the_foreground_group_is_read_after_the_command_name_and_only_when_not_the_own_group() {
        let stat = |group: &str, foreground: &str| {
            format!("41 (a) b) S 1 {group} 41 34816 {foreground} 4194304 0")
        };
        assert_eq!(foreground_in(&stat("41", "57")), Some(57));
        assert_eq!(foreground_in(&stat("41", "41")), None);
        assert_eq!(foreground_in(&stat("41", "-1")), None);

        let own = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
        let (_, fields) = own.rsplit_once(')').unwrap();
        assert!(
            fields
                .split_whitespace()
                .nth(5)
                .unwrap()
                .parse::<i32>()
                .is_ok()
        );
    }

    #[test]
    fn the_command_name_is_the_programs_name_cut_to_15_bytes() {
        let program = std::env::current_exe().unwrap();
        let name = program.file_name().unwrap().to_string_lossy();
        let cut: String = name.chars().take(15).collect();
        assert_eq!(comm(std::process::id()), Some(cut));
    }
}
