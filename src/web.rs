use crate::hub::{Hub, SettingsPatch};
use crate::launcher::Launcher;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    pub launcher: Arc<Launcher>,
    pub lcm_enabled: bool,
    pub zenoh_enabled: bool,
    /// Set while the lcm receiver is down. Without this the thread could die at
    /// boot and the UI would look merely idle rather than deaf.
    pub lcm_receiver_error: Arc<Mutex<Option<String>>>,
    /// Which zenoh-web the page is talking to: "in-process" or "external", and where.
    pub zenoh_web_mode: &'static str,
    pub zenoh_web_address: String,
}

/// `zenoh_web` carries the page's signaling routes, mounted at `/zenoh-web`.
pub fn router(state: AppState, zenoh_web: Router) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(script))
        .route("/style.css", get(stylesheet))
        .route("/vendor/zenoh_web.js", get(zenoh_web_client))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/status", get(status))
        .route("/api/tf", get(tf))
        .route("/api/recordings", get(recordings))
        .route("/api/recordings/{name}", delete(remove_recording))
        .route("/ws", get(control_socket))
        .with_state(state)
        .nest("/zenoh-web", zenoh_web)
}

async fn index() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../web/index.html"),
    )
        .into_response()
}

async fn script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
        .into_response()
}

/// zenoh-web's browser client, bundled from the pinned commit by
/// `run/vendor_zenoh_web` and compiled into the binary, so a robot without
/// internet still serves a working page.
async fn zenoh_web_client() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/vendor/zenoh_web.js"),
    )
        .into_response()
}

async fn stylesheet() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/style.css"),
    )
        .into_response()
}

async fn status(State(state): State<AppState>) -> Response {
    axum::Json(status_payload(&state)).into_response()
}

async fn tf(State(state): State<AppState>) -> Response {
    axum::Json(state.hub.tf_view()).into_response()
}

async fn recordings(State(state): State<AppState>) -> Response {
    axum::Json(state.hub.list_recordings()).into_response()
}

async fn remove_recording(Path(name): Path<String>, State(state): State<AppState>) -> Response {
    match state.hub.delete_recording(&name) {
        Ok(()) => axum::Json(json!({ "ok": true })).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

fn status_payload(state: &AppState) -> serde_json::Value {
    json!({
        "type": "status",
        "topics": state.hub.topic_views(),
        "settings": state.hub.settings(),
        "recording": state.hub.recording_status(),
        "launcher": state.launcher.view(),
        "publish": {
            "topic": state.hub.settings().publish_topic,
            "lcm": state.lcm_enabled,
            "zenoh": state.zenoh_enabled,
        },
        "zenoh_web": {
            "mode": state.zenoh_web_mode,
            "address": state.zenoh_web_address,
        },
        "receivers": {
            "lcm_error": state.lcm_receiver_error.lock().unwrap().clone(),
        },
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Settings(SettingsPatch),
    Record {
        #[serde(default)]
        path: Option<String>,
    },
    StopRecord,
    RecordTopic {
        topic: String,
        recorded: bool,
    },
    LaunchRun {
        name: String,
    },
    LaunchStop,
    LaunchKill,
    LaunchSave {
        name: String,
        command: String,
    },
    LaunchDelete {
        name: String,
    },
}

async fn control_socket(upgrade: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    upgrade.on_upgrade(move |socket| run_control_socket(socket, state))
}

/// Reading and writing run as separate tasks. Sharing one loop meant a status
/// payload that could not drain into a congested phone also held up everything the
/// browser was trying to say — a Stop Recording press, say — until the send finally
/// went through. Steering does not come through here: the page publishes it on
/// zenoh itself, through zenoh-web.
async fn run_control_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let writer_state = state.clone();
    let writer = tokio::spawn(async move {
        let mut status_timer = tokio::time::interval(Duration::from_millis(500));
        // A client that fell behind should get the next status, not a burst of the
        // ones it missed, which would only push it further behind.
        status_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            status_timer.tick().await;
            let payload = status_payload(&writer_state).to_string();
            if sink.send(Message::Text(payload.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Settings(patch)) => {
                state.hub.apply_settings(patch);
            }
            Ok(ClientMessage::Record { path }) => {
                if let Err(error) = state.hub.start_recording(path.as_deref()) {
                    eprintln!("could not start recording: {error}");
                }
            }
            Ok(ClientMessage::StopRecord) => {
                // Finalising an mcap drains the writer queue and writes the index,
                // which on a multi-gigabyte file takes long enough that doing it
                // here would stall every command behind it, steering included.
                let hub = Arc::clone(&state.hub);
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = hub.stop_recording() {
                        eprintln!("could not stop recording: {error}");
                    }
                });
            }
            Ok(ClientMessage::RecordTopic { topic, recorded }) => {
                state.hub.set_topic_recorded(&topic, recorded);
            }
            Ok(ClientMessage::LaunchRun { name }) => {
                if let Err(error) = state.launcher.run(&name) {
                    state.launcher.note(error.to_string());
                }
            }
            Ok(ClientMessage::LaunchStop) => {
                if let Err(error) = state.launcher.stop() {
                    state.launcher.note(error.to_string());
                }
            }
            Ok(ClientMessage::LaunchKill) => state.launcher.kill_blueprint(),
            Ok(ClientMessage::LaunchSave { name, command }) => {
                if let Err(error) = state.launcher.save_command(&name, &command) {
                    state.launcher.note(error.to_string());
                }
            }
            Ok(ClientMessage::LaunchDelete { name }) => {
                if let Err(error) = state.launcher.delete_command(&name) {
                    state.launcher.note(error.to_string());
                }
            }
            Err(error) => eprintln!("ignoring malformed control message: {error}"),
        }
    }

    writer.abort();
}

