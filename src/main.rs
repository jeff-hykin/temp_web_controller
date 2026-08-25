mod cdr;
mod hub;
mod image;
mod launcher;
mod lcm;
mod msgs;
mod record;
mod service;
mod web;
mod zenoh_io;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hub::{Hub, Settings, Transport};
use std::path::{Path, PathBuf};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TransportChoice {
    Both,
    Lcm,
    Zenoh,
}

#[derive(Parser)]
#[command(name = "web_ctrl", about = "Web teleop controller for dimos robots")]
struct Args {
    #[arg(long, global = true, default_value_t = 8099)]
    port: u16,

    #[arg(long, global = true, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// Topic to publish velocity commands on.
    #[arg(long, global = true, default_value = "/tele_cmd_vel")]
    topic: String,

    /// Which transports to publish commands on. Both listen either way.
    #[arg(long, global = true, value_enum, default_value_t = TransportChoice::Both)]
    transport: TransportChoice,

    #[arg(long, global = true, default_value = lcm::DEFAULT_URL)]
    lcm_url: String,

    #[arg(long, global = true, default_value_t = 0.25)]
    linear_speed: f64,

    #[arg(long, global = true, default_value_t = 0.5)]
    angular_speed: f64,

    /// Where mcap recordings are written and listed from.
    #[arg(long, global = true, default_value = "recordings")]
    record_dir: PathBuf,

    /// Where the launcher's saved commands are kept. Defaults to
    /// `~/.dimos/temp_web_control.json` so every browser on the robot shares one
    /// list rather than each keeping its own.
    #[arg(long, global = true)]
    launch_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install web_ctrl as a boot service (systemd or launchd) with these same
    /// options, and start it now. Asks for sudo.
    #[command(name = "survive_reboot", alias = "survive-reboot")]
    SurviveReboot,
}

impl Args {
    /// The flags the installed service should be launched with. Every path is
    /// made absolute, since a service does not inherit this shell's directory.
    fn service_arguments(&self, record_dir: &Path, launch_file: &Path) -> Vec<String> {
        vec![
            "--port".into(),
            self.port.to_string(),
            "--bind".into(),
            self.bind.to_string(),
            "--topic".into(),
            self.topic.clone(),
            "--transport".into(),
            describe_choice(self.transport).into(),
            "--lcm-url".into(),
            self.lcm_url.clone(),
            "--linear-speed".into(),
            self.linear_speed.to_string(),
            "--angular-speed".into(),
            self.angular_speed.to_string(),
            "--record-dir".into(),
            record_dir.to_string_lossy().into_owned(),
            "--launch-file".into(),
            launch_file.to_string_lossy().into_owned(),
        ]
    }
}

fn describe_choice(transport: TransportChoice) -> &'static str {
    match transport {
        TransportChoice::Both => "both",
        TransportChoice::Lcm => "lcm",
        TransportChoice::Zenoh => "zenoh",
    }
}

fn default_launch_file() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".dimos/temp_web_control.json"),
        None => PathBuf::from("temp_web_control.json"),
    }
}

