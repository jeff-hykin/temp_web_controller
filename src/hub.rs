use crate::image::{self, EncodedFrame, ImageFormat};
use crate::msgs::{self, ImageMessage};
use crate::record::{self, Compression, Recorder, RecordingStatus};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const MIN_QUALITY: u8 = 25;
const MAX_QUALITY: u8 = 85;
const MIN_WIDTH: usize = 320;

/// Latency budget, measured from the sender's own stamp to the moment we are about
/// to encode, so it covers the robot's pipeline and any backlog of ours. Driving is
/// comfortable under 150 ms, visibly laggy past 300, and a frame half a second old
/// is no longer worth the CPU to encode.
const LATENCY_GOOD_MS: f64 = 150.0;
const LATENCY_HIGH_MS: f64 = 300.0;
const LATENCY_DROP_MS: f64 = 500.0;
/// Past this the sender's clock disagrees with ours rather than the link being slow,
/// and a skewed clock must not be allowed to pin quality to the floor forever.
const MAX_PLAUSIBLE_LATENCY_MS: f64 = 10_000.0;
/// Dropping every late frame would leave a permanently black tile when the delay is
/// upstream of us, so the view still refreshes even while it is behind.
const MAX_CONSECUTIVE_LATE_DROPS: u32 = 4;

const TF_STALE: Duration = Duration::from_secs(10);
const TF_FORGET: Duration = Duration::from_secs(120);
const TF_MAX_EDGES: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Lcm,
    Zenoh,
}

#[derive(Clone, Serialize)]
pub struct Settings {
    pub publish_topic: String,
    pub linear_speed: f64,
    pub angular_speed: f64,
    pub publish_hz: f64,
    pub deadman_ms: u64,
    pub invert_turn: bool,
    pub auto_quality: bool,
    pub quality: u8,
    pub max_width: usize,
    pub record_compression: Compression,
    pub record_image_format: ImageFormat,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            publish_topic: "/tele_cmd_vel".to_owned(),
            linear_speed: 0.25,
            angular_speed: 0.5,
            publish_hz: 20.0,
            deadman_ms: 400,
            invert_turn: false,
            auto_quality: true,
            quality: 70,
            max_width: 960,
            record_compression: Compression::default(),
            record_image_format: ImageFormat::default(),
        }
    }
}

#[derive(Deserialize)]
pub struct SettingsPatch {
    pub publish_topic: Option<String>,
    pub linear_speed: Option<f64>,
    pub angular_speed: Option<f64>,
    pub publish_hz: Option<f64>,
    pub deadman_ms: Option<u64>,
    pub invert_turn: Option<bool>,
    pub auto_quality: Option<bool>,
    pub quality: Option<u8>,
    pub max_width: Option<usize>,
    pub record_compression: Option<Compression>,
    pub record_image_format: Option<ImageFormat>,
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
    unclassifiable: u64,
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
    pub recorded: bool,
    pub is_rpc: bool,
    pub unclassifiable: u64,
}

#[derive(Serialize, Clone, Default)]
pub struct StreamStats {
    pub source_fps: f64,
    pub stream_fps: f64,
    /// What one viewer's socket actually swallowed. A client on weak wifi sits far
    /// below `stream_fps`, and that gap is invisible in every other number here.
    pub client_fps: f64,
    /// What the browser managed to draw, which it reports back because nothing on
    /// this side can see a phone whose jpeg decoder is the bottleneck. `None` until
    /// a viewer has said.
    pub painted_fps: Option<f64>,
    pub dropped: u64,
    pub quality: u8,
    pub max_width: usize,
    pub encode_ms: f64,
    pub jpeg_bytes: usize,
    pub width: usize,
    pub height: usize,
    pub passthrough: bool,
    /// Age of the frame against the sender's clock. `None` when the publisher does
    /// not stamp its frames or its clock disagrees with ours.
    pub latency_ms: Option<f64>,
    pub late_dropped: u64,
    pub error: Option<String>,
}

struct TfEdgeRecord {
    last_seen: Instant,
    is_static: bool,
    messages: u64,
}

