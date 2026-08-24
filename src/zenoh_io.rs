use crate::hub::Hub;
use anyhow::{anyhow, Result};
use std::sync::Arc;
use zenoh::pubsub::{Publisher, Subscriber};
use zenoh::Session;

pub async fn open() -> Result<Session> {
    zenoh::open(zenoh::Config::default())
        .await
        .map_err(|error| anyhow!("{error}"))
}

/// One catch-all subscriber feeds discovery; payloads for unwatched topics are
/// counted and dropped without decoding.
pub async fn subscribe_all(session: &Session, hub: Arc<Hub>) -> Result<Subscriber<()>> {
    let subscriber = session
        .declare_subscriber("**")
        .callback(move |sample| {
            let key_expr = sample.key_expr().as_str().to_owned();
            let payload = sample.payload().to_bytes();
            hub.on_zenoh_message(&key_expr, &payload);
        })
        .await
        .map_err(|error| anyhow!("{error}"))?;
    Ok(subscriber)
}

pub async fn declare_publisher(session: &Session, key_expr: String) -> Result<Publisher<'static>> {
    session
        .declare_publisher(key_expr)
        .await
        .map_err(|error| anyhow!("{error}"))
}

/// Zenoh key expressions may not start with `/`, but dimos topics are written
/// `/tele_cmd_vel`, so the leading slash is dropped and the type appended.
pub fn key_expr_for(topic: &str, msg_type: &str) -> String {
    format!("{}/{}", topic.trim_start_matches('/'), msg_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_dimos_style_key_expression() {
        assert_eq!(
            key_expr_for("/tele_cmd_vel", "geometry_msgs.Twist"),
            "tele_cmd_vel/geometry_msgs.Twist"
        );
    }
}
