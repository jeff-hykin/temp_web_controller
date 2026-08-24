use crate::image::{self, EncodedFrame};
use crate::msgs::{self, ImageMessage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const MIN_QUALITY: u8 = 25;
const MAX_QUALITY: u8 = 85;
const MIN_WIDTH: usize = 320;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Lcm,
    Zenoh,
}

#[derive(Clone, Serialize)]
pub struct Settings {
    pub linear_speed: f64,
    pub angular_speed: f64,
    pub publish_hz: f64,
    pub deadman_ms: u64,
    pub invert_turn: bool,
    pub auto_quality: bool,
    pub quality: u8,
    pub max_width: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            linear_speed: 0.25,
            angular_speed: 0.5,
            publish_hz: 20.0,
            deadman_ms: 400,
            invert_turn: false,
            auto_quality: true,
            quality: 70,
            max_width: 960,
        }
    }
}

#[derive(Deserialize)]
pub struct SettingsPatch {
    pub linear_speed: Option<f64>,
    pub angular_speed: Option<f64>,
    pub publish_hz: Option<f64>,
    pub deadman_ms: Option<u64>,
    pub invert_turn: Option<bool>,
    pub auto_quality: Option<bool>,
    pub quality: Option<u8>,
    pub max_width: Option<usize>,
}

#[derive(Clone, Copy)]
pub struct Command {
    pub forward: f64,
    pub strafe: f64,
    pub turn: f64,
}

impl Default for Command {
    fn default() -> Self {
        Command {
            forward: 0.0,
            strafe: 0.0,
            turn: 0.0,
        }
    }
}

struct TopicRecord {
    msg_type: Option<String>,
    transport: Transport,
    messages: u64,
    window_messages: u64,
    window_bytes: u64,
    rate: f64,
    bytes_per_second: f64,
    last_seen: Instant,
}

#[derive(Serialize)]
pub struct TopicView {
    pub topic: String,
    pub msg_type: Option<String>,
    pub transport: Transport,
    pub is_image: bool,
    pub rate: f64,
    pub bytes_per_second: f64,
    pub messages: u64,
    pub seconds_since_seen: f64,
}

#[derive(Serialize, Clone, Default)]
pub struct StreamStats {
    pub source_fps: f64,
    pub stream_fps: f64,
    pub dropped: u64,
    pub quality: u8,
    pub max_width: usize,
    pub encode_ms: f64,
    pub jpeg_bytes: usize,
    pub width: usize,
    pub height: usize,
    pub passthrough: bool,
    pub error: Option<String>,
}

struct Pending {
    msg_type: String,
    payload: Vec<u8>,
}

pub struct Stream {
    viewers: AtomicUsize,
    running: AtomicBool,
    slot: Mutex<Option<Pending>>,
    ready: Condvar,
    frames: watch::Sender<Option<Arc<EncodedFrame>>>,
    stats: Mutex<StreamStats>,
    arrived: AtomicUsize,
    encoded: AtomicUsize,
    dropped: AtomicUsize,
}

impl Stream {
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<EncodedFrame>>> {
        self.frames.subscribe()
    }

    pub fn stats(&self) -> StreamStats {
        self.stats.lock().unwrap().clone()
    }
}

pub struct Hub {
    topics: Mutex<HashMap<String, TopicRecord>>,
    streams: RwLock<HashMap<String, Arc<Stream>>>,
    settings: Mutex<Settings>,
    command: Mutex<(Command, Instant)>,
    control_clients: AtomicUsize,
}