/// `canonicalize` only works on paths that already exist, and the recordings
/// directory is created lazily, so fall back to joining the current directory.
fn absolute(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| match std::env::current_dir() {
        Ok(directory) => directory.join(path),
        Err(_) => path.to_path_buf(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let record_dir = absolute(&args.record_dir);
    let launch_file = absolute(
        &args
            .launch_file
            .clone()
            .unwrap_or_else(default_launch_file),
    );

    if let Some(Command::SurviveReboot) = args.command {
        let arguments = args.service_arguments(&record_dir, &launch_file);
        let working_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        return service::install(&arguments, &working_directory);
    }

    let hub = Hub::new(
        Settings {
            publish_topic: hub::command_topic(&args.topic)
                .with_context(|| format!("{} is not a usable topic name", args.topic))?,
            linear_speed: args.linear_speed,
            angular_speed: args.angular_speed,
            ..Settings::default()
        },
        record_dir,
    );

    let lcm_url = lcm::parse_url(&args.lcm_url)?;
    let lcm_transport = Arc::new(lcm::LcmTransport::new(lcm_url).context("opening lcm socket")?);
    spawn_lcm_receiver(Arc::clone(&hub), lcm_url);

    let zenoh_session = match zenoh_io::open().await {
        Ok(session) => Some(session),
        Err(error) => {
            eprintln!("zenoh unavailable, continuing on lcm only: {error}");
            None
        }
    };
    if let Some(session) = &zenoh_session {
        match zenoh_io::subscribe_all(session, Arc::clone(&hub)).await {
            Ok(subscriber) => std::mem::forget(subscriber),
            Err(error) => eprintln!("zenoh discovery subscription failed: {error}"),
        }
    }
    let zenoh_publishing = match args.transport {
        TransportChoice::Lcm => None,
        _ => zenoh_session.clone(),
    };

    let lcm_publishing = args.transport != TransportChoice::Zenoh;
    let state = web::AppState {
        hub: Arc::clone(&hub),
        launcher: Arc::new(launcher::Launcher::new(launch_file)),
        lcm_enabled: lcm_publishing,
        zenoh_enabled: zenoh_publishing.is_some(),
    };

    spawn_rate_ticker(Arc::clone(&hub));
    tokio::spawn(publish_commands(
        Arc::clone(&hub),
        lcm_publishing.then(|| Arc::clone(&lcm_transport)),
        zenoh_publishing,
    ));

    let address = SocketAddr::new(args.bind, args.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding {address}"))?;
    println!("web_ctrl on http://{}:{}", local_address(), args.port);
    println!("  commands  -> {} ({})", args.topic, describe(args.transport));
    axum::serve(listener, web::router(state)).await?;
    Ok(())
}

fn describe(transport: TransportChoice) -> &'static str {
    match transport {
        TransportChoice::Both => "lcm + zenoh",
        TransportChoice::Lcm => "lcm",
        TransportChoice::Zenoh => "zenoh",
    }
}

fn spawn_lcm_receiver(hub: Arc<Hub>, url: lcm::LcmUrl) {
    std::thread::Builder::new()
        .name("lcm receive".to_owned())
        .spawn(move || {
            let sink_hub = Arc::clone(&hub);
            let result = lcm::run_receiver(
                url,
                |incoming| match incoming {
                    lcm::Incoming::Message { channel, payload } => {
                        sink_hub.on_lcm_message(channel, payload)
                    }
                    lcm::Incoming::Skipped { channel, bytes } => {
                        sink_hub.record_skipped(Transport::Lcm, channel, bytes)
                    }
                },
                |channel| hub.wants_payload(channel),
            );
            if let Err(error) = result {
                eprintln!("lcm receiver stopped: {error}");
            }
        })
        .expect("failed to spawn lcm thread");
}

fn spawn_rate_ticker(hub: Arc<Hub>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut last = std::time::Instant::now();
        loop {
            ticker.tick().await;
            hub.tick_rates(last.elapsed());
            last = std::time::Instant::now();
        }
    });
}

/// Publishes at a fixed rate while a browser is connected, and keeps sending
/// zeros for a moment after it leaves so the robot cannot inherit a stale command.
async fn publish_commands(
    hub: Arc<Hub>,
    lcm_transport: Option<Arc<lcm::LcmTransport>>,
    zenoh_session: Option<zenoh::Session>,
) {
    let mut stop_flush = 0;
    loop {
        let settings = hub.settings();
        tokio::time::sleep(Duration::from_secs_f64(1.0 / settings.publish_hz)).await;

        let (linear_x, linear_y, angular_z) = hub.current_command();
        // Publishing zeros forever would fight whatever else drives this topic, so
        // the stream goes quiet when nobody is steering. The stop still has to be
        // heard though, so a second of zeros follows the last real input.
        if linear_x != 0.0 || linear_y != 0.0 || angular_z != 0.0 {
            stop_flush = settings.publish_hz as i32;
        } else if stop_flush > 0 {
            stop_flush -= 1;
        } else {
            continue;
        }

        let payload = msgs::encode_twist([linear_x, linear_y, 0.0], [0.0, 0.0, angular_z]);
        let topic = &settings.publish_topic;
        if let Some(transport) = &lcm_transport {
            let channel = format!("{topic}#{}", msgs::TWIST_TYPE);
            if let Err(error) = transport.publish(&channel, &payload) {
                eprintln!("lcm publish failed: {error}");
            }
        }
        if let Some(session) = &zenoh_session {
            let key_expr = zenoh_io::key_expr_for(topic, msgs::TWIST_TYPE);
            if let Err(error) = zenoh_io::put(session, key_expr, payload).await {
                eprintln!("zenoh publish failed: {error}");
            }
        }
    }
}

fn local_address() -> IpAddr {
    let probe = UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("8.8.8.8:80")?;
            socket.local_addr()
        })
        .map(|address| address.ip());
    probe.unwrap_or(IpAddr::from([127, 0, 0, 1]))
}
