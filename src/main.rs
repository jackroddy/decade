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
mod workspace;

#[cfg(target_os = "linux")]
const USAGE: &str = "usage: decade [--class <app id>]
       decade server";

#[cfg(target_os = "linux")]
fn main() {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next(), args.next()) {
        (Some("server"), None, _) => compositor::run(),
        (None, _, _) => viewer::run("decade".to_string()),
        (Some("--class"), Some(class), None) => viewer::run(class),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
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
