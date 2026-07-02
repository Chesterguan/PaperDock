use std::process::Stdio;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AnswerEvent {
    Status { text: String },
    Token { text: String },
    References { items: Vec<RefItem> },
    /// A non-fatal heads-up (e.g. some papers had no PDF and were skipped).
    Notice { message: String },
    Done,
    Error { message: String },
}

#[derive(Clone, serde::Serialize)]
pub struct RefItem {
    pub item_key: String,
    pub citation: String,
    #[serde(default)]
    pub passages: Vec<Passage>,
}

#[derive(Clone, serde::Serialize)]
pub struct Passage {
    pub page: String,
    pub snippet: String,
}

pub type ChildSlot = Arc<Mutex<Option<Child>>>;

fn emit(app: &tauri::AppHandle, ev: AnswerEvent) {
    let _ = app.emit("answer", ev);
}

/// Spawn the Python worker, send one `ask` request, stream its JSON-line
/// output back to the front as `answer` events. The spawned child is stored in
/// `child_slot` so `cancel()` can kill it.
pub async fn run_ask(
    app: tauri::AppHandle,
    worker_path: String,
    cmd: &str,
    request_id: String,
    question: String,
    model: String,
    embedding: String,
    api_base: String,
    cache_dir: String,
    api_key: String,
    qdrant_url: String,
    qdrant_key: String,
    collection_key: String,
    docs: Vec<crate::zotero::DocRef>,
    child_slot: ChildSlot,
) -> Result<(), String> {
    let docs_json: Vec<Value> = docs
        .iter()
        .map(|d| {
            json!({
                "path": d.path,
                "zotero_key": d.zotero_key,
                "citation": d.citation,
            })
        })
        .collect();

    let request = json!({
        "id": request_id,
        "cmd": cmd,
        "question": question,
        "index_name": cache_dir_index_name(&cache_dir),
        "cache_dir": cache_dir,
        "model": model,
        "embedding": embedding,
        "api_base": api_base,
        "api_key": api_key,
        "qdrant_url": qdrant_url,
        "qdrant_key": qdrant_key,
        "collection_key": collection_key,
        "docs": docs_json,
    });

    let mut line = match serde_json::to_string(&request) {
        Ok(s) => s,
        Err(_) => {
            emit(
                &app,
                AnswerEvent::Error {
                    message: "Could not build the query request.".to_string(),
                },
            );
            return Ok(());
        }
    };
    line.push('\n');

    let mut child = match Command::new(python_for_worker(&worker_path))
        .arg(&worker_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            emit(
                &app,
                AnswerEvent::Error {
                    message: "Could not start the query engine.".to_string(),
                },
            );
            return Ok(());
        }
    };

    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            emit(
                &app,
                AnswerEvent::Error {
                    message: "Could not connect to the query engine.".to_string(),
                },
            );
            return Ok(());
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            emit(
                &app,
                AnswerEvent::Error {
                    message: "Could not read from the query engine.".to_string(),
                },
            );
            return Ok(());
        }
    };

    // Stash the child so cancel() can reach it.
    {
        let mut slot = child_slot.lock().await;
        *slot = Some(child);
    }

    if stdin.write_all(line.as_bytes()).await.is_err() {
        emit(
            &app,
            AnswerEvent::Error {
                message: "Could not send the question to the query engine.".to_string(),
            },
        );
        clear_slot(&child_slot).await;
        return Ok(());
    }
    let _ = stdin.shutdown().await;
    drop(stdin);

    let mut reader = BufReader::new(stdout).lines();
    loop {
        match reader.next_line().await {
            Ok(Some(raw)) => {
                let raw = raw.trim();
                if raw.is_empty() {
                    continue;
                }
                if let Some(ev) = parse_event(raw) {
                    let done = matches!(ev, AnswerEvent::Done | AnswerEvent::Error { .. });
                    emit(&app, ev);
                    if done {
                        break;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => {
                emit(
                    &app,
                    AnswerEvent::Error {
                        message: "The query engine stopped unexpectedly.".to_string(),
                    },
                );
                break;
            }
        }
    }

    clear_slot(&child_slot).await;
    Ok(())
}

/// Kill the currently running worker, if any.
pub async fn cancel(child_slot: ChildSlot) {
    let mut slot = child_slot.lock().await;
    if let Some(mut child) = slot.take() {
        let _ = child.kill().await;
    }
}

async fn clear_slot(child_slot: &ChildSlot) {
    let mut slot = child_slot.lock().await;
    if let Some(mut child) = slot.take() {
        let _ = child.start_kill();
    }
}

/// Prefer the sidecar's bundled venv interpreter (PaperQA needs 3.11+); fall
/// back to system `python3` (e.g. the stdlib mock) if no venv is present.
fn python_for_worker(worker_path: &str) -> String {
    if let Some(dir) = std::path::Path::new(worker_path).parent() {
        let venv = dir.join(".venv/bin/python");
        if venv.exists() {
            return venv.to_string_lossy().into_owned();
        }
    }
    "python3".to_string()
}

fn cache_dir_index_name(cache_dir: &str) -> String {
    std::path::Path::new(cache_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("paperdock")
        .to_string()
}

/// Translate one worker JSON line into an AnswerEvent. Unknown types are
/// ignored (returns None).
fn parse_event(raw: &str) -> Option<AnswerEvent> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let ty = v.get("type").and_then(Value::as_str)?;
    match ty {
        "status" => Some(AnswerEvent::Status {
            text: v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "token" => Some(AnswerEvent::Token {
            text: v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "references" => {
            let items = v
                .get("items")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|it| RefItem {
                            item_key: it
                                .get("zotero_key")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            citation: it
                                .get("citation")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            passages: it
                                .get("passages")
                                .and_then(Value::as_array)
                                .map(|ps| {
                                    ps.iter()
                                        .map(|p| Passage {
                                            page: p
                                                .get("page")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                            snippet: p
                                                .get("snippet")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(AnswerEvent::References { items })
        }
        "done" => Some(AnswerEvent::Done),
        "error" => Some(AnswerEvent::Error {
            message: v
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The query engine reported an error.")
                .to_string(),
        }),
        _ => None,
    }
}
