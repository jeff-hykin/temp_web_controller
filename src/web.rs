use crate::hub::{Command, Hub, SettingsPatch};
use crate::launcher::Launcher;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::Router;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    pub launcher: Arc<Launcher>,
    pub lcm_enabled: bool,
    pub zenoh_enabled: bool,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(script))
        .route("/style.css", get(stylesheet))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/status", get(status))
        .route("/api/tf", get(tf))
        .route("/api/recordings", get(recordings))
        .route("/api/recordings/{name}", delete(remove_recording))
        .route("/ws", get(control_socket))
        .route("/ws/stream/{*topic}", get(stream_socket))
        .with_state(state)
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
        "streams": state.hub.stream_stats(),
        "recording": state.hub.recording_status(),
        "launcher": state.launcher.view(),
        "publish": {
            "topic": state.hub.settings().publish_topic,
            "lcm": state.lcm_enabled,
            "zenoh": state.zenoh_enabled,
        },
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Cmd {
        #[serde(default)]
        forward: f64,
        #[serde(default)]
        strafe: f64,
        #[serde(default)]
        turn: f64,
    },
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

async fn run_control_socket(mut socket: WebSocket, state: AppState) {
    let mut status_timer = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else {
                    break;
                };
                let Message::Text(text) = message else {
                    continue;
                };
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Cmd { forward, strafe, turn }) => {
                        state.hub.set_command(Command { forward, strafe, turn });
                    }
                    Ok(ClientMessage::Settings(patch)) => {
                        state.hub.apply_settings(patch);
                    }
                    Ok(ClientMessage::Record { path }) => {
                        if let Err(error) = state.hub.start_recording(path.as_deref()) {
                            eprintln!("could not start recording: {error}");
                        }
                    }
                    Ok(ClientMessage::StopRecord) => {
                        if let Err(error) = state.hub.stop_recording() {
                            eprintln!("could not stop recording: {error}");
                        }
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
            _ = status_timer.tick() => {
                let payload = status_payload(&state).to_string();
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
        }
    }

    state.hub.on_control_disconnect();
}

async fn stream_socket(
    upgrade: WebSocketUpgrade,
    Path(topic): Path<String>,
    State(state): State<AppState>,
) -> Response {
    upgrade.on_upgrade(move |socket| run_stream_socket(socket, topic, state))
}

/// Sends the newest frame available at the moment the socket is free. A client
/// that cannot keep up simply misses the frames it slept through.
async fn run_stream_socket(mut socket: WebSocket, topic: String, state: AppState) {
    let stream = state.hub.open_stream(&topic);
    let mut frames = stream.subscribe();

    loop {
        let frame = tokio::select! {
            changed = frames.changed() => {
                if changed.is_err() {
                    break;
                }
                frames.borrow_and_update().clone()
            }
            // An idle topic must still notice a browser that walked away.
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
                continue;
            }
        };
        let Some(frame) = frame else {
            continue;
        };
        if socket.send(Message::Binary(frame.jpeg.clone())).await.is_err() {
            break;
        }
    }

    state.hub.close_stream(&stream);
}
