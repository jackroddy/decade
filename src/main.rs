mod chrome;
mod compositor;
mod grid;
mod panes;
mod text;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let chat = match args.first().map(String::as_str) {
        Some("--chat") if args.len() > 1 => Some(args[1..].to_vec()),
        None => None,
        _ => {
            eprintln!("usage: decade [--chat <command> [args...]]");
            std::process::exit(2);
        }
    };
    compositor::run(compositor::Commands { chat });
}
