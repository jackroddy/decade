use decade_channel::{Channel, Restore};

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.restore(&Restore {
        argv: std::env::args().collect(),
        terminal: true,
        cwd: std::env::current_dir().ok(),
    })?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
