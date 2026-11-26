use decade_channel::{Agent, AgentState, AgentUpdate, Channel};

fn main() -> std::io::Result<()> {
    let agent = Agent {
        name: "blah".to_string(),
        version: "0.1.0".to_string(),
        session: "42".to_string(),
        args: Vec::new(),
    };
    let (mut channel, messages) =
        Channel::from_env("blah", &[], &[], Some(&agent)).expect("DECADE_SOCKET is not set")?;
    channel.agent(&AgentUpdate {
        state: Some(Some(AgentState::Working)),
        activity: Some(Some("blah blah blah".to_string())),
        ..AgentUpdate::default()
    })?;
    for message in messages {
        println!("{message:?}");
    }
    Ok(())
}
