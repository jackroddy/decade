use std::collections::VecDeque;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use serde::Deserialize;
use smithay::reexports::calloop::RegistrationToken;

use crate::server::mcp::Reply;
use crate::server::proc;

/// How many parents up from a window's client to look for the program
/// `scratch.open` started.
const ANCESTRY_DEPTH: usize = 8;

/// How long `scratch.open` waits for its program's window when the call
/// gives no `wait`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(5);

/// How many of the last lines of a program's stderr decade keeps.
const STDERR_LINES: usize = 20;

/// How many characters of a stderr line decade keeps.
const STDERR_LINE_CHARS: usize = 500;

/// The answer to `scratch.status` when no program has been started.
pub const NOT_STARTED: &str = "no program has been started with scratch.open in this workspace";

/// The pane agents show programs in, one per workspace at most.
pub struct Scratch {
    pub pane: String,

    /// The program `scratch.open` started outside a terminal, whose window
    /// has not arrived yet.
    pub expected: Option<u32>,

    /// The program that starts once the pane's client has gone.
    pub next: Option<Launch>,
    pub waiting: Option<Waiting>,

    /// The program `scratch.open` last started.
    pub started: Option<Started>,
}

/// A `scratch.open` call that has not been answered yet.
pub struct Waiting {
    pub reply: Reply,
    pub program: String,
    pub wait: Duration,

    /// The program's pid, or `None` while the old occupant closes.
    pub pid: Option<u32>,
    pub timer: Option<RegistrationToken>,
}

/// A program `scratch.open` started, and what is known of it since.
pub struct Started {
    pub program: String,

    /// The pid of the program, or of the terminal it runs in.
    pub pid: u32,
    pub terminal: bool,
    pub exited: Option<ExitStatus>,

    /// The last lines the program wrote to stderr.
    pub stderr: VecDeque<String>,
    pub stderr_open: bool,
}

/// The arguments of `scratch.open`.
#[derive(Deserialize)]
pub struct Launch {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "in_terminal")]
    pub terminal: bool,

    /// How many seconds to wait for the program's window.
    pub wait: Option<f64>,
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
            waiting: None,
            started: None,
        }
    }

    /// The program last started, if its pid is `pid`.
    pub fn started_as(&mut self, pid: u32) -> Option<&mut Started> {
        self.started.as_mut().filter(|started| started.pid == pid)
    }

    /// The answer to `scratch.status`.
    pub fn status(&self) -> String {
        match &self.started {
            Some(started) => started.status(),
            None => NOT_STARTED.to_string(),
        }
    }
}

impl Waiting {
    pub fn new(reply: Reply, program: String, wait: Duration) -> Waiting {
        Waiting {
            reply,
            program,
            wait,
            pid: None,
            timer: None,
        }
    }

    /// The answer to the call from what is known of its program so far, in
    /// the scratch pane `pane`.
    pub fn report(&self, pane: &str, started: Option<&Started>) -> String {
        let program = &self.program;
        let Some(started) = started.filter(|started| Some(started.pid) == self.pid) else {
            return format!(
                "asked the program in pane {pane} to close; it has not closed yet, \
                 and {program} starts there once it has"
            );
        };
        match started.exited {
            Some(status) if started.stderr.is_empty() => {
                format!("{program} exited before opening a window ({status}); its stderr was empty")
            }
            Some(status) => format!(
                "{program} exited before opening a window ({status}); \
                 the last lines of its stderr:\n{}",
                started.stderr_lines()
            ),
            None => format!(
                "{program} is running in pane {pane} with no window after {}s; \
                 decade puts its window in the pane when it opens one",
                self.wait.as_secs_f64()
            ),
        }
    }
}

impl Started {
    pub fn new(program: String, pid: u32, terminal: bool) -> Started {
        Started {
            program,
            pid,
            terminal,
            exited: None,
            stderr: VecDeque::new(),
            stderr_open: !terminal,
        }
    }

    pub fn stderr_line(&mut self, line: &str) {
        if self.stderr.len() == STDERR_LINES {
            self.stderr.pop_front();
        }
        self.stderr
            .push_back(line.chars().take(STDERR_LINE_CHARS).collect());
    }

    fn stderr_lines(&self) -> String {
        let lines: Vec<&str> = self.stderr.iter().map(String::as_str).collect();
        lines.join("\n")
    }

    /// The answer to `scratch.status` for this program.
    pub fn status(&self) -> String {
        let Started { program, pid, .. } = self;
        let Some(status) = self.exited else {
            if self.terminal {
                return format!("{program} is running in a terminal (pid {pid})");
            }
            return format!("{program} (pid {pid}) is running");
        };
        let exited = match status.code() {
            Some(code) => format!("exited with status {code}"),
            None => format!("exited ({status})"),
        };
        if self.terminal {
            format!("{program} ran in a terminal (pid {pid}), which {exited}")
        } else if self.stderr.is_empty() {
            format!("{program} (pid {pid}) {exited}; its stderr was empty")
        } else {
            format!(
                "{program} (pid {pid}) {exited}; its last stderr lines:\n{}",
                self.stderr_lines()
            )
        }
    }
}