#[derive(Serialize)]
pub struct TfLink {
    pub parent: String,
    pub child: String,
    pub is_static: bool,
    pub messages: u64,
    pub seconds_since_seen: f64,
    pub stale: bool,
}

#[derive(Serialize, Default)]
pub struct TfView {
    pub links: Vec<TfLink>,
    pub roots: Vec<String>,
    pub warnings: Vec<String>,
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
    delivered: AtomicUsize,
    painted: Mutex<Option<(Instant, f64)>>,
}

impl Stream {
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<EncodedFrame>>> {
        self.frames.subscribe()
    }

    pub fn on_delivered(&self) {
        self.delivered.fetch_add(1, Ordering::Relaxed);
    }

    fn report_painted(&self, fps: f64) {
        *self.painted.lock().unwrap() = Some((Instant::now(), fps));
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
    tf: Mutex<HashMap<(String, String), TfEdgeRecord>>,
    recorder: Mutex<Option<Recorder>>,
    /// Only topics somebody actually toggled, so a topic that appears
    /// mid-recording still lands on its default rather than on a stale answer.
    recording_overrides: RwLock<HashMap<String, bool>>,
    record_dir: PathBuf,
}

impl Hub {
    pub fn new(settings: Settings, record_dir: PathBuf) -> Arc<Self> {
        Arc::new(Hub {
            topics: Mutex::new(HashMap::new()),
            streams: RwLock::new(HashMap::new()),
            settings: Mutex::new(settings),
            command: Mutex::new((Command::default(), Instant::now())),
            tf: Mutex::new(HashMap::new()),
            recorder: Mutex::new(None),
            recording_overrides: RwLock::new(HashMap::new()),
            record_dir,
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn apply_settings(&self, patch: SettingsPatch) -> Settings {
        let mut settings = self.settings.lock().unwrap();
        if let Some(value) = patch.publish_topic.as_deref().and_then(command_topic) {
            settings.publish_topic = value;
        }
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
        if let Some(value) = patch.record_compression {
            settings.record_compression = value;
        }
        if let Some(value) = patch.record_image_format {
            settings.record_image_format = value;
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
        let turn_sign = if settings.invert_turn { 1.0 } else { -1.0 };
        (
            command.forward.clamp(-1.0, 1.0) * settings.linear_speed,
            // The browser sends screen-space axes where right is positive, but
            // REP-103 puts +y to the left and +yaw counter-clockwise, so both flip
            // here rather than in three separate places in the frontend.
            -command.strafe.clamp(-1.0, 1.0) * settings.linear_speed,
            command.turn.clamp(-1.0, 1.0) * settings.angular_speed * turn_sign,
        )
    }

    /// A browser that closed its socket cannot steer any more, and the last thing
    /// it sent may well have been a drive command.
    pub fn on_control_disconnect(&self) {
        self.set_command(Command::default());
    }

    pub fn wants_payload(&self, channel: &str) -> bool {
        let (topic, msg_type) = parse_lcm_channel(channel);
        if msg_type.as_deref() == Some(msgs::TF_TYPE) {
            return true;
        }
        // A recording must be complete, so the viewer-based drop optimization
        // is suspended for whatever the recording is actually capturing.
        if self.is_recording() && self.is_topic_recorded(&topic) {
            return true;
        }
        self.streams
            .read()
            .unwrap()
            .get(&topic)
            .is_some_and(|stream| stream.viewers.load(Ordering::Relaxed) > 0)
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.lock().unwrap().is_some()
    }

    pub fn is_topic_recorded(&self, topic: &str) -> bool {
        match self.recording_overrides.read().unwrap().get(topic) {
            Some(&recorded) => recorded,
            None => !is_rpc_topic(topic),
        }
    }

    pub fn set_topic_recorded(&self, topic: &str, recorded: bool) {
        self.recording_overrides
            .write()
            .unwrap()
            .insert(topic.to_string(), recorded);
    }

    pub fn recording_status(&self) -> RecordingStatus {
        match self.recorder.lock().unwrap().as_ref() {
            Some(recorder) => recorder.status(),
            None => record::idle_status(),
        }
    }

    pub fn start_recording(&self, name: Option<&str>) -> Result<RecordingStatus> {
        let name = name.map(str::to_string).unwrap_or_else(record::default_name);
        let path = record::resolve(&self.record_dir, &name)?;
        let mut slot = self.recorder.lock().unwrap();
        if slot.is_some() {
            bail!("already recording");
        }
        let settings = self.settings();
        let recorder =
            Recorder::start(&path, settings.record_compression, settings.record_image_format)?;
        let status = recorder.status();
        *slot = Some(recorder);
        Ok(status)
    }

    pub fn list_recordings(&self) -> Vec<record::RecordingFile> {
        record::list(&self.record_dir)
    }

    pub fn delete_recording(&self, name: &str) -> Result<()> {
        let path = record::resolve(&self.record_dir, name)?;
        if self
            .recorder
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|recorder| recorder.is_writing_to(&path))
        {
            bail!("that recording is still being written");
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    pub fn stop_recording(&self) -> Result<RecordingStatus> {
        let recorder = self.recorder.lock().unwrap().take();
        match recorder {
            Some(recorder) => recorder.finish(),
            None => bail!("not recording"),
        }
    }

    pub fn record_skipped(&self, transport: Transport, channel: &str, bytes: usize) {
        let (topic, msg_type) = parse_lcm_channel(channel);
        self.touch(transport, topic, msg_type, bytes, false);
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
        let parts = match msg_type.as_deref() {
            Some(msgs::IMAGE_TYPE) => msgs::read_raw_image(payload).ok(),
            _ => None,
        };
        let unclassifiable = parts.as_ref().is_some_and(|parts| {
            image::frame_is_unclassifiable(parts.height, parts.step, parts.data)
        });
        let first_bad_frame = self.touch(
            transport,
            topic.clone(),
            msg_type.clone(),
            payload.len(),
            unclassifiable,
        );
        if let (true, Some(parts)) = (first_bad_frame, &parts) {
            eprintln!(
                "{topic}: image frame is neither {}x{} pixels nor a container we recognise \
                 (encoding {:?}, {} bytes); recording it as-is",
                parts.height,
                parts.step,
                parts.encoding,
                parts.data.len()
            );
        }
        if msg_type.as_deref() == Some(msgs::TF_TYPE) {
            self.record_tf(&topic, payload);
        }
        if self.is_topic_recorded(&topic) {
            if let Some(recorder) = self.recorder.lock().unwrap().as_ref() {
                recorder.offer(&topic, msg_type.as_deref(), payload);
            }
        }
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

    fn record_tf(&self, topic: &str, payload: &[u8]) {
        let Ok(edges) = msgs::decode_tf(payload) else {
            return;
        };
        let is_static = topic.contains("static");
        let now = Instant::now();
        let mut tf = self.tf.lock().unwrap();
        tf.retain(|_, record| record.is_static || record.last_seen.elapsed() < TF_FORGET);
        for edge in edges {
            let key = (normalize(&edge.child), normalize(&edge.parent));
            if key.0.is_empty() || key.1.is_empty() {
                continue;
            }
            if !tf.contains_key(&key) && tf.len() >= TF_MAX_EDGES {
                continue;
            }
            let record = tf.entry(key).or_insert(TfEdgeRecord {
                last_seen: now,
                is_static,
                messages: 0,
            });
            record.last_seen = now;
            record.is_static = is_static;
            record.messages += 1;
        }
    }

    pub fn tf_view(&self) -> TfView {
        let tf = self.tf.lock().unwrap();
        let mut links: Vec<TfLink> = tf
            .iter()
            .map(|((child, parent), record)| TfLink {
                parent: parent.clone(),
                child: child.clone(),
                is_static: record.is_static,
                messages: record.messages,
                seconds_since_seen: record.last_seen.elapsed().as_secs_f64(),
                stale: !record.is_static && record.last_seen.elapsed() > TF_STALE,
            })
            .collect();
        drop(tf);
        links.sort_by(|left, right| {
            (&left.parent, &left.child).cmp(&(&right.parent, &right.child))
        });

        let mut parents_of: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut frames: BTreeSet<&str> = BTreeSet::new();
        for link in &links {
            parents_of.entry(&link.child).or_default().push(&link.parent);
            children_of.entry(&link.parent).or_default().push(&link.child);
            frames.insert(&link.parent);
            frames.insert(&link.child);
        }

        let mut warnings = Vec::new();
        for (child, parents) in &parents_of {
            if parents.len() > 1 {
                let mut named: Vec<&str> = parents.clone();
                named.sort_unstable();
                warnings.push(format!(
                    "{child} has {} parents: {}",
                    named.len(),
                    named.join(", ")
                ));
            }
        }

        let roots: Vec<String> = frames
            .iter()
            .filter(|frame| !parents_of.contains_key(**frame))
            .map(|frame| (*frame).to_owned())
            .collect();

        let mut reached: HashSet<&str> = HashSet::new();
        let mut queue: Vec<&str> = roots.iter().map(|frame| frame.as_str()).collect();
        while let Some(frame) = queue.pop() {
            if !reached.insert(frame) {
                continue;
            }
            if let Some(children) = children_of.get(frame) {
                queue.extend(children.iter().copied());
            }
        }

        let orphans: Vec<&str> = frames
            .iter()
            .filter(|frame| !reached.contains(**frame))
            .copied()
            .collect();
        if !orphans.is_empty() {
            warnings.push(format!("cycle in tf, unreachable frames: {}", orphans.join(", ")));
        }
        if roots.len() > 1 {
            warnings.push(format!(
                "tf is disjoint, {} separate trees rooted at: {}",
                roots.len(),
                roots.join(", ")
            ));
        }
        for link in links.iter().filter(|link| link.stale) {
            warnings.push(format!(
                "{} -> {} last seen {:.0}s ago",
                link.parent, link.child, link.seconds_since_seen
            ));
        }
        warnings.sort();

        TfView {
            links,
            roots,
            warnings,
        }
    }

    fn touch(
        &self,
        transport: Transport,
        topic: String,
        msg_type: Option<String>,
        bytes: usize,
        unclassifiable: bool,
    ) -> bool {
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
            unclassifiable: 0,
        });
        let first_bad_frame = unclassifiable && record.unclassifiable == 0;
        record.unclassifiable += unclassifiable as u64;
        if record.msg_type.is_none() {
            record.msg_type = msg_type;
        }
        record.transport = transport;
        record.messages += 1;
        record.window_messages += 1;
        record.window_bytes += bytes as u64;
        record.last_seen = Instant::now();
        first_bad_frame
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
                recorded: self.is_topic_recorded(topic),
                is_rpc: is_rpc_topic(topic),
                unclassifiable: record.unclassifiable,
            })
            .collect();
        views.sort_by(|left, right| {
            (left.is_rpc, &left.topic).cmp(&(right.is_rpc, &right.topic))
        });
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
                    delivered: AtomicUsize::new(0),
                    painted: Mutex::new(None),
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

    pub fn report_painted(&self, topic: &str, fps: f64) {
        if let Some(stream) = self.streams.read().unwrap().get(&normalize(topic)) {
            stream.report_painted(fps);
        }
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
    let mut consecutive_late_drops = 0;
    loop {
        let pending = {
            let mut slot = stream.slot.lock().unwrap();
            while slot.is_none() {
                if stream.viewers.load(Ordering::Relaxed) == 0 {
                    stream.running.store(false, Ordering::SeqCst);
                    return;
                }
                // Rates are rolled here as well as after a frame, or a publisher
                // that stops leaves its last window standing and /api/status keeps
                // reporting the fps it had when it died.
                if window_start.elapsed() >= Duration::from_secs(1) {
                    roll_window(&stream, &mut window_start);
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
        let latency_ms = decoded.as_ref().ok().and_then(frame_age_ms);

        // Encoding a frame this old only pushes the next one further behind, so the
        // backlog is thrown away rather than worked through. Unwinding the LCM
        // message above is cheap; the scale and jpeg encode below are not.
        let too_late = latency_ms.is_some_and(|age| age > LATENCY_DROP_MS)
            && consecutive_late_drops < MAX_CONSECUTIVE_LATE_DROPS;
        if too_late {
            consecutive_late_drops += 1;
            let mut stats = stream.stats.lock().unwrap();
            stats.latency_ms = latency_ms;
            stats.late_dropped += 1;
            continue;
        }
        consecutive_late_drops = 0;

        // A latency spike is answered on the frame that shows it rather than at the
        // next one-second window, which is far too slow to catch up from.
        if settings.auto_quality && latency_ms.is_some_and(|age| age > LATENCY_HIGH_MS) {
            healthy_windows = 0;
            if quality > MIN_QUALITY {
                quality = quality.saturating_sub(20).max(MIN_QUALITY);
            } else if max_width > MIN_WIDTH {
                max_width = (max_width / 2).max(MIN_WIDTH);
            }
        }

        let outcome = match decoded {
            Ok(ImageMessage::Compressed(compressed)) => {
                image::encode_compressed(&compressed, quality, max_width)
            }
            Ok(ImageMessage::Raw(raw)) => image::encode(&raw, quality, max_width),
            Err(error) => Err(error),
        };

        let mut stats = stream.stats.lock().unwrap();
        match outcome {
            Ok(frame) => {
                stats.encode_ms = started.elapsed().as_secs_f64() * 1000.0;
                stats.jpeg_bytes = frame.jpeg.len();
                stats.width = frame.width;
                stats.height = frame.height;
                stats.passthrough = frame.passthrough;
                stats.quality = quality;
                stats.max_width = max_width;
                stats.latency_ms = latency_ms;
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
            let window = roll_window(&stream, &mut window_start);

            if hub.settings().auto_quality {
                // Throughput alone would climb straight back into a latency spike,
                // since a backlog can be drained at full rate and still be a second old.
                let prompt = latency_ms.is_none_or(|age| age < LATENCY_GOOD_MS);
                let encoding_keeps_up =
                    prompt && (window.arrived == 0 || window.encoded as f64 >= window.arrived as f64 * 0.9);
                let (viewer_share, limit) = window.viewer_shortfall();
                if encoding_keeps_up && viewer_share >= VIEWER_TARGET_SHARE {
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
                    // Cut in proportion to how far behind the viewer is: a browser
                    // getting a fifth of the frames needs a step it can feel this
                    // second, not six windows of shaving ten off the quality.
                    let cut = (30.0 * (1.0 - viewer_share)).round().max(10.0) as u8;
                    match limit {
                        // The link cannot carry the bytes, so make them smaller.
                        ViewerLimit::Link if quality > MIN_QUALITY => {
                            quality = quality.saturating_sub(cut).max(MIN_QUALITY);
                        }
                        // Bytes arrive fine and the decoder is what cannot keep up,
                        // so there are fewer pixels to spend, not fewer bits.
                        ViewerLimit::Decode if max_width > MIN_WIDTH => {
                            max_width = (max_width / 2).max(MIN_WIDTH);
                        }
                        _ if quality > MIN_QUALITY => {
                            quality = quality.saturating_sub(cut).max(MIN_QUALITY);
                        }
                        _ => max_width = (max_width / 2).max(MIN_WIDTH),
                    }
                }
            }
        }
    }
}

/// How much of what we encode a viewer has to actually see before the stream counts
/// as healthy. Below this the controller gives up quality to close the gap.
const VIEWER_TARGET_SHARE: f64 = 0.9;
const PAINTED_REPORT_STALE: Duration = Duration::from_secs(3);

enum ViewerLimit {
    Link,
    Decode,
    None,
}

struct Window {
    arrived: usize,
    encoded: usize,
    delivered: usize,
    painted_fps: Option<f64>,
    elapsed: f64,
}

impl Window {
    /// The fraction of encoded frames the viewer actually got, and which side of the
    /// wire lost the rest.
    fn viewer_shortfall(&self) -> (f64, ViewerLimit) {
        if self.encoded == 0 {
            return (1.0, ViewerLimit::None);
        }
        let encoded_fps = self.encoded as f64 / self.elapsed;
        let delivered_fps = self.delivered as f64 / self.elapsed;
        let seen_fps = match self.painted_fps {
            Some(painted) => painted.min(delivered_fps),
            None => delivered_fps,
        };
        let share = (seen_fps / encoded_fps).clamp(0.0, 1.0);
        let limit = if share >= VIEWER_TARGET_SHARE {
            ViewerLimit::None
        } else if delivered_fps < encoded_fps * VIEWER_TARGET_SHARE {
            ViewerLimit::Link
        } else {
            ViewerLimit::Decode
        };
        (share, limit)
    }
}

/// Closes the one-second measurement window and publishes the rates. Called both
/// after a frame and while idling, so a publisher that stops cannot leave its last
/// window standing.
fn roll_window(stream: &Stream, window_start: &mut Instant) -> Window {
    let elapsed = window_start.elapsed().as_secs_f64();
    let arrived = stream.arrived.swap(0, Ordering::Relaxed);
    let encoded = stream.encoded.swap(0, Ordering::Relaxed);
    let dropped = stream.dropped.swap(0, Ordering::Relaxed) as u64;
    let viewers = stream.viewers.load(Ordering::Relaxed).max(1);
    let delivered = stream.delivered.swap(0, Ordering::Relaxed) / viewers;
    // A viewer that stopped reporting must stop steering the controller, or a tab
    // that was closed mid-struggle pins the quality down for everyone after it.
    let painted_fps = stream
        .painted
        .lock()
        .unwrap()
        .filter(|(at, _)| at.elapsed() < PAINTED_REPORT_STALE)
        .map(|(_, fps)| fps);
    let mut stats = stream.stats.lock().unwrap();
    stats.source_fps = arrived as f64 / elapsed;
    stats.stream_fps = encoded as f64 / elapsed;
    stats.client_fps = delivered as f64 / elapsed;
    stats.painted_fps = painted_fps;
    stats.dropped += dropped;
    drop(stats);
    *window_start = Instant::now();
    Window { arrived, encoded, delivered, painted_fps, elapsed }
}

/// How far behind the sender's own stamp this frame is. `None` when there is no
/// usable stamp, which must not read as "zero latency": an unstamped publisher
/// sends zeros, and a sender whose clock is ahead of ours produces a negative age.
/// Both would otherwise drive the controller off a number that means nothing.
fn frame_age_ms(message: &ImageMessage) -> Option<f64> {
    let header = match message {
        ImageMessage::Raw(image) => &image.header,
        ImageMessage::Compressed(image) => &image.header,
    };
    if header.stamp_sec <= 0 || header.stamp_nsec < 0 {
        return None;
    }
    let stamped = UNIX_EPOCH + Duration::new(header.stamp_sec as u64, header.stamp_nsec as u32);
    let age = SystemTime::now().duration_since(stamped).ok()?.as_secs_f64() * 1000.0;
    (age < MAX_PLAUSIBLE_LATENCY_MS).then_some(age)
}

pub fn normalize(topic: &str) -> String {
    topic.trim_start_matches('/').to_owned()
}

/// dimos publishes every service call as a pair of `rpc/<Service>/<method>/{req,res}`
/// topics. There are dozens of them and they bury the topics worth recording,
/// so they sort last and stay off unless somebody asks for them.
pub fn is_rpc_topic(topic: &str) -> bool {
    let topic = normalize(topic);
    topic.starts_with("rpc/") || topic.contains("/rpc/")
}

/// Cleans up a topic name typed into the settings drawer. `None` means the name
/// is unusable and the current one should be kept, since a robot that quietly
/// stops receiving commands is worse than one that ignores a typo. `#` is
/// refused because it is the separator in an LCM channel name.
pub fn command_topic(name: &str) -> Option<String> {
    let trimmed = normalize(name.trim());
    if trimmed.is_empty() || trimmed.contains('#') || trimmed.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("/{trimmed}"))
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
    fn rpc_topics_default_to_unrecorded_but_stay_overridable() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        assert!(!hub.is_topic_recorded("rpc/AlfredHighLevel/start/req"));
        assert!(!hub.is_topic_recorded("/alfred/rpc/CalibRecorder/build/res"));
        assert!(hub.is_topic_recorded("pointlio_odometry"));
        assert!(hub.is_topic_recorded("rpc_status"));

        hub.set_topic_recorded("rpc/AlfredHighLevel/start/req", true);
        assert!(hub.is_topic_recorded("rpc/AlfredHighLevel/start/req"));
        hub.set_topic_recorded("pointlio_odometry", false);
        assert!(!hub.is_topic_recorded("pointlio_odometry"));
    }

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
    fn a_renamed_command_topic_gets_a_leading_slash_and_a_bad_one_is_refused() {
        assert_eq!(command_topic("cmd_vel").as_deref(), Some("/cmd_vel"));
        assert_eq!(command_topic("  /cmd_vel ").as_deref(), Some("/cmd_vel"));
        assert_eq!(command_topic(""), None);
        assert_eq!(command_topic("cmd vel"), None);
        assert_eq!(command_topic("cmd_vel#geometry_msgs.Twist"), None);

        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.apply_settings(SettingsPatch {
            publish_topic: Some("alfred/cmd_vel".to_owned()),
            ..empty_patch()
        });
        assert_eq!(hub.settings().publish_topic, "/alfred/cmd_vel");

        hub.apply_settings(SettingsPatch {
            publish_topic: Some(String::new()),
            ..empty_patch()
        });
        assert_eq!(hub.settings().publish_topic, "/alfred/cmd_vel");
    }

    fn empty_patch() -> SettingsPatch {
        SettingsPatch {
            publish_topic: None,
            linear_speed: None,
            angular_speed: None,
            publish_hz: None,
            deadman_ms: None,
            invert_turn: None,
            auto_quality: None,
            quality: None,
            max_width: None,
            record_compression: None,
            record_image_format: None,
        }
    }

    #[test]
    fn commands_scale_by_the_configured_speeds() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.set_command(Command {
            forward: 1.0,
            strafe: 0.0,
            turn: -0.5,
        });
        let (linear_x, _, angular_z) = hub.current_command();
        assert!((linear_x - 0.25).abs() < 1e-9);
        assert!((angular_z - 0.25).abs() < 1e-9);
    }

    /// dimos's own keyboard teleop sends `angular.z = +speed` for A and
    /// `linear.y = +speed` for Q, so screen-right has to leave here negative or
    /// the robot turns and strafes the opposite way from every other teleop source.
    #[test]
    fn screen_right_turns_and_strafes_right_in_rep_103() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.set_command(Command {
            forward: 0.0,
            strafe: 1.0,
            turn: 1.0,
        });
        let (_, linear_y, angular_z) = hub.current_command();
        assert!(linear_y < 0.0);
        assert!(angular_z < 0.0);
    }

    #[test]
    fn a_stale_command_becomes_zero() {
        let hub = Hub::new(
            Settings {
                deadman_ms: 100,
                ..Settings::default()
            },
            std::env::temp_dir(),
        );
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
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        assert!(!hub.wants_payload("/image#sensor_msgs.Image"));
        let stream = hub.open_stream("/image");
        assert!(hub.wants_payload("/image#sensor_msgs.Image"));
        hub.close_stream(&stream);
        assert!(!hub.wants_payload("/image#sensor_msgs.Image"));
    }

    #[test]
    fn a_healthy_tf_tree_has_one_root_and_no_warnings() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.on_lcm_message(
            "/tf#tf2_msgs.TFMessage",
            &tf_payload(&[("map", "odom"), ("odom", "base_link")]),
        );
        let view = hub.tf_view();
        assert_eq!(view.roots, vec!["map".to_owned()]);
        assert_eq!(view.links.len(), 2);
        assert!(view.warnings.is_empty(), "{:?}", view.warnings);
    }

    #[test]
    fn a_child_with_two_parents_is_reported() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.on_lcm_message(
            "/tf#tf2_msgs.TFMessage",
            &tf_payload(&[("map", "base_link"), ("odom", "base_link")]),
        );
        let warnings = hub.tf_view().warnings;
        assert!(
            warnings.iter().any(|warning| warning == "base_link has 2 parents: map, odom"),
            "{warnings:?}"
        );
    }

    #[test]
    fn disconnected_trees_are_reported() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.on_lcm_message(
            "/tf#tf2_msgs.TFMessage",
            &tf_payload(&[("map", "base_link"), ("camera", "lens")]),
        );
        let view = hub.tf_view();
        assert_eq!(view.roots, vec!["camera".to_owned(), "map".to_owned()]);
        assert!(view.warnings.iter().any(|warning| warning.contains("disjoint")));
    }

    #[test]
    fn a_cycle_is_reported() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        hub.on_lcm_message(
            "/tf#tf2_msgs.TFMessage",
            &tf_payload(&[("a", "b"), ("b", "c"), ("c", "a")]),
        );
        let view = hub.tf_view();
        assert!(view.roots.is_empty());
        assert!(view.warnings.iter().any(|warning| warning.contains("cycle")));
    }

