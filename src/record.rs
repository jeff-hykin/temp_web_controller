//! Records live wire traffic to an mcap file.
//!
//! Types we can transcode are written as ROS2 CDR so the file opens in
//! Foxglove; anything else is stored as raw LCM bytes rather than dropped, so a
//! recording is never silently incomplete.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use mcap::{WriteOptions, Writer};
use serde::{Deserialize, Serialize};

/// Deep enough to ride out a disk hiccup, shallow enough that we shed frames
/// instead of growing an unbounded backlog.
const QUEUE_DEPTH: usize = 256;

struct Sample {
    topic: String,
    msg_type: Option<String>,
    payload: Vec<u8>,
    log_time: u64,
}

#[derive(Default)]
struct Counters {
    messages: AtomicU64,
    bytes: AtomicU64,
    dropped: AtomicU64,
}

/// Chunk compression for the mcap file. `None` is the safest under a hard kill
/// (the file stays readable up to the last message) but depth frames make a
/// recording enormous, so lz4 is the default trade.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    None,
    #[default]
    Lz4,
    Zstd,
}

impl Compression {
    fn to_mcap(self) -> Option<mcap::Compression> {
        match self {
            Compression::None => None,
            Compression::Lz4 => Some(mcap::Compression::Lz4),
            Compression::Zstd => Some(mcap::Compression::Zstd),
        }
    }
}

#[derive(Serialize)]
pub struct RecordingStatus {
    pub active: bool,
    pub path: Option<String>,
    pub messages: u64,
    pub bytes: u64,
    pub dropped: u64,
    pub seconds: f64,
}

pub struct Recorder {
    path: PathBuf,
    started: SystemTime,
    counters: Arc<Counters>,
    sender: Option<SyncSender<Sample>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Recorder {
    pub fn start(path: &Path, compression: Compression) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let file = File::create(path).with_context(|| format!("could not create {}", path.display()))?;
        let writer = WriteOptions::new()
            .compression(compression.to_mcap())
            .profile("ros2")
            .create(BufWriter::new(file))?;

        let (sender, receiver) = sync_channel(QUEUE_DEPTH);
        let counters = Arc::new(Counters::default());
        let worker_counters = Arc::clone(&counters);
        let worker = std::thread::Builder::new()
            .name("mcap-writer".into())
            .spawn(move || drain(writer, receiver, worker_counters))?;

        Ok(Recorder {
            path: path.to_path_buf(),
            started: SystemTime::now(),
            counters,
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    pub fn offer(&self, topic: &str, msg_type: Option<&str>, payload: &[u8]) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let sample = Sample {
            topic: topic.to_string(),
            msg_type: msg_type.map(str::to_string),
            payload: payload.to_vec(),
            log_time: now_nanos(),
        };
        if let Err(TrySendError::Full(_)) = sender.try_send(sample) {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn is_writing_to(&self, path: &Path) -> bool {
        self.path == path
    }

    pub fn status(&self) -> RecordingStatus {
        RecordingStatus {
            active: true,
            path: Some(self.path.display().to_string()),
            messages: self.counters.messages.load(Ordering::Relaxed),
            bytes: self.counters.bytes.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
            seconds: self.started.elapsed().map(|age| age.as_secs_f64()).unwrap_or(0.0),
        }
    }

    /// Flushes the queue and closes the file. The tally is read after the join,
    /// since the queued tail is still being written until then.
    pub fn finish(mut self) -> Result<RecordingStatus> {
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("mcap writer thread panicked"))??;
        }
        Ok(RecordingStatus {
            active: false,
            ..self.status()
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn drain(
    mut writer: Writer<BufWriter<File>>,
    receiver: Receiver<Sample>,
    counters: Arc<Counters>,
) -> Result<()> {
    let mut channels: HashMap<String, (u16, u32)> = HashMap::new();
    for sample in receiver {
        let encoded = sample
            .msg_type
            .as_deref()
            .and_then(|msg_type| crate::cdr::to_ros2(msg_type, &sample.payload));

        let channel_id = match channels.get(&sample.topic) {
            Some((id, _)) => *id,
            None => {
                let id = match &encoded {
                    Some(encoded) => {
                        let schema_id = writer.add_schema(
                            &encoded.schema_name,
                            "ros2msg",
                            encoded.schema_text.as_bytes(),
                        )?;
                        writer.add_channel(schema_id, &sample.topic, "cdr", &BTreeMap::new())?
                    }
                    // Schema id 0 means "no schema", which is how an
                    // untranscodable type gets stored rather than dropped.
                    None => writer.add_channel(
                        0,
                        &sample.topic,
                        "lcm",
                        &lcm_metadata(sample.msg_type.as_deref()),
                    )?,
                };
                channels.insert(sample.topic.clone(), (id, 0));
                id
            }
        };

        let sequence = {
            let slot = channels.get_mut(&sample.topic).expect("just inserted");
            slot.1 = slot.1.wrapping_add(1);
            slot.1
        };

        let data = encoded.map(|encoded| encoded.data).unwrap_or(sample.payload);
        writer.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id,
                sequence,
                log_time: sample.log_time,
                publish_time: sample.log_time,
            },
            &data,
        )?;
        counters.messages.fetch_add(1, Ordering::Relaxed);
        counters.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
    }
    writer.finish()?;
    Ok(())
}

/// Without a schema the LCM type name is the only clue a reader has about what
/// the bytes are, so keep it alongside the channel.
fn lcm_metadata(msg_type: Option<&str>) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    if let Some(msg_type) = msg_type {
        metadata.insert("lcm_type".into(), msg_type.into());
    }
    metadata
}

pub fn idle_status() -> RecordingStatus {
    RecordingStatus {
        active: false,
        path: None,
        messages: 0,
        bytes: 0,
        dropped: 0,
        seconds: 0.0,
    }
}

#[derive(Serialize)]
pub struct RecordingFile {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub seconds_old: f64,
}

pub fn list(directory: &Path) -> Vec<RecordingFile> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<RecordingFile> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|end| end == "mcap"))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some(RecordingFile {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path().display().to_string(),
                bytes: metadata.len(),
                seconds_old: metadata
                    .modified()
                    .ok()
                    .and_then(|when| when.elapsed().ok())
                    .map(|age| age.as_secs_f64())
                    .unwrap_or(0.0),
            })
        })
        .collect();
    files.sort_by(|left, right| left.seconds_old.total_cmp(&right.seconds_old));
    files
}