/// The time `scratch.open` waits, from its `wait` in seconds.
pub fn wait_for(seconds: Option<f64>) -> Result<Duration, String> {
    let Some(seconds) = seconds else {
        return Ok(DEFAULT_WAIT);
    };
    Duration::try_from_secs_f64(seconds)
        .map_err(|_| format!("wait is a number of seconds, at least 0, not {seconds}"))
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

    fn waiting(pid: Option<u32>) -> Waiting {
        let (reply, _) = tokio::sync::oneshot::channel();
        let mut waiting = Waiting::new(reply, "acre-gui".into(), Duration::from_millis(2500));
        waiting.pid = pid;
        waiting
    }

    fn exit(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn an_exit_is_reported_with_its_status_and_the_last_twenty_lines_of_stderr() {
        let waiting = waiting(Some(40));
        let mut started = Started::new("acre-gui".into(), 40, false);
        started.exited = Some(exit(1));
        assert_eq!(
            waiting.report("scratch", Some(&started)),
            "acre-gui exited before opening a window (exit status: 1); its stderr was empty"
        );
        for n in 1..=25 {
            started.stderr_line(&format!("line {n}"));
        }
        let report = waiting.report("scratch", Some(&started));
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(
            lines[0],
            "acre-gui exited before opening a window (exit status: 1); the last lines of its stderr:"
        );
        assert_eq!(lines.len(), 21);
        assert_eq!((lines[1], lines[20]), ("line 6", "line 25"));
    }

    #[test]
    fn a_program_past_its_wait_is_reported_running_and_one_not_started_as_waiting_to_start() {
        let started = Started::new("acre-gui".into(), 40, false);
        assert_eq!(
            waiting(Some(40)).report("scratch", Some(&started)),
            "acre-gui is running in pane scratch with no window after 2.5s; \
             decade puts its window in the pane when it opens one"
        );

        // the program from an earlier scratch.open is still the
        // one started while the pane's old occupant closes
        for started in [None, Some(&started)] {
            assert!(
                waiting(None).report("scratch", started).starts_with(
                    "asked the program in pane scratch to close; it has not closed yet"
                )
            );
        }
    }

    #[test]
    fn the_status_says_a_program_runs_or_gives_its_exit_and_last_stderr_lines() {
        let mut scratch = Scratch::new("scratch".into());
        assert_eq!(scratch.status(), NOT_STARTED);
        scratch.started = Some(Started::new("acre-gui".into(), 123, false));
        assert_eq!(scratch.status(), "acre-gui (pid 123) is running");
        let started = scratch.started.as_mut().unwrap();
        started.exited = Some(exit(101));
        assert_eq!(
            scratch.status(),
            "acre-gui (pid 123) exited with status 101; its stderr was empty"
        );
        let started = scratch.started.as_mut().unwrap();
        started.stderr_line("thread 'main' panicked");
        started.stderr_line("note: run with RUST_BACKTRACE=1");
        assert_eq!(
            scratch.status(),
            "acre-gui (pid 123) exited with status 101; its last stderr lines:\n\
             thread 'main' panicked\nnote: run with RUST_BACKTRACE=1"
        );
    }

    #[test]
    fn the_status_of_a_program_in_a_terminal_is_the_terminals_with_no_stderr() {
        let mut started = Started::new("htop".into(), 77, true);
        assert!(!started.stderr_open);
        assert_eq!(started.status(), "htop is running in a terminal (pid 77)");
        started.exited = Some(exit(0));
        assert_eq!(
            started.status(),
            "htop ran in a terminal (pid 77), which exited with status 0"
        );
    }

    #[test]
    fn a_new_start_replaces_the_record_and_the_old_programs_stderr_is_not_kept() {
        let mut scratch = Scratch::new("scratch".into());
        scratch.started = Some(Started::new("acre-gui".into(), 40, false));
        scratch.started_as(40).unwrap().stderr_line("old");
        scratch.started = Some(Started::new("acre-gui".into(), 41, false));
        assert!(scratch.started_as(40).is_none());
        scratch.started_as(41).unwrap().stderr_line("new");
        assert_eq!(scratch.started.as_ref().unwrap().stderr, ["new"]);
        assert_eq!(scratch.status(), "acre-gui (pid 41) is running");
    }

    #[test]
    fn stderr_keeps_the_last_twenty_lines_each_cut_to_its_first_500_characters() {
        let mut started = Started::new("acre-gui".into(), 40, false);
        for n in 1..=45 {
            started.stderr_line(&format!("line {n}"));
        }
        started.stderr_line(&"x".repeat(2000));
        assert_eq!(started.stderr.len(), 20);
        assert_eq!(started.stderr[0], "line 27");
        assert_eq!(started.stderr[19].len(), 500);
    }

    #[test]
    fn the_wait_defaults_to_five_seconds_and_refuses_a_negative_time() {
        assert_eq!(wait_for(None), Ok(Duration::from_secs(5)));
        assert_eq!(wait_for(Some(30.0)), Ok(Duration::from_secs(30)));
        assert!(wait_for(Some(-1.0)).is_err());
        assert!(wait_for(Some(f64::NAN)).is_err());
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
