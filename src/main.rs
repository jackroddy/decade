#[cfg(target_os = "linux")]
mod apps;
#[cfg(target_os = "linux")]
mod channel;
#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "linux")]
mod draw;
#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod hypr;
#[cfg(target_os = "linux")]
mod inhibit;
#[cfg(target_os = "linux")]
mod mcp;
#[cfg(target_os = "linux")]
mod menu;
#[cfg(target_os = "linux")]
mod palette;
#[cfg(target_os = "linux")]
mod panel;
#[cfg(target_os = "linux")]
mod panes;
#[cfg(target_os = "linux")]
mod settings;
#[cfg(target_os = "linux")]
mod text;
#[cfg(target_os = "linux")]
mod viewer;
mod wire;

#[cfg(target_os = "linux")]
const USAGE: &str = "usage: decade [--class <app id>] [--chat \"<command>\"] [--code \"<command>\"]
       decade server [--chat \"<command>\"] [--code \"<command>\"]";

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let server = args.peek().map(String::as_str) == Some("server");
    if server {
        args.next();
    }
    let mut commands = compositor::Commands {
        chat: None,
        code: None,
    };
    let mut class = "decade".to_string();
    let mut server_args = Vec::new();
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let command = || Some(value.split_whitespace().map(str::to_string).collect());
        match flag.as_str() {
            "--chat" => commands.chat = command(),
            "--code" => commands.code = command(),
            "--class" if !server => {
                class = value;
                continue;
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
        server_args.push(flag);
        server_args.push(value);
    }
    if server {
        compositor::run(commands);
    } else {
        viewer::run(class, server_args);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "decade: here decade can only view a server on another machine, which is not built yet"
    );
    std::process::exit(1);
}
