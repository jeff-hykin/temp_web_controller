//! Republishes LCM channels on zenoh so browsers can reach them through zenoh-web.
//!
//! Lazy on two levels. A channel gets a zenoh publisher and a liveliness token the
//! first time it is heard, so it shows up in zenoh-web's `listTopics` even while
//! nobody watches it. Payloads are only copied onto zenoh while a matching listener
//! says some remote subscriber wants that key, so an unwatched 30 Hz camera costs a
//! hash lookup per frame rather than a 1 MB put.
//!
//! The publishers only deliver to *remote* subscribers. zenoh counts every local
//! subscriber as a match regardless of its allowed origin, and web_ctrl's own
//! catch-all `**` subscriber lives in this session, so a local-inclusive publisher
//! would always look watched (and would feed every relayed frame back into the
//! recorder as a second, zenoh-side copy of the topic). That is why zenoh-web always
//! runs on a zenoh session of its own, even when web_ctrl starts it in-process.

use crate::hub::{self, Hub};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zenoh::liveliness::LivelinessToken;
use zenoh::matching::MatchingListener;
use zenoh::pubsub::Publisher;
use zenoh::qos::CongestionControl;
use zenoh::sample::Locality;
use zenoh::{Session, Wait};

/// A channel unheard for this long has its publisher and token withdrawn, so a
/// camera that stopped drops out of the browser's topic list.
pub const FORGET_AFTER: Duration = Duration::from_secs(60);

struct Relayed {
    publisher: Publisher<'static>,
    matched: Arc<AtomicBool>,
    _token: LivelinessToken,
    _listener: MatchingListener<()>,
}

struct Slot {
    /// `None` for a channel that is deliberately not relayed or could not be: rpc,
    /// the command topic, a name zenoh refuses. Kept so it is not retried per message.
    relayed: Option<Arc<Relayed>>,
    last_seen: Instant,
}

pub struct LcmRelay {
    session: Session,
    hub: Arc<Hub>,
    slots: Mutex<HashMap<String, Slot>>,
}

impl LcmRelay {
    pub fn new(session: Session, hub: Arc<Hub>) -> Arc<Self> {
        Arc::new(LcmRelay { session, hub, slots: Mutex::new(HashMap::new()) })
    }

    /// Notes that `channel` is alive, declaring its publisher the first time.
    fn observe(&self, channel: &str) -> Option<Arc<Relayed>> {
        let mut slots = self.slots.lock().unwrap();
        if let Some(slot) = slots.get_mut(channel) {
            slot.last_seen = Instant::now();
            return slot.relayed.clone();
        }
        let relayed = match self.declare(channel) {
            Ok(relayed) => relayed.map(Arc::new),
            Err(error) => {
                eprintln!("not relaying lcm channel {channel} to zenoh: {error}");
                None
            }
        };
        slots.insert(channel.to_owned(), Slot { relayed: relayed.clone(), last_seen: Instant::now() });
        relayed
    }

