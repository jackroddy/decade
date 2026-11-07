mod channel;
mod chrome;
mod compositor;
mod grid;
mod panes;
mod text;

fn main() {
    let mut commands = compositor::Commands {
        chat: None,
        code: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let command = args
            .next()
            .map(|command| command.split_whitespace().map(str::to_string).collect());
        match (flag.as_str(), command) {
            ("--chat", Some(command)) => commands.chat = Some(command),
            ("--code", Some(command)) => commands.code = Some(command),
            _ => {
                eprintln!("usage: decade [--chat \"<command>\"] [--code \"<command>\"]");
                std::process::exit(2);
            }
        }
    }
    compositor::run(commands);
}
