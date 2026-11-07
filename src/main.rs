mod apps;
mod channel;
mod compositor;
mod draw;
mod gpu;
mod hypr;
mod inhibit;
mod mcp;
mod menu;
mod palette;
mod panel;
mod panes;
mod settings;
mod text;
mod viewer;
mod wire;

const USAGE: &str = "usage: decade [--class <app id>] [--chat \"<command>\"] [--code \"<command>\"]
       decade server [--chat \"<command>\"] [--code \"<command>\"]";

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