    fn tf_payload(edges: &[(&str, &str)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&msgs::TF_FINGERPRINT);
        payload.extend_from_slice(&(edges.len() as i32).to_be_bytes());
        for (parent, child) in edges {
            payload.extend_from_slice(&[0u8; 12]);
            for name in [parent, child] {
                payload.extend_from_slice(&((name.len() + 1) as u32).to_be_bytes());
                payload.extend_from_slice(name.as_bytes());
                payload.push(0);
            }
            payload.extend_from_slice(&[0u8; 56]);
        }
        payload
    }

    /// A frame that is neither the right size for its own dimensions nor a
    /// container we know is still recorded, so this counter is the only place the
    /// defect is visible before someone opens the file.
    #[test]
    fn frames_that_cannot_be_classified_are_counted_per_topic() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
        let channel = "/cam#sensor_msgs.Image";
        // 4 rows of step 12 want 48 bytes and only 12 arrive, with nothing
        // recognisable at the front: a truncated frame or a codec we do not sniff.
        let truncated = image_payload(4, 4, 12, "rgb8", &[3u8; 12]);
        hub.on_lcm_message(channel, &truncated);
        hub.on_lcm_message(channel, &truncated);
        // A conformant frame of the same shape must not be counted.
        hub.on_lcm_message(channel, &image_payload(4, 4, 12, "rgb8", &[3u8; 48]));
        let views = hub.topic_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].unclassifiable, 2);
        assert_eq!(views[0].messages, 3);
    }

    /// Every one of these returns `None` rather than a number, because a bogus age
    /// would be read as real latency and would pin the encoder's quality to the floor
    /// for as long as the publisher kept sending.
    #[test]
    fn only_a_plausible_sender_stamp_produces_a_latency() {
        let stamped = |stamp_sec: i32, stamp_nsec: i32| {
            frame_age_ms(&ImageMessage::Raw(msgs::RawImage {
                header: msgs::Header { stamp_sec, stamp_nsec, frame_id: String::new() },
                width: 1,
                height: 1,
                step: 1,
                is_bigendian: 0,
                encoding: "mono8".into(),
                data: vec![0],
            }))
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let seconds = now.as_secs() as i32;
        assert!(stamped(0, 0).is_none(), "an unstamped publisher must not read as zero latency");
        assert!(stamped(-1, 0).is_none());
        assert!(stamped(seconds, -1).is_none());
        assert!(stamped(seconds + 60, 0).is_none(), "a sender clock ahead of ours is skew, not latency");
        assert!(stamped(seconds - 3600, 0).is_none(), "an hour behind is skew, not a link we can recover");
        let age = stamped(seconds - 1, now.subsec_nanos() as i32).expect("a one second old frame is plausible");
        assert!((age - 1000.0).abs() < 100.0, "expected about 1000 ms, got {age}");
    }

    fn image_payload(width: i32, height: i32, step: i32, encoding: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&msgs::IMAGE_FINGERPRINT);
        payload.extend_from_slice(&(data.len() as i32).to_be_bytes());
        payload.extend_from_slice(&[0u8; 12]);
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(&height.to_be_bytes());
        payload.extend_from_slice(&width.to_be_bytes());
        payload.extend_from_slice(&((encoding.len() + 1) as u32).to_be_bytes());
        payload.extend_from_slice(encoding.as_bytes());
        payload.push(0);
        payload.push(0);
        payload.extend_from_slice(&step.to_be_bytes());
        payload.extend_from_slice(data);
        payload
    }

    #[test]
    fn discovery_records_topics_from_both_transports() {
        let hub = Hub::new(Settings::default(), std::env::temp_dir());
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
