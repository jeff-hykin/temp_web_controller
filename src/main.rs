mod cdr;
mod hub;
mod image;
mod launcher;
mod lcm;
mod msgs;
mod record;
mod relay;
mod service;
mod web;
mod zenoh_io;
mod zenoh_web_link;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hub::{Hub, Settings, Transport};
use relay::LcmRelay;
use std::path::{Path, PathBuf};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
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

    /// Which transports commands reach. The page always publishes on zenoh (through
    /// zenoh-web); anything but `zenoh` also mirrors the command topic onto lcm.
    #[arg(long, global = true, value_enum, default_value_t = TransportChoice::Both)]
    transport: TransportChoice,

    /// The zenoh-web server the page uses. If one already answers here it is used;
    /// otherwise web_ctrl starts its own in-process on this url's port.
    #[arg(long, global = true, default_value = zenoh_web_link::DEFAULT_URL)]
    zenoh_web: String,

    /// zenoh config (json5) for web_ctrl's sessions. Default: zenoh's defaults, a
    /// peer that finds others by multicast scouting.
    #[arg(long, global = true)]
    zenoh_config: Option<PathBuf>,

    /// zenoh endpoint to connect to, e.g. tcp/127.0.0.1:7447 (repeatable).
    #[arg(long, global = true)]
    zenoh_connect: Vec<String>,

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
        let mut arguments: Vec<String> = vec![
            "--port".into(),
            self.port.to_string(),
            "--bind".into(),
            self.bind.to_string(),
            "--topic".into(),
            self.topic.clone(),
            "--transport".into(),
            describe_choice(self.transport).into(),
            "--zenoh-web".into(),
            self.zenoh_web.clone(),
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
        ];
        if let Some(config) = &self.zenoh_config {
            arguments.push("--zenoh-config".into());
            arguments.push(absolute(config).to_string_lossy().into_owned());
        }
        for endpoint in &self.zenoh_connect {
            arguments.push("--zenoh-connect".into());
            arguments.push(endpoint.clone());
        }
        arguments
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
            record_dir,
            ..Settings::default()
        },
    );

    let zenoh_config = zenoh_io::config(args.zenoh_config.as_deref(), &args.zenoh_connect)?;
    let session = zenoh_io::open(zenoh_config.clone())
        .await
        .context("opening the zenoh session")?;
    let relay = LcmRelay::new(session.clone(), Arc::clone(&hub));

    let lcm_url = lcm::parse_url(&args.lcm_url)?;
    let lcm_mirroring = args.transport != TransportChoice::Zenoh;
    let lcm_transport = match lcm_mirroring {
        true => Some(Arc::new(lcm::LcmTransport::new(lcm_url).context("opening lcm socket")?)),
        false => None,
    };
    let lcm_receiver_error = Arc::new(Mutex::new(Some("starting up".to_owned())));
    spawn_lcm_receiver(Arc::clone(&hub), Arc::clone(&relay), lcm_url, Arc::clone(&lcm_receiver_error));

    let mirror = CommandMirror { hub: Arc::clone(&hub), lcm: lcm_transport };
    let sample_hub = Arc::clone(&hub);
    let subscriber = zenoh_io::subscribe_all(&session, move |key_expr, payload| {
        sample_hub.on_zenoh_message(key_expr, payload);
        mirror.offer(key_expr, payload);
    })
    .await
    .context("zenoh discovery subscription")?;

    let zenoh_web = zenoh_web_link::use_or_start(
        &args.zenoh_web,
        args.bind,
        zenoh_config,
        zenoh_io::local_tcp_port(&session).await,
    )
    .await?;

    let state = web::AppState {
        hub: Arc::clone(&hub),
        launcher: Arc::new(launcher::Launcher::new(launch_file)),
        lcm_enabled: lcm_mirroring,
        zenoh_enabled: true,
        lcm_receiver_error,
        zenoh_web_mode: zenoh_web.mode(),
        zenoh_web_address: zenoh_web.address().to_owned(),
    };

    spawn_ticker(Arc::clone(&hub), Arc::clone(&relay));

    let address = SocketAddr::new(args.bind, args.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding {address}"))?;
    println!("web_ctrl on http://{}:{}", local_address(), args.port);
    println!("  commands  -> {} ({})", args.topic, describe(args.transport));
    println!("  zenoh-web -> {} ({})", zenoh_web.address(), zenoh_web.mode());
    let app = web::router(state, zenoh_web.router());
    tokio::select! {
        served = axum::serve(listener, app) => served?,
        () = terminated() => {}
    }
    // Deadmen go out before the session they travel on closes.
    zenoh_web.shutdown().await;
    drop(subscriber);
    Ok(())
}

