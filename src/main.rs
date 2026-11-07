mod apps;
mod channel;
mod compositor;
mod draw;
mod mcp;
mod menu;
mod palette;
mod panes;
mod text;

fn main() {
    let mut commands = compositor::Commands {
        chat: None,
        code: None,
        class: "decade".to_string(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let command = args
            .next()
            .map(|command| command.split_whitespace().map(str::to_string).collect());
        match (flag.as_str(), command) {
            ("--chat", Some(command)) => commands.chat = Some(command),
            ("--code", Some(command)) => commands.code = Some(command),
            ("--class", Some(class)) if class.len() == 1 => commands.class = class[0].clone(),
            _ => {
                eprintln!(
                    "usage: decade [--chat \"<command>\"] [--code \"<command>\"] [--class <app id>]"
                );
                std::process::exit(2);
            }
        }
    }
    compositor::run(commands);
}
