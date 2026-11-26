use std::time::Duration;

use decade_channel::Channel;

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.send("b", "blah", 42)?;
    while let Ok(message) = messages.recv_timeout(Duration::from_secs(1)) {
        println!("{message:?}");
    }
    Ok(())
}