impl Hub {
    pub fn new(settings: Settings) -> Arc<Self> {
        Arc::new(Hub {
            topics: Mutex::new(HashMap::new()),
            streams: RwLock::new(HashMap::new()),
            settings: Mutex::new(settings),
            command: Mutex::new((Command::default(), Instant::now())),
            control_clients: AtomicUsize::new(0),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn apply_settings(&self, patch: SettingsPatch) -> Settings {
        let mut settings = self.settings.lock().unwrap();
        if let Some(value) = patch.linear_speed {
            settings.linear_speed = value.clamp(0.0, 3.0);
        }
        if let Some(value) = patch.angular_speed {
            settings.angular_speed = value.clamp(0.0, 6.0);
        }
        if let Some(value) = patch.publish_hz {
            settings.publish_hz = value.clamp(1.0, 100.0);
        }
        if let Some(value) = patch.deadman_ms {
            settings.deadman_ms = value.clamp(100, 5000);
        }
        if let Some(value) = patch.invert_turn {
            settings.invert_turn = value;
        }
        if let Some(value) = patch.auto_quality {
            settings.auto_quality = value;
        }
        if let Some(value) = patch.quality {
            settings.quality = value.clamp(MIN_QUALITY, MAX_QUALITY);
        }
        if let Some(value) = patch.max_width {
            settings.max_width = value.clamp(MIN_WIDTH, 1920);
        }
        settings.clone()
    }

    pub fn set_command(&self, command: Command) {
        *self.command.lock().unwrap() = (command, Instant::now());
    }

    /// The command to publish right now, or zeros once the client has gone quiet.
    pub fn current_command(&self) -> (f64, f64, f64) {
        let settings = self.settings();
        let (command, received) = *self.command.lock().unwrap();
        if received.elapsed() > Duration::from_millis(settings.deadman_ms) {
            return (0.0, 0.0, 0.0);
        }
        let turn_sign = if settings.invert_turn { -1.0 } else { 1.0 };
        (
            command.forward.clamp(-1.0, 1.0) * settings.linear_speed,
            command.strafe.clamp(-1.0, 1.0) * settings.linear_speed,
            command.turn.clamp(-1.0, 1.0) * settings.angular_speed * turn_sign,
        )
    }

    pub fn add_control_client(&self) {
        self.control_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn remove_control_client(&self) {
        self.control_clients.fetch_sub(1, Ordering::Relaxed);
        self.set_command(Command::default());
    }

    pub fn has_control_clients(&self) -> bool {
        self.control_clients.load(Ordering::Relaxed) > 0
    }

    pub fn wants_payload(&self, channel: &str) -> bool {
        let (topic, _) = parse_lcm_channel(channel);
        self.streams
            .read()
            .unwrap()
            .get(&topic)
            .is_some_and(|stream| stream.viewers.load(Ordering::Relaxed) > 0)
    }

    pub fn record_skipped(&self, transport: Transport, channel: &str, bytes: usize) {
        let (topic, msg_type) = parse_lcm_channel(channel);
        self.touch(transport, topic, msg_type, bytes);
    }

    pub fn on_lcm_message(&self, channel: &str, payload: &[u8]) {
        let (topic, msg_type) = parse_lcm_channel(channel);
        self.ingest(Transport::Lcm, topic, msg_type, payload);
    }

    pub fn on_zenoh_message(&self, key_expr: &str, payload: &[u8]) {
        let (topic, msg_type) = parse_zenoh_key(key_expr);
        self.ingest(Transport::Zenoh, topic, msg_type, payload);
    }

    fn ingest(
        &self,
        transport: Transport,
        topic: String,
        msg_type: Option<String>,
        payload: &[u8],
    ) {
        let is_image = msg_type.as_deref().is_some_and(msgs::is_image_type);
        self.touch(transport, topic.clone(), msg_type.clone(), payload.len());
        if !is_image {
            return;
        }
        let stream = self.streams.read().unwrap().get(&topic).cloned();
        let Some(stream) = stream else {
            return;
        };
        if stream.viewers.load(Ordering::Relaxed) == 0 {
            return;
        }
        stream.arrived.fetch_add(1, Ordering::Relaxed);
        let mut slot = stream.slot.lock().unwrap();
        if slot.is_some() {
            stream.dropped.fetch_add(1, Ordering::Relaxed);
        }
        *slot = Some(Pending {
            msg_type: msg_type.unwrap_or_default(),
            payload: payload.to_vec(),
        });
        drop(slot);
        stream.ready.notify_one();
    }

    fn touch(
        &self,
        transport: Transport,
        topic: String,
        msg_type: Option<String>,
        bytes: usize,
    ) {
        let mut topics = self.topics.lock().unwrap();
        let record = topics.entry(topic).or_insert_with(|| TopicRecord {
            msg_type: msg_type.clone(),
            transport,
            messages: 0,
            window_messages: 0,
            window_bytes: 0,
            rate: 0.0,
            bytes_per_second: 0.0,
            last_seen: Instant::now(),
        });
        if record.msg_type.is_none() {
            record.msg_type = msg_type;
        }
        record.transport = transport;
        record.messages += 1;
        record.window_messages += 1;
        record.window_bytes += bytes as u64;
        record.last_seen = Instant::now();
    }

    pub fn tick_rates(&self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64().max(0.001);
        let mut topics = self.topics.lock().unwrap();
        for record in topics.values_mut() {
            record.rate = record.window_messages as f64 / seconds;
            record.bytes_per_second = record.window_bytes as f64 / seconds;
            record.window_messages = 0;
            record.window_bytes = 0;
        }
    }

    pub fn topic_views(&self) -> Vec<TopicView> {
        let topics = self.topics.lock().unwrap();
        let mut views: Vec<TopicView> = topics
            .iter()
            .map(|(topic, record)| TopicView {
                topic: topic.clone(),
                msg_type: record.msg_type.clone(),
                transport: record.transport,
                is_image: record.msg_type.as_deref().is_some_and(msgs::is_image_type),
                rate: record.rate,
                bytes_per_second: record.bytes_per_second,
                messages: record.messages,
                seconds_since_seen: record.last_seen.elapsed().as_secs_f64(),
            })
            .collect();
        views.sort_by(|left, right| left.topic.cmp(&right.topic));
        views
    }

    pub fn stream_stats(&self) -> HashMap<String, StreamStats> {
        self.streams
            .read()
            .unwrap()
            .iter()
            .filter(|(_, stream)| stream.viewers.load(Ordering::Relaxed) > 0)
            .map(|(topic, stream)| (topic.clone(), stream.stats()))
            .collect()
    }

    /// Attach a viewer, starting the encoder thread if this is the first one.
    pub fn open_stream(self: &Arc<Self>, topic: &str) -> Arc<Stream> {
        let topic = normalize(topic);
        let mut streams = self.streams.write().unwrap();
        let stream = streams
            .entry(topic.clone())
            .or_insert_with(|| {
                let (frames, _) = watch::channel(None);
                Arc::new(Stream {
                    viewers: AtomicUsize::new(0),
                    running: AtomicBool::new(false),
                    slot: Mutex::new(None),
                    ready: Condvar::new(),
                    frames,
                    stats: Mutex::new(StreamStats::default()),
                    arrived: AtomicUsize::new(0),
                    encoded: AtomicUsize::new(0),
                    dropped: AtomicUsize::new(0),
                })
            })
            .clone();
        stream.viewers.fetch_add(1, Ordering::Relaxed);
        if !stream.running.swap(true, Ordering::SeqCst) {
            let hub = Arc::clone(self);
            let encoder_stream = Arc::clone(&stream);
            std::thread::Builder::new()
                .name(format!("encode {topic}"))
                .spawn(move || run_encoder(hub, encoder_stream))
                .expect("failed to spawn encoder thread");
        }
        stream
    }

    pub fn close_stream(&self, stream: &Arc<Stream>) {
        stream.viewers.fetch_sub(1, Ordering::Relaxed);
        stream.ready.notify_all();
    }
}

fn run_encoder(hub: Arc<Hub>, stream: Arc<Stream>) {
    let mut quality = hub.settings().quality;
    let mut max_width = hub.settings().max_width;
    let mut window_start = Instant::now();
    let mut healthy_windows = 0;

    loop {
        let pending = {
            let mut slot = stream.slot.lock().unwrap();
            while slot.is_none() {
                if stream.viewers.load(Ordering::Relaxed) == 0 {
                    stream.running.store(false, Ordering::SeqCst);
                    return;
                }
                let (guard, _) = stream
                    .ready
                    .wait_timeout(slot, Duration::from_millis(250))
                    .unwrap();
                slot = guard;
            }
            slot.take().unwrap()
        };

        let settings = hub.settings();
        if !settings.auto_quality {
            quality = settings.quality;
            max_width = settings.max_width;
        }

        let started = Instant::now();
        let decoded = msgs::decode_any_image(&pending.msg_type, &pending.payload);
        let outcome = match decoded {
            Ok(ImageMessage::Compressed(compressed)) => {
                if compressed.format.contains("jpeg") || compressed.format.contains("jpg") {
                    Ok((
                        EncodedFrame {
                            jpeg: bytes::Bytes::from(compressed.data),
                            width: 0,
                            height: 0,
                        },
                        true,
                    ))
                } else {
                    Err(anyhow::anyhow!(
                        "compressed format {} is not viewable",
                        compressed.format
                    ))
                }
            }
            Ok(ImageMessage::Raw(raw)) => {
                image::encode(&raw, quality, max_width).map(|frame| (frame, false))
            }
            Err(error) => Err(error),
        };

        let mut stats = stream.stats.lock().unwrap();
        match outcome {
            Ok((frame, passthrough)) => {
                stats.encode_ms = started.elapsed().as_secs_f64() * 1000.0;
                stats.jpeg_bytes = frame.jpeg.len();
                stats.width = frame.width;
                stats.height = frame.height;
                stats.passthrough = passthrough;
                stats.quality = quality;
                stats.max_width = max_width;
                stats.error = None;
                drop(stats);
                stream.encoded.fetch_add(1, Ordering::Relaxed);
                let _ = stream.frames.send(Some(Arc::new(frame)));
            }
            Err(error) => {
                stats.error = Some(error.to_string());
                drop(stats);
            }
        }

        if window_start.elapsed() >= Duration::from_secs(1) {
            let elapsed = window_start.elapsed().as_secs_f64();
            let arrived = stream.arrived.swap(0, Ordering::Relaxed);
            let encoded = stream.encoded.swap(0, Ordering::Relaxed);
            let dropped = stream.dropped.swap(0, Ordering::Relaxed) as u64;
            let mut stats = stream.stats.lock().unwrap();
            stats.source_fps = arrived as f64 / elapsed;
            stats.stream_fps = encoded as f64 / elapsed;
            stats.dropped += dropped;
            drop(stats);

            if hub.settings().auto_quality {
                let keeping_up = arrived == 0 || encoded as f64 >= arrived as f64 * 0.9;
                if keeping_up {
                    healthy_windows += 1;
                    if healthy_windows >= 3 {
                        healthy_windows = 0;
                        if max_width < hub.settings().max_width {
                            max_width = (max_width * 2).min(hub.settings().max_width);
                        } else if quality < MAX_QUALITY {
                            quality = (quality + 5).min(MAX_QUALITY);
                        }
                    }
                } else {
                    healthy_windows = 0;
                    if quality > MIN_QUALITY {
                        quality = quality.saturating_sub(10).max(MIN_QUALITY);
                    } else if max_width > MIN_WIDTH {
                        max_width = (max_width / 2).max(MIN_WIDTH);
                    }
                }
            }
            window_start = Instant::now();
        }
    }
}

pub fn normalize(topic: &str) -> String {
    topic.trim_start_matches('/').to_owned()
}

pub fn parse_lcm_channel(channel: &str) -> (String, Option<String>) {
    match channel.rsplit_once('#') {
        Some((topic, msg_type)) => (normalize(topic), Some(msg_type.to_owned())),
        None => (normalize(channel), None),
    }
}

/// Zenoh key expressions carry the type as a trailing path segment
/// (`dimos/cmd_vel/geometry_msgs.Twist`), so a dotted last segment is the type.
pub fn parse_zenoh_key(key_expr: &str) -> (String, Option<String>) {
    match key_expr.rsplit_once('/') {
        Some((topic, last)) if last.contains('.') && !last.is_empty() => {
            (normalize(topic), Some(last.to_owned()))
        }
        _ => (normalize(key_expr), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_parsing_strips_the_type_suffix() {
        assert_eq!(
            parse_lcm_channel("/image#sensor_msgs.Image"),
            ("image".to_owned(), Some("sensor_msgs.Image".to_owned()))
        );
        assert_eq!(parse_lcm_channel("/odom"), ("odom".to_owned(), None));
    }

    #[test]
    fn zenoh_keys_only_split_on_a_dotted_last_segment() {
        assert_eq!(
            parse_zenoh_key("dimos/cmd_vel/geometry_msgs.Twist"),
            ("dimos/cmd_vel".to_owned(), Some("geometry_msgs.Twist".to_owned()))
        );
        assert_eq!(
            parse_zenoh_key("dimos/cmd_vel"),
            ("dimos/cmd_vel".to_owned(), None)
        );
    }

    #[test]
    fn lcm_and_zenoh_names_land_on_one_topic() {
        let (from_lcm, _) = parse_lcm_channel("/image#sensor_msgs.Image");
        let (from_zenoh, _) = parse_zenoh_key("image/sensor_msgs.Image");
        assert_eq!(from_lcm, from_zenoh);
    }

    #[test]
    fn commands_scale_by_the_configured_speeds() {
        let hub = Hub::new(Settings::default());
        hub.set_command(Command {
            forward: 1.0,
            strafe: 0.0,
            turn: -0.5,
        });
        let (linear_x, _, angular_z) = hub.current_command();
        assert!((linear_x - 0.25).abs() < 1e-9);
        assert!((angular_z + 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_stale_command_becomes_zero() {
        let hub = Hub::new(Settings {
            deadman_ms: 100,
            ..Settings::default()
        });
        hub.set_command(Command {
            forward: 1.0,
            strafe: 0.0,
            turn: 1.0,
        });
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(hub.current_command(), (0.0, 0.0, 0.0));
    }

    #[test]
    fn only_watched_image_topics_are_wanted() {
        let hub = Hub::new(Settings::default());
        assert!(!hub.wants_payload("/image#sensor_msgs.Image"));
        let stream = hub.open_stream("/image");
        assert!(hub.wants_payload("/image#sensor_msgs.Image"));
        hub.close_stream(&stream);
        assert!(!hub.wants_payload("/image#sensor_msgs.Image"));
    }

    #[test]
    fn discovery_records_topics_from_both_transports() {
        let hub = Hub::new(Settings::default());
        hub.on_lcm_message("/odom#nav_msgs.Odometry", &[0; 32]);
        hub.on_zenoh_message("scan/sensor_msgs.LaserScan", &[0; 16]);
        hub.tick_rates(Duration::from_secs(1));
        let views = hub.topic_views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].topic, "odom");
        assert_eq!(views[0].transport, Transport::Lcm);
        assert!((views[0].rate - 1.0).abs() < 1e-9);
        assert_eq!(views[1].transport, Transport::Zenoh);
        assert!(!views[1].is_image);
    }
}
