use decade_channel::{Channel, Verb};

fn main() -> std::io::Result<()> {
    let accepts = [Verb::new("blah", "Print the payload.")];
    let (_channel, messages) =
        Channel::from_env("blah", &accepts, &[], None).expect("DECADE_SOCKET is not set")?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
