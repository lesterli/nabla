use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

const HERMES_BASE: &str = "http://127.0.0.1:8642";

struct AppState {
    client: Client,
    session_id: Mutex<Option<String>>,
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

#[tauri::command]
async fn health_check(state: State<'_, AppState>) -> Result<bool, String> {
    let resp = state
        .client
        .get(format!("{HERMES_BASE}/health"))
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;
    Ok(resp.status().is_success())
}

#[tauri::command]
async fn send_message(
    app: AppHandle,
    state: State<'_, AppState>,
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

        // Decode only complete UTF-8 from the byte buffer
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

        // Keep only unprocessed bytes
        raw_buf.drain(..search_start);
    }

    let _ = app.emit("hermes://done", DoneEvent {
        session_id: returned_session.clone(),
    });

    Ok(returned_session)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            client: Client::new(),
            session_id: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![health_check, send_message])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
