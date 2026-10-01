//! Finding or starting the zenoh-web server the page talks to.
//!
//! Use-or-start: `GET <url>/zenoh-web/health` decides. If a zenoh-web server already
//! answers there it is used as-is, so one robot runs one bridge no matter how many
//! tools want it. Otherwise web_ctrl starts zenoh-web inside this process, binds it
//! on that same port (so the next tool finds it), and mounts its routes under
//! `/zenoh-web` on web_ctrl's own port as well.
//!
//! Either way the page signals through web_ctrl's own origin: `POST /zenoh-web/offer`
//! is zenoh-web's own route when it runs in-process and a proxy to the external one
//! otherwise. The page then never needs to know the bridge's address, which matters
//! because the usual `--zenoh-web` is a loopback url a phone cannot reach.

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use std::net::IpAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const DEFAULT_URL: &str = "http://127.0.0.1:7448";
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// zenoh-web gathers ICE candidates before answering, which can take a while.
const OFFER_TIMEOUT: Duration = Duration::from_secs(20);

pub enum ZenohWebLink {
    InProcess {
        server: zenoh_web::Server,
        /// `None` when the port was taken by something that is not zenoh-web; the
        /// page still works through the routes mounted on web_ctrl's port.
        running: Option<zenoh_web::RunningServer>,
        address: String,
    },
    External {
        address: String,
    },
}

impl ZenohWebLink {
    pub fn mode(&self) -> &'static str {
        match self {
            ZenohWebLink::InProcess { .. } => "in-process",
            ZenohWebLink::External { .. } => "external",
        }
    }

    pub fn address(&self) -> &str {
        match self {
            ZenohWebLink::InProcess { address, .. } | ZenohWebLink::External { address } => address,
        }
    }

    /// The routes the page signals through, mounted at `/zenoh-web`.
    pub fn router(&self) -> Router {
        match self {
            ZenohWebLink::InProcess { server, .. } => server.router(),
            ZenohWebLink::External { address } => Router::new()
                .route("/offer", post(proxy_offer))
                .with_state(address.clone()),
        }
    }

    /// Fires every browser's deadmen and closes them, when zenoh-web is ours to stop.
    pub async fn shutdown(self) {
        if let ZenohWebLink::InProcess { server, running, .. } = self {
            let result = match running {
                Some(running) => running.shutdown().await,
                None => server.shutdown().await,
            };
            if let Err(error) = result {
                eprintln!("zenoh-web shutdown: {error:#}");
            }
        }
    }
}

/// `http://host:port` → `host:port`, with zenoh-web's default port when none is given.
pub fn address_of(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("http://")
        .with_context(|| format!("--zenoh-web must be an http:// url, got {url}"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        bail!("--zenoh-web has no host: {url}");
    }
    let has_port = authority.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>().is_ok());
    Ok(match has_port {
        true => authority.to_owned(),
        false => format!("{authority}:{}", zenoh_web::DEFAULT_PORT),
    })
}

fn port_of(address: &str) -> u16 {
    address
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(zenoh_web::DEFAULT_PORT)
}

/// Whether a zenoh-web server answers its health check at `address`.
pub async fn probe(address: &str) -> bool {
    match http_request(address, "GET", zenoh_web::HEALTH_PATH, &[], PROBE_TIMEOUT).await {
        Ok((200, body)) => serde_json::from_slice::<serde_json::Value>(&body)
            .is_ok_and(|health| health["service"] == "zenoh-web"),
        _ => false,
    }
}

/// `own_session_config` is web_ctrl's zenoh config; the in-process server gets a
/// session of its own built from it (see the relay module for why it cannot share
/// web_ctrl's), connected straight to web_ctrl's session on `web_ctrl_zenoh_port`.
pub async fn use_or_start(
    url: &str,
    bind: IpAddr,
    own_session_config: zenoh::Config,
    web_ctrl_zenoh_port: Option<u16>,
) -> Result<ZenohWebLink> {
    let address = address_of(url)?;
    if probe(&address).await {
        return Ok(ZenohWebLink::External { address });
    }

    let mut config = own_session_config;
    // Its own listener would collide with a fixed port in a shared config file, and
    // nothing needs to dial into it: it dials web_ctrl and anything scouting finds.
    config.insert_json5("listen/endpoints", "[]").map_err(|error| anyhow!("{error}"))?;
    if let Some(port) = web_ctrl_zenoh_port {
        let mut endpoints: Vec<String> = config
            .get_json("connect/endpoints")
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        endpoints.push(format!("tcp/127.0.0.1:{port}"));
        config
            .insert_json5("connect/endpoints", &serde_json::to_string(&endpoints)?)
            .map_err(|error| anyhow!("{error}"))?;
    }
    let mut builder = zenoh_web::Server::builder().zenoh_config(config);
    for codec in zenoh_dimos_codecs::all() {
        builder = builder.shared_codec(codec);
    }
    let server = builder
        .build()
        .await
        .context("starting zenoh-web in-process")?;
    let port = port_of(&address);
    let running = match server.clone().bind((bind, port)).await {
        Ok(running) => Some(running),
        Err(error) => {
            eprintln!(
                "zenoh-web: could not also listen on {bind}:{port} ({error:#}); \
                 it is only reachable through web_ctrl's /zenoh-web"
            );
            None
        }
    };
    Ok(ZenohWebLink::InProcess { server, running, address })
}

async fn proxy_offer(State(address): State<String>, body: Bytes) -> Response {
    match http_request(&address, "POST", "/offer", &body, OFFER_TIMEOUT).await {
        Ok((status, answer)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            [(header::CONTENT_TYPE, "application/json")],
            answer,
        )
            .into_response(),
        Err(error) => {
            (StatusCode::BAD_GATEWAY, format!("zenoh-web at {address}: {error:#}")).into_response()
        }
    }
}

/// A bare HTTP/1.0 exchange: the server closes the connection after answering and
/// never chunks, so no HTTP client crate is needed for one probe and one proxy.
async fn http_request(
    address: &str,
    method: &str,
    path: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, Vec<u8>)> {
    let exchange = async {
        let mut stream = TcpStream::connect(address).await?;
        let head = format!(
            "{method} {path} HTTP/1.0\r\nhost: {address}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        anyhow::Ok(response)
    };
    let response = tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| anyhow!("no answer within {}s", timeout.as_secs()))??;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("malformed HTTP response")?;
    let status_line = std::str::from_utf8(&response[..split])?.lines().next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .with_context(|| format!("malformed status line {status_line:?}"))?;
    Ok((status, response[split + 4..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_become_addresses_with_the_default_port_filled_in() {
        assert_eq!(address_of("http://127.0.0.1:7448").unwrap(), "127.0.0.1:7448");
        assert_eq!(address_of("http://robot.local/").unwrap(), "robot.local:7448");
        assert_eq!(address_of("http://10.0.0.2:9000/zenoh-web").unwrap(), "10.0.0.2:9000");
        assert!(address_of("https://robot.local").is_err());
        assert!(address_of("http://").is_err());
    }

    #[tokio::test]
    async fn nothing_listening_is_not_a_zenoh_web_server() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(!probe(&format!("127.0.0.1:{port}")).await);
    }

    #[tokio::test]
    async fn some_other_http_server_is_not_a_zenoh_web_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let app = Router::new().fallback(|| async { "hello" });
        tokio::spawn(async move { axum::serve(listener, app).await });
        assert!(!probe(&address).await);
    }
}
