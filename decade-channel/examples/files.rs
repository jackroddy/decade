use decade_channel::{Channel, Shows};

fn main() -> std::io::Result<()> {
    let shows = [Shows {
        kind: "file".to_string(),
        highlight: true,
    }];
    let (_channel, messages) =
        Channel::from_env("blah", &[], &shows, None).expect("DECADE_SOCKET is not set")?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
