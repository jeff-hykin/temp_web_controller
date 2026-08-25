mod cdr;
mod hub;
mod image;
mod launcher;
mod lcm;
mod msgs;
mod record;
mod web;
mod zenoh_io;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use hub::{Hub, Settings, Transport};
use std::path::PathBuf;
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
    #[arg(long, default_value_t = 8099)]
    port: u16,

    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// Topic to publish velocity commands on.
    #[arg(long, default_value = "/tele_cmd_vel")]
    topic: String,

    /// Which transports to publish commands on. Both listen either way.
    #[arg(long, value_enum, default_value_t = TransportChoice::Both)]
    transport: TransportChoice,

    #[arg(long, default_value = lcm::DEFAULT_URL)]
    lcm_url: String,

    #[arg(long, default_value_t = 0.25)]
    linear_speed: f64,

    #[arg(long, default_value_t = 0.5)]
    angular_speed: f64,

    /// Where mcap recordings are written and listed from.
    #[arg(long, default_value = "recordings")]
    record_dir: PathBuf,

    /// Where the launcher's saved commands are kept.
    #[arg(long, default_value = "launch_commands.json")]
    launch_file: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let record_dir = std::fs::canonicalize(&args.record_dir).unwrap_or(args.record_dir.clone());
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
        launcher: Arc::new(launcher::Launcher::new(args.launch_file.clone())),
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
    let mut idle_flush = 0;
    loop {
        let settings = hub.settings();
        tokio::time::sleep(Duration::from_secs_f64(1.0 / settings.publish_hz)).await;

        if hub.has_control_clients() {
            idle_flush = settings.publish_hz as i32;
        } else if idle_flush > 0 {
            idle_flush -= 1;
        } else {
            continue;
        }

        let (linear_x, linear_y, angular_z) = hub.current_command();
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
