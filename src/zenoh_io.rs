use anyhow::{anyhow, Result};
use std::path::Path;
use zenoh::pubsub::Subscriber;
use zenoh::sample::Sample;
use zenoh::Session;

/// zenoh's defaults (a peer with multicast scouting) unless a json5 file is given,
/// plus any extra endpoints to connect to.
pub fn config(file: Option<&Path>, connect: &[String]) -> Result<zenoh::Config> {
    let mut config = match file {
        Some(path) => zenoh::Config::from_file(path).map_err(|error| anyhow!("{}: {error}", path.display()))?,
        None => zenoh::Config::default(),
    };
    if !connect.is_empty() {
        config
            .insert_json5("connect/endpoints", &serde_json::to_string(connect)?)
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(config)
}

pub async fn open(config: zenoh::Config) -> Result<Session> {
    zenoh::open(config).await.map_err(|error| anyhow!("{error}"))
}

/// One catch-all subscriber feeds discovery, recording and the command mirror.
pub async fn subscribe_all<F>(session: &Session, on_sample: F) -> Result<Subscriber<()>>
where
    F: Fn(&str, &[u8]) + Send + Sync + 'static,
{
    let subscriber = session
        .declare_subscriber("**")
        .callback(move |sample: Sample| {
            on_sample(sample.key_expr().as_str(), &sample.payload().to_bytes());
        })
        .await
        .map_err(|error| anyhow!("{error}"))?;
    Ok(subscriber)
}

/// The tcp port this session listens on, for a second session in this process to
/// connect to directly instead of hoping multicast scouting finds it.
pub async fn local_tcp_port(session: &Session) -> Option<u16> {
    session
        .info()
        .locators()
        .await
        .iter()
        .map(|locator| locator.to_string())
        .filter(|locator| locator.starts_with("tcp/"))
        .find_map(|locator| locator.rsplit(':').next().and_then(|port| port.parse().ok()))
}
