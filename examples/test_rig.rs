//! The robot side of `test/e2e.js`: publishes camera frames on lcm and zenoh and
//! prints every command that reaches the watched topics, on either transport.
//!
//! ```text
//! test_rig --zenoh-listen tcp/127.0.0.1:17447 --lcm-url 'udpm://239.255.76.67:17667?ttl=0' \
//!     --lcm-image '/cam#sensor_msgs.Image=test/fixtures/image_rgb8.bin@10' \
//!     --zenoh-image 'dimos/cam2/sensor_msgs.CompressedImage=test/fixtures/compressed_png.bin@10' \
//!     --watch-zenoh 'dimos/tele_cmd_vel_test/geometry_msgs.Twist' \
//!     --watch-lcm '/tele_cmd_vel_test#geometry_msgs.Twist'
//! ```
//!
//! Prints `READY`, then one `ZENOH <key> lx ly lz ax ay az` or `LCM <channel> ...`
//! line per command received. Its zenoh session never scouts, so it only meets the
//! processes the test points at it.

#[allow(dead_code)]
#[path = "../src/lcm.rs"]
mod lcm;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use zenoh::Wait;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    zenoh_listen: String,
    #[arg(long)]
    lcm_url: String,
    /// `<channel>=<file>@<hz>`: a dimos-encoded payload published on lcm.
    #[arg(long)]
    lcm_image: Vec<String>,
    /// `<key>=<file>@<hz>`: a dimos-encoded payload published on zenoh.
    #[arg(long)]
    zenoh_image: Vec<String>,
    #[arg(long)]
    watch_zenoh: Vec<String>,
    #[arg(long)]
    watch_lcm: Vec<String>,
}

struct Feed {
    name: String,
    payload: Vec<u8>,
    period: Duration,
}

fn feed(spec: &str) -> Result<Feed> {
    let (name, rest) = spec.rsplit_once('=').with_context(|| format!("{spec}: expected <name>=<file>@<hz>"))?;
    let (file, hz) = rest.rsplit_once('@').with_context(|| format!("{spec}: expected <file>@<hz>"))?;
    let hz: f64 = hz.parse()?;
    Ok(Feed {
        name: name.to_owned(),
        payload: std::fs::read(file).with_context(|| format!("reading {file}"))?,
        period: Duration::from_secs_f64(1.0 / hz),
    })
}

fn describe_twist(payload: &[u8]) -> String {
    if payload.len() != 56 {
        return format!("bad-length-{}", payload.len());
    }
    payload[8..]
        .chunks_exact(8)
        .map(|value| f64::from_be_bytes(value.try_into().unwrap()).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn say(line: String) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut config = zenoh::Config::default();
    config.insert_json5("scouting/multicast/enabled", "false").map_err(|error| anyhow!("{error}"))?;
    config
        .insert_json5("listen/endpoints", &serde_json::to_string(&[&args.zenoh_listen])?)
        .map_err(|error| anyhow!("{error}"))?;
    let session = zenoh::open(config).wait().map_err(|error| anyhow!("{error}"))?;

    let mut subscribers = Vec::new();
    for key in &args.watch_zenoh {
        let subscriber = session
            .declare_subscriber(key.as_str())
            .callback(|sample| {
                say(format!("ZENOH {} {}", sample.key_expr(), describe_twist(&sample.payload().to_bytes())))
            })
            .wait()
            .map_err(|error| anyhow!("{error}"))?;
        subscribers.push(subscriber);
    }

    let lcm_url = lcm::parse_url(&args.lcm_url)?;
    if !args.watch_lcm.is_empty() {
        let watched = args.watch_lcm.clone();
        std::thread::spawn(move || {
            let result = lcm::run_receiver(
                lcm_url,
                |incoming| {
                    if let lcm::Incoming::Message { channel, payload } = incoming {
                        if watched.iter().any(|name| name == channel) {
                            say(format!("LCM {channel} {}", describe_twist(payload)));
                        }
                    }
                },
                |channel| watched.iter().any(|name| name == channel),
                || {},
            );
            if let Err(error) = result {
                eprintln!("lcm receiver: {error:#}");
            }
        });
    }

    let lcm_transport = Arc::new(lcm::LcmTransport::new(lcm_url)?);
    for spec in &args.lcm_image {
        let feed = feed(spec)?;
        let transport = Arc::clone(&lcm_transport);
        std::thread::spawn(move || loop {
            if let Err(error) = transport.publish(&feed.name, &feed.payload) {
                eprintln!("lcm publish {}: {error:#}", feed.name);
            }
            std::thread::sleep(feed.period);
        });
    }
    for spec in &args.zenoh_image {
        let feed = feed(spec)?;
        let publisher = session
            .declare_publisher(feed.name.clone())
            .wait()
            .map_err(|error| anyhow!("{error}"))?;
        std::thread::spawn(move || loop {
            if let Err(error) = publisher.put(feed.payload.clone()).wait() {
                eprintln!("zenoh publish {}: {error}", feed.name);
            }
            std::thread::sleep(feed.period);
        });
    }

    say("READY".to_owned());
    loop {
        std::thread::park();
    }
}