/// Resolves a client-supplied recording name against the recording directory.
/// Anything with a path separator or a non-mcap extension is refused outright,
/// so a browser cannot talk the server into touching an unrelated file.
pub fn resolve(directory: &Path, name: &str) -> Result<PathBuf> {
    let mut parts = Path::new(name).components();
    let Some(std::path::Component::Normal(single)) = parts.next() else {
        anyhow::bail!("recording name must be a plain file name");
    };
    if parts.next().is_some() {
        anyhow::bail!("recording name must be a plain file name");
    }
    let path = directory.join(single);
    if path.extension().is_none_or(|end| end != "mcap") {
        anyhow::bail!("recording name must end in .mcap");
    }
    Ok(path)
}

pub fn default_name() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0);
    format!("web_ctrl_{seconds}.mcap")
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|age| age.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgs;

    fn scratch(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("web_ctrl_test_{label}_{}", now_nanos()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn known_types_get_a_ros2_schema_and_unknown_ones_survive_as_lcm() {
        let directory = scratch("mixed");
        let path = directory.join("out.mcap");
        let recorder = Recorder::start(&path, Compression::None).unwrap();
        recorder.offer(
            "/tele_cmd_vel",
            Some(msgs::TWIST_TYPE),
            &msgs::encode_twist([1.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
        );
        recorder.offer("/mystery", Some("nav_msgs.Odometry"), &[7u8; 16]);
        let status = recorder.finish().unwrap();
        assert_eq!(status.messages, 2);
        assert!(!status.active);

        let bytes = std::fs::read(&path).unwrap();
        let mut seen = HashMap::new();
        for message in mcap::MessageStream::new(&bytes).unwrap() {
            let message = message.unwrap();
            seen.insert(
                message.channel.topic.clone(),
                (
                    message.channel.message_encoding.clone(),
                    message.channel.schema.as_ref().map(|s| s.name.clone()),
                    message.data.to_vec(),
                ),
            );
        }

        let (encoding, schema, data) = &seen["/tele_cmd_vel"];
        assert_eq!(encoding, "cdr");
        assert_eq!(schema.as_deref(), Some("geometry_msgs/msg/Twist"));
        assert_eq!(&data[4..12], &1.0f64.to_le_bytes());

        let (encoding, schema, data) = &seen["/mystery"];
        assert_eq!(encoding, "lcm");
        assert_eq!(*schema, None);
        assert_eq!(data, &vec![7u8; 16]);

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn compressed_recordings_still_read_back() {
        for compression in [Compression::Lz4, Compression::Zstd] {
            let directory = scratch("compressed");
            let path = directory.join("out.mcap");
            let recorder = Recorder::start(&path, compression).unwrap();
            for _ in 0..64 {
                recorder.offer(
                    "/tele_cmd_vel",
                    Some(msgs::TWIST_TYPE),
                    &msgs::encode_twist([1.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
                );
            }
            recorder.finish().unwrap();

            let bytes = std::fs::read(&path).unwrap();
            let read: Vec<_> = mcap::MessageStream::new(&bytes)
                .unwrap()
                .map(|message| message.unwrap())
                .collect();
            assert_eq!(read.len(), 64, "{compression:?}");
            assert_eq!(&read[0].data[4..12], &1.0f64.to_le_bytes());
            std::fs::remove_dir_all(&directory).unwrap();
        }
    }

    #[test]
    fn resolve_refuses_to_escape_the_recording_directory() {
        let directory = Path::new("/tmp/recordings");
        assert!(resolve(directory, "../../etc/passwd.mcap").is_err());
        assert!(resolve(directory, "nested/run.mcap").is_err());
        assert!(resolve(directory, "run.txt").is_err());
        assert_eq!(
            resolve(directory, "run.mcap").unwrap(),
            Path::new("/tmp/recordings/run.mcap")
        );
    }

    #[test]
    fn listing_reports_size_and_ignores_other_files() {
        let directory = scratch("listing");
        std::fs::write(directory.join("a.mcap"), [0u8; 12]).unwrap();
        std::fs::write(directory.join("notes.txt"), [0u8; 3]).unwrap();
        let files = list(&directory);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "a.mcap");
        assert_eq!(files[0].bytes, 12);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
