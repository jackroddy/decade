use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::FlockOperation;

use crate::files;
use crate::wire::{Decoder, Listed, Outbox, ServerMessage, ViewerMessage};
use rustix::process::{Pid, Signal};

/// The exit status of a server that found another server running.
pub const ANOTHER_SERVER: i32 = 3;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// The directory of the server's viewer socket, lock and log.
pub fn dir() -> PathBuf {
    files::runtime_dir().join("decade")
}

pub fn viewer_socket_path() -> PathBuf {
    dir().join("viewer.sock")
}

fn lock_path() -> PathBuf {
    dir().join("server.lock")
}

/// Lock the server's lock file and record this process's pid in it, or exit.
pub fn lock_server() -> File {
    let path = lock_path();
    let mut lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("decade: {}: {error}", path.display());
            std::process::exit(1);
        }
    };
    match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => {
            eprintln!(
                "decade: another server holds {}, so this one is exiting",
                path.display()
            );
            std::process::exit(ANOTHER_SERVER);
        }
        Err(error) => {
            eprintln!("decade: {} would not lock: {error}", path.display());
            std::process::exit(1);
        }
    }
    let written = lock
        .set_len(0)
        .and_then(|()| write!(lock, "{}", std::process::id()));
    if let Err(error) = written {
        eprintln!(
            "decade: {} has no pid, so decade stop cannot find this server: {error}",
            path.display()
        );
    }
    lock
}

/// The pid of the running server, or `None` when no server holds the lock.
pub fn running() -> Result<Option<u32>, String> {
    let path = lock_path();
    let Ok(mut lock) = File::open(&path) else {
        return Ok(None);
    };

    // a lock this probe takes is released when `lock` drops
    match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockShared) {
        Ok(()) => return Ok(None),
        Err(rustix::io::Errno::WOULDBLOCK) => {}
        Err(error) => return Err(format!("{} would not lock: {error}", path.display())),
    }
    let mut pid = String::new();
    lock.read_to_string(&mut pid)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    match pid.trim().parse() {
        Ok(pid) => Ok(Some(pid)),
        Err(_) => Err(format!(
            "a server holds {} but recorded no pid there",
            path.display()
        )),
    }
}

/// Stop the running server and wait for it to exit, returning its pid.
pub fn stop() -> Result<Option<u32>, String> {
    let Some(pid) = running()? else {
        return Ok(None);
    };
    let process = Pid::from_raw(pid as i32).ok_or_else(|| format!("{pid} is not a pid"))?;
    rustix::process::kill_process(process, Signal::Term)
        .map_err(|error| format!("signalling the server (pid {pid}) failed: {error}"))?;
    let start = Instant::now();
    while running()?.is_some() {
        if start.elapsed() > STOP_TIMEOUT {
            return Err(format!(
                "the server (pid {pid}) is still running {} s after it was told to stop",
                STOP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
    Ok(Some(pid))
}

/// The running server's workspaces, in picker order.
pub fn list() -> Result<Vec<Listed>, String> {
    listing(&connect()?)
}

/// Connect to the running server's viewer socket.
pub fn connect() -> Result<UnixStream, String> {
    UnixStream::connect(viewer_socket_path()).map_err(|_| "no server is running".to_string())
}

fn listing(stream: &UnixStream) -> Result<Vec<Listed>, String> {
    let mut outbox = Outbox::new();
    outbox
        .push_viewer_message(ViewerMessage::List)
        .map_err(|error| format!("{error:?}"))?;
    outbox
        .flush(stream, None)
        .map_err(|error| error.to_string())?;
    let mut decoder = Decoder::new();
    loop {
        if let Some(message) = decoder
            .next_server_message()
            .map_err(|error| format!("{error:?}"))?
        {
            return match message {
                ServerMessage::Listing { workspaces } => Ok(workspaces),
                ServerMessage::Error { message, .. } => Err(message),
                _ => Err("the server answered with something other than a listing".into()),
            };
        }
        let open = decoder
            .wait(stream, None)
            .map_err(|error| format!("{error:?}"))?;
        if !open {
            return Err("the server closed the connection without a listing".into());
        }
    }
}

/// Connect to the running server, starting one in the background first if none answers.
pub fn connect_or_start() -> Result<UnixStream, String> {
    if let Ok(stream) = connect() {
        return Ok(stream);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let log_path = dir().join("server.log");
    let log = std::fs::create_dir_all(dir())
        .and_then(|()| OpenOptions::new().create(true).append(true).open(&log_path))
        .map_err(|error| format!("{}: {error}", log_path.display()))?;
    let log_out = log.try_clone().map_err(|error| error.to_string())?;
    let mut child = Command::new(exe)
        .arg("server")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log_out)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("decade server did not start: {error}"))?;
    let start = Instant::now();
    let mut lost = false;
    loop {
        std::thread::sleep(POLL);
        if let Ok(stream) = connect() {
            std::thread::spawn(move || child.wait());
            return Ok(stream);
        }
        if !lost && let Ok(Some(status)) = child.try_wait() {
            if status.code() != Some(ANOTHER_SERVER) {
                return Err(format!("decade server exited before it answered: {status}"));
            }
            // the server holding the lock answers at the same path
            lost = true;
        }
        if start.elapsed() > START_TIMEOUT {
            return Err(format!(
                "decade server did not answer at {}",
                viewer_socket_path().display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_cpu_time() -> Duration {
        let schedstat = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap();
        let ns = schedstat
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        Duration::from_nanos(ns)
    }

    #[test]
    fn waiting_for_a_listing_sleeps_until_the_server_answers() {
        let (client, server) = UnixStream::pair().unwrap();
        let answer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let mut outbox = Outbox::new();
            outbox
                .push_server_message(ServerMessage::Listing { workspaces: vec![] })
                .unwrap();
            outbox.flush(&server, None).unwrap();
            server
        });
        let before = thread_cpu_time();
        let workspaces = listing(&client).unwrap();
        let spent = thread_cpu_time() - before;
        drop(answer.join().unwrap());
        assert!(workspaces.is_empty());
        assert!(spent < Duration::from_millis(100), "{spent:?} on the CPU");
    }
}
