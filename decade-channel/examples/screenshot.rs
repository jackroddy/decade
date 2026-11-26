use decade_channel::{Channel, Message, Screenshot};

fn main() -> std::io::Result<()> {
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], None).expect("DECADE_SOCKET is not set")?;
    channel.screenshot(&Screenshot {
        pane: "b".to_string(),
        file: "blah.png".to_string(),
        caller: None,
    })?;
    for message in messages {
        match message {
            Message::Screenshot { png } => {
                std::fs::write("blah.png", &png)?;
                println!("wrote {} bytes to blah.png", png.len());
                break;
            }
            Message::Error { reason, .. } => {
                println!("{reason}");
                break;
            }
            _ => {}
        }
    }
    Ok(())
}