/// Resolves on SIGINT or SIGTERM, the second being how systemd stops the service.
async fn terminated() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Puts what the page publishes on the command topic onto lcm as well, for robots
/// whose stack listens there. The page itself only reaches zenoh.
struct CommandMirror {
    hub: Arc<Hub>,
    lcm: Option<Arc<lcm::LcmTransport>>,
}

impl CommandMirror {
    fn offer(&self, key_expr: &str, payload: &[u8]) {
        let Some(lcm) = &self.lcm else {
            return;
        };
        if !key_expr.ends_with(msgs::TWIST_TYPE) {
            return;
        }
        let topic = self.hub.settings().publish_topic;
        if key_expr != hub::lcm_relay_key(&topic, Some(msgs::TWIST_TYPE)) {
            return;
        }
        let channel = format!("{topic}#{}", msgs::TWIST_TYPE);
        if let Err(error) = lcm.publish(&channel, payload) {
            eprintln!("lcm publish failed: {error}");
        }
    }
}

fn describe(transport: TransportChoice) -> &'static str {
    match transport {
        TransportChoice::Zenoh => "zenoh",
        _ => "zenoh + lcm mirror",
    }
}

/// Joining the multicast group fails with ENODEV until an interface carrying a
/// multicast route exists, which at boot happens after `network-online.target`
/// is already satisfied. Retrying is the only reliable fix; giving up left LCM
/// dead for the whole life of the process.
fn retry_delay(consecutive_failures: u32) -> Duration {
    let seconds = 1u64 << consecutive_failures.min(5);
    Duration::from_secs(seconds.min(30))
}

fn spawn_lcm_receiver(
    hub: Arc<Hub>,
    relay: Arc<LcmRelay>,
    url: lcm::LcmUrl,
    status: Arc<Mutex<Option<String>>>,
) {
    std::thread::Builder::new()
        .name("lcm receive".to_owned())
        .spawn(move || {
            let mut consecutive_failures = 0;
            loop {
                let started = std::time::Instant::now();
                let listening_status = Arc::clone(&status);
                let result = lcm::run_receiver(
                    url,
                    |incoming| match incoming {
                        lcm::Incoming::Message { channel, payload } => {
                            hub.on_lcm_message(channel, payload);
                            relay.forward(channel, payload);
                        }
                        lcm::Incoming::Skipped { channel, bytes } => {
                            hub.record_skipped(Transport::Lcm, channel, bytes);
                            relay.note_skipped(channel);
                        }
                    },
                    // Both are asked, so the relay hears about every channel.
                    |channel| relay.wants_payload(channel) | hub.wants_payload(channel),
                    || *listening_status.lock().unwrap() = None,
                );
                if started.elapsed() > Duration::from_secs(60) {
                    consecutive_failures = 0;
                }
                let delay = retry_delay(consecutive_failures);
                if let Err(error) = result {
                    let reason = format!("{error:#}");
                    eprintln!("lcm receiver down, retrying in {}s: {reason}", delay.as_secs());
                    *status.lock().unwrap() = Some(reason);
                }
                consecutive_failures += 1;
                std::thread::sleep(delay);
            }
        })
        .expect("failed to spawn lcm thread");
}

fn spawn_ticker(hub: Arc<Hub>, relay: Arc<LcmRelay>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut last = std::time::Instant::now();
        loop {
            ticker.tick().await;
            hub.tick_rates(last.elapsed());
            last = std::time::Instant::now();
            relay.sweep();
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_retry_is_immediate_enough_to_cover_the_boot_race() {
        assert_eq!(retry_delay(0), Duration::from_secs(1));
        assert_eq!(retry_delay(1), Duration::from_secs(2));
        assert_eq!(retry_delay(2), Duration::from_secs(4));
    }

    #[test]
    fn retries_back_off_to_a_capped_delay() {
        assert_eq!(retry_delay(5), Duration::from_secs(30));
        assert_eq!(retry_delay(50), Duration::from_secs(30));
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(30));
    }
}