    fn declare(&self, channel: &str) -> Result<Option<Relayed>> {
        let (topic, msg_type) = hub::parse_lcm_channel(channel);
        // Commands are mirrored the other way (zenoh to lcm) by web_ctrl itself, so
        // relaying them back would echo every press.
        if hub::is_rpc_topic(&topic) || self.hub.is_command_topic(&topic) {
            return Ok(None);
        }
        let key = hub::lcm_relay_key(&topic, msg_type.as_deref());
        let publisher = self
            .session
            .declare_publisher(key.clone())
            .allowed_destination(Locality::Remote)
            .congestion_control(CongestionControl::Drop)
            .wait()
            .map_err(|error| anyhow!("{key}: {error}"))?;
        let token = self
            .session
            .liveliness()
            .declare_token(key.clone())
            .wait()
            .map_err(|error| anyhow!("{key}: {error}"))?;
        let matched = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&matched);
        let listener = publisher
            .matching_listener()
            .callback(move |status| flag.store(status.matching(), Ordering::Relaxed))
            .wait()
            .map_err(|error| anyhow!("{key}: {error}"))?;
        // Read after the listener exists, so a subscriber that arrived in between is
        // not missed.
        if let Ok(status) = publisher.matching_status().wait() {
            matched.fetch_or(status.matching(), Ordering::Relaxed);
        }
        Ok(Some(Relayed { publisher, matched, _token: token, _listener: listener }))
    }

    /// Whether a fragmented message on `channel` is worth reassembling for zenoh.
    pub fn wants_payload(&self, channel: &str) -> bool {
        self.observe(channel).is_some_and(|relayed| relayed.matched.load(Ordering::Relaxed))
    }

    /// A message nobody is watching, counted for discovery only.
    pub fn note_skipped(&self, channel: &str) {
        self.observe(channel);
    }

    pub fn forward(&self, channel: &str, payload: &[u8]) {
        let Some(relayed) = self.observe(channel) else {
            return;
        };
        if !relayed.matched.load(Ordering::Relaxed) {
            return;
        }
        // The command topic can be renamed onto a channel already relayed.
        let (topic, _) = hub::parse_lcm_channel(channel);
        if self.hub.is_command_topic(&topic) {
            return;
        }
        let put = relayed
            .publisher
            .put(payload.to_vec())
            .timestamp(self.session.new_timestamp())
            .wait();
        if let Err(error) = put {
            eprintln!("relaying {channel} to zenoh failed: {error}");
        }
    }

    #[cfg(test)]
    pub fn is_relayed(&self, channel: &str) -> bool {
        self.slots.lock().unwrap().get(channel).is_some_and(|slot| slot.relayed.is_some())
    }

    pub fn sweep(&self) {
        self.slots.lock().unwrap().retain(|_, slot| slot.last_seen.elapsed() < FORGET_AFTER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Settings;
    use zenoh::sample::Sample;

    fn isolated_config(listen: &str, connect: Option<&str>) -> zenoh::Config {
        let mut config = zenoh::Config::default();
        config.insert_json5("scouting/multicast/enabled", "false").unwrap();
        config.insert_json5("listen/endpoints", &format!("[\"{listen}\"]")).unwrap();
        if let Some(connect) = connect {
            config.insert_json5("connect/endpoints", &format!("[\"{connect}\"]")).unwrap();
        }
        config
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_channel_is_only_forwarded_while_a_remote_subscriber_matches() {
        let port = free_port();
        let relay_session =
            zenoh::open(isolated_config(&format!("tcp/127.0.0.1:{port}"), None)).wait().unwrap();
        let viewer_session = zenoh::open(isolated_config(
            "tcp/127.0.0.1:0",
            Some(&format!("tcp/127.0.0.1:{port}")),
        ))
        .wait()
        .unwrap();
        let hub = Hub::new(Settings { publish_topic: "/tele_cmd_vel_test".to_owned(), ..Settings::default() });
        let relay = LcmRelay::new(relay_session.clone(), hub);

        // A local catch-all subscriber, like web_ctrl's own, must neither count as a
        // match nor receive relayed frames.
        let local = Arc::new(Mutex::new(Vec::<String>::new()));
        let local_sink = Arc::clone(&local);
        let _local = relay_session
            .declare_subscriber("**")
            .callback(move |sample: Sample| local_sink.lock().unwrap().push(sample.key_expr().to_string()))
            .wait()
            .unwrap();

        let channel = "/cam#sensor_msgs.Image";
        assert!(!relay.wants_payload(channel), "nobody is subscribed yet");
        assert!(relay.is_relayed(channel));

        // The token makes the channel discoverable without subscribing to it.
        let tokens = || -> Vec<String> {
            viewer_session
                .liveliness()
                .get("dimos/**")
                .wait()
                .unwrap()
                .iter()
                .filter_map(|reply| reply.result().ok().map(|sample| sample.key_expr().to_string()))
                .collect()
        };
        assert!(
            wait_until(|| tokens() == vec!["dimos/cam/sensor_msgs.Image".to_owned()]),
            "the relay's token never showed up: {:?}",
            tokens()
        );

        let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = Arc::clone(&received);
        let subscriber = viewer_session
            .declare_subscriber("dimos/cam/sensor_msgs.Image")
            .callback(move |sample: Sample| sink.lock().unwrap().push(sample.payload().to_bytes().to_vec()))
            .wait()
            .unwrap();
        assert!(wait_until(|| relay.wants_payload(channel)), "the remote subscriber never matched");
        relay.forward(channel, b"frame");
        assert!(wait_until(|| !received.lock().unwrap().is_empty()));
        assert_eq!(received.lock().unwrap()[0], b"frame");

        drop(subscriber);
        assert!(wait_until(|| !relay.wants_payload(channel)), "the match never went away");
        assert!(local.lock().unwrap().is_empty(), "relayed frames leaked to a local subscriber: {:?}", local.lock().unwrap());

        // Neither rpc nor the command topic is relayed.
        assert!(!relay.wants_payload("/rpc/Nav/start/req"));
        assert!(!relay.is_relayed("/rpc/Nav/start/req"));
        relay.note_skipped("/tele_cmd_vel_test#geometry_msgs.Twist");
        assert!(!relay.is_relayed("/tele_cmd_vel_test#geometry_msgs.Twist"));
    }
}
