use decade_channel::{Channel, Message};

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.panes()?;
    for message in messages {
        if let Message::Panes(panes) = message {
            for pane in panes {
                println!("{pane:?}");
            }
            break;
        }
    }
    Ok(())
}
