use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const HERMES_BASE: &str = "http://127.0.0.1:8642";

struct AppState {
    client: Client,
    session_id: Mutex<Option<String>>,
    hermes_process: Mutex<Option<Child>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct StreamEvent {
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct DoneEvent {
    session_id: String,
}

async fn start_hermes(state: &AppState) -> Result<(), String> {
    // Check if already running
    let resp = state.client.get(format!("{HERMES_BASE}/health")).send().await;
    if resp.is_ok_and(|r| r.status().is_success()) {
        return Ok(());
    }

    let child = Command::new("hermes")
        .arg("gateway")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to start hermes gateway: {e}"))?;

    *state.hermes_process.lock().await = Some(child);

    // Wait for health check (up to 15s)
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(resp) = state.client.get(format!("{HERMES_BASE}/health")).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
    }

    Err("Hermes gateway failed to start within 15s".to_string())
}

#[tauri::command]
async fn health_check(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    start_hermes(&state).await?;
    Ok(true)
}

#[tauri::command]
async fn send_message(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    history: Vec<ChatMessage>,
) -> Result<String, String> {
    let mut messages: Vec<serde_json::Value> = history
        .into_iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .collect();
    messages.push(serde_json::json!({ "role": "user", "content": message }));

    let body = serde_json::json!({
        "model": "hermes-agent",
        "messages": messages,
        "stream": true,
    });

    let session_id = state.session_id.lock().await.clone();
    let mut req = state
        .client
        .post(format!("{HERMES_BASE}/v1/chat/completions"))
        .json(&body);

    if let Some(ref sid) = session_id {
        req = req.header("X-Hermes-Session-Id", sid);
    }

    let resp = req.send().await.map_err(|e| format!("Request failed: {e}"))?;

    let new_session_id = resp
        .headers()
        .get("x-hermes-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    if new_session_id.is_some() {
        *state.session_id.lock().await = new_session_id.clone();
    }

    let returned_session = new_session_id.or(session_id).unwrap_or_default();

    let mut stream = resp.bytes_stream();
    let mut raw_buf: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
        raw_buf.extend_from_slice(&chunk);

        let valid_up_to = match std::str::from_utf8(&raw_buf) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        if valid_up_to == 0 {
            continue;
        }

        let valid_str = std::str::from_utf8(&raw_buf[..valid_up_to]).unwrap();
        let mut search_start = 0;

        while let Some(rel_pos) = valid_str[search_start..].find('\n') {
            let pos = search_start + rel_pos;
            let line = valid_str[search_start..pos].trim();
            search_start = pos + 1;

            if line.is_empty() || !line.starts_with("data: ") {
                continue;
            }

            let data = &line[6..];
            if data == "[DONE]" {
                let _ = app.emit("hermes://done", DoneEvent {
                    session_id: returned_session.clone(),
                });
                return Ok(returned_session);
            }

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(content) = parsed
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !content.is_empty() {
                        let _ = app.emit("hermes://stream", StreamEvent {
                            content: content.to_string(),
                        });
                    }
                }
            }
        }

        raw_buf.drain(..search_start);
    }

    let _ = app.emit("hermes://done", DoneEvent {
        session_id: returned_session.clone(),
    });

    Ok(returned_session)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = Arc::new(AppState {
        client: Client::new(),
        session_id: Mutex::new(None),
        hermes_process: Mutex::new(None),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![health_check, send_message])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
