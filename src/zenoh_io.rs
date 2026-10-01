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

/// Adds a loopback tcp listener on a free port to `config`, for a second session in
/// this process to connect to directly instead of hoping multicast scouting finds it
/// (zenoh 1.6 does not report the ports a session bound).
pub fn with_loopback_listener(mut config: zenoh::Config) -> Result<(zenoh::Config, u16)> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let endpoint = serde_json::Value::String(format!("tcp/127.0.0.1:{port}"));
    let mut endpoints: serde_json::Value = config
        .get_json("listen/endpoints")
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    // a plain list, or one per mode (zenoh's default)
    match &mut endpoints {
        serde_json::Value::Array(list) => list.push(endpoint),
        serde_json::Value::Object(by_mode) => by_mode
            .values_mut()
            .filter_map(serde_json::Value::as_array_mut)
            .for_each(|list| list.push(endpoint.clone())),
        _ => endpoints = serde_json::Value::Array(vec![endpoint]),
    }
    config
        .insert_json5("listen/endpoints", &endpoints.to_string())
        .map_err(|error| anyhow!("{error}"))?;
    Ok((config, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_listener_joins_the_configured_ones() {
        let (config, port) = with_loopback_listener(zenoh::Config::default()).unwrap();
        let endpoints = config.get_json("listen/endpoints").unwrap();
        assert!(endpoints.contains(&format!("tcp/127.0.0.1:{port}")), "{endpoints}");
        assert!(endpoints.contains("tcp/[::]:0"), "zenoh's default listener is kept: {endpoints}");
        let mut explicit = zenoh::Config::default();
        explicit.insert_json5("listen/endpoints", r#"["tcp/0.0.0.0:7447"]"#).unwrap();
        let (config, port) = with_loopback_listener(explicit).unwrap();
        assert_eq!(config.get_json("listen/endpoints").unwrap(), format!(r#"["tcp/0.0.0.0:7447","tcp/127.0.0.1:{port}"]"#));
    }
}
