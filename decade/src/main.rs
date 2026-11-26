#[cfg(target_os = "linux")]
mod chrome;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod files;
#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod lines;
#[cfg(target_os = "linux")]
mod png;
#[cfg(target_os = "linux")]
mod record;
#[cfg(target_os = "linux")]
mod screenshot;
#[cfg(target_os = "linux")]
mod script;
#[cfg(target_os = "linux")]
mod server;
#[cfg(target_os = "linux")]
mod viewer;
mod wire;

#[cfg(target_os = "linux")]
const USAGE: &str = "usage: decade [--class <app id>]
       decade start | stop | restart | status | list
       decade script FILE
       decade screenshot PANE FILE
       decade record PANE FILE [--format mp4|gif] [--fps N] [--duration SECONDS]
                     [--width N] [--quality N] [--no-pointer] [--once]
       decade server";

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next(), args.next()) {
        (Some("server"), None, _) => {
            set_palette();
            server::compositor::run();
        }
        (Some("start"), None, _) => start(),
        (Some("stop"), None, _) => stop(),
        (Some("restart"), None, _) => {
            stop();
            start();
        }
        (Some("status"), None, _) => status(),
        (Some("list"), None, _) => list(),
        (Some("script"), Some(file), None) => {
            if let Err(error) = script::run(&file) {
                eprintln!("decade: {error}");
                std::process::exit(1);
            }
        }
        (Some("screenshot"), Some(pane), Some(file)) if args.next().is_none() => {
            if let Err(error) = screenshot::run(&pane, &file) {
                eprintln!("decade: {error}");
                std::process::exit(1);
            }
        }
        (Some("record"), Some(pane), Some(file)) => {
            let flags: Vec<String> = args.collect();
            if let Err(error) = record::run(&pane, &file, &flags) {
                eprintln!("decade: {error}");
                std::process::exit(1);
            }
        }
        (None, _, _) => {
            set_palette();
            viewer::run("decade".to_string());
        }
        (Some("--class"), Some(class), None) => {
            set_palette();
            viewer::run(class);
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "linux")]
fn set_palette() {
    let path = files::xdg_home("XDG_STATE_HOME", ".local/state")
        .map(|state| state.join("omarchy/current/theme/colors.toml"));
    let text = path
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    decade_ui::palette::set(decade_ui::palette::Palette::from_theme(&text));
}

#[cfg(target_os = "linux")]
fn start() {
    match control::running() {
        Ok(None) => {}
        Ok(Some(pid)) => {
            println!("decade: a server is already running, pid {pid}");
            return;
        }
        Err(error) => {
            eprintln!("decade: {error}");
            std::process::exit(1);
        }
    }
    if let Err(error) = control::connect_or_start() {
        eprintln!("decade: {error}");
        std::process::exit(1);
    }
    match control::running() {
        Ok(Some(pid)) => println!("decade: started a server, pid {pid}"),
        _ => println!("decade: started a server"),
    }
}

#[cfg(target_os = "linux")]
fn stop() {
    match control::stop() {
        Ok(Some(pid)) => println!("decade: stopped the server, pid {pid}"),
        Ok(None) => println!("decade: no server is running"),
        Err(error) => {
            eprintln!("decade: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn list() {
    let workspaces = match control::list() {
        Ok(workspaces) => workspaces,
        Err(error) => {
            eprintln!("decade: {error}");
            std::process::exit(1);
        }
    };
    let width = workspaces
        .iter()
        .map(|workspace| workspace.name.chars().count())
        .max()
        .unwrap_or(0);
    for workspace in workspaces {
        let panes = if workspace.panes == 1 {
            "pane"
        } else {
            "panes"
        };
        println!(
            "{:>3}  {:width$}  {:>2} {panes:5}  {}",
            workspace.id, workspace.name, workspace.panes, workspace.cwd
        );
    }
}

#[cfg(target_os = "linux")]
fn status() {
    match control::running() {
        Ok(Some(pid)) => println!("decade: a server is running, pid {pid}"),
        Ok(None) => {
            println!("decade: no server is running");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("decade: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "decade: here decade can only view a server on another machine, which is not built yet"
    );
    std::process::exit(1);
}
