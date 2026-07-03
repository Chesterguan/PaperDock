use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{Emitter, Manager};
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
    /// Draft batch citation-check: per-claim verdicts.
    Draft { claims: Vec<DraftItem> },
    Done,
    Error { message: String },
}

#[derive(Clone, serde::Serialize)]
pub struct DraftItem {
    pub claim: String,
    pub verdict: String,
    pub detail: String,
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
    history: String,
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
        "history": history,
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

    let mut child = match Command::new(python_for_worker(&app, &worker_path))
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

/// Prefer a real PaperQA interpreter (needs 3.11+); fall back to system
/// `python3` only if none is provisioned.
fn python_for_worker(app: &tauri::AppHandle, worker_path: &str) -> String {
    resolve_python(app, worker_path).unwrap_or_else(|| "python3".to_string())
}

/// The provisioned Python interpreter (for one-off tasks like PDF text
/// extraction). Same resolution as the worker uses.
pub fn interpreter(app: &tauri::AppHandle, worker_path: &str) -> String {
    python_for_worker(app, worker_path)
}

// ---- First-run Python environment provisioning -------------------------
//
// A packaged .app ships only the worker + a `uv` binary + a pinned
// requirements list — NOT the 287 MB venv. On first launch the app asks the
// user, then uses `uv` to fetch a relocatable Python and the deps into the app
// data dir. The app is online-only anyway (remote LLM + Qdrant), so a one-time
// online setup costs nothing, keeps the DMG ~50 MB, and works on any Mac arch.

/// The first-run-provisioned venv location (inside the app data dir).
fn pyenv_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("pyenv"))
}

/// A usable interpreter if one exists: the provisioned venv (release) or the
/// dev `.venv` next to the worker. `None` => first-run setup is needed.
fn resolve_python(app: &tauri::AppHandle, worker_path: &str) -> Option<String> {
    if let Some(py) = pyenv_dir(app).map(|d| d.join("bin/python")) {
        if py.exists() {
            return Some(py.to_string_lossy().into_owned());
        }
    }
    if let Some(dir) = Path::new(worker_path).parent() {
        let venv = dir.join(".venv/bin/python");
        if venv.exists() {
            return Some(venv.to_string_lossy().into_owned());
        }
    }
    None
}

/// True once a PaperQA-capable interpreter is available (no setup needed).
pub fn env_ready(app: &tauri::AppHandle, worker_path: &str) -> bool {
    resolve_python(app, worker_path).is_some()
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SetupEvent {
    Status { message: String },
    Done,
    Error { message: String },
}

/// Find the bundled `uv` (release resource, dev copy, or PATH).
fn uv_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join("bin/uv");
        if p.exists() {
            return Some(p);
        }
    }
    for p in ["src-tauri/bin/uv", "bin/uv"] {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    None
}

/// Find the pinned requirements list (release resource or dev copy).
fn reqs_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join("requirements.lock");
        if p.exists() {
            return Some(p);
        }
    }
    for p in ["sidecar/requirements.lock", "../sidecar/requirements.lock"] {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    None
}

/// Free bytes on the volume holding `dir` (via `df -Pk`). None if unknown.
fn free_bytes(dir: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(dir)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let avail_kb: u64 = text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_kb.saturating_mul(1024))
}

/// Provision the Python env with `uv`, streaming progress as `setup` events.
/// Preflight-checks disk/space/writability first so a first run can never wedge
/// or fill the user's machine. Never returns Err to the UI un-humanized.
pub async fn setup_env(app: tauri::AppHandle, worker_path: String) {
    let err = |m: String| {
        let _ = app.emit("setup", SetupEvent::Error { message: m });
    };
    let status = |m: &str| {
        let _ = app.emit("setup", SetupEvent::Status { message: m.to_string() });
    };

    if env_ready(&app, &worker_path) {
        let _ = app.emit("setup", SetupEvent::Done);
        return;
    }

    // ---- preflight: fail loud and early, never touch the system on failure --
    let (Some(uv), Some(reqs), Some(pyenv)) =
        (uv_path(&app), reqs_path(&app), pyenv_dir(&app))
    else {
        return err(
            "This build is missing its setup files (uv / requirements). \
             Please reinstall PaperDock."
                .into(),
        );
    };
    let Some(app_dir) = pyenv.parent().map(Path::to_path_buf) else {
        return err("Could not locate the app data folder.".into());
    };
    if std::fs::create_dir_all(&app_dir).is_err() {
        return err("Cannot write to the app data folder — check permissions.".into());
    }
    // ~1.5 GB headroom: managed Python (~80 MB) + deps (~300 MB) + build slack.
    const NEED: u64 = 1_500_000_000;
    if let Some(free) = free_bytes(&app_dir) {
        if free < NEED {
            return err(format!(
                "Not enough free disk space for setup — about 1.5 GB needed, \
                 {:.1} GB free. Free some space and try again.",
                free as f64 / 1e9
            ));
        }
    }

    // ---- provision (idempotent; a re-run repairs a half-built env) ----------
    status("Downloading Python (one-time)…");
    let pyenv_s = pyenv.to_string_lossy().into_owned();
    if !run_uv(&app, &uv, &["venv", &pyenv_s, "--python", "3.12"]).await {
        return err(
            "Could not set up Python. Check your internet connection and retry."
                .into(),
        );
    }
    status("Installing packages (this can take a minute)…");
    let py = format!("{pyenv_s}/bin/python");
    let reqs_s = reqs.to_string_lossy().into_owned();
    if !run_uv(&app, &uv, &["pip", "install", "--python", &py, "-r", &reqs_s]).await {
        return err(
            "Could not install the reading packages. Check your connection and retry."
                .into(),
        );
    }

    if env_ready(&app, &worker_path) {
        status("Ready.");
        let _ = app.emit("setup", SetupEvent::Done);
    } else {
        err("Setup finished but the environment is incomplete. Please retry.".into());
    }
}

/// Run a `uv` subcommand, streaming its stderr progress lines to the UI.
/// Returns true on a clean exit. Forces a managed (relocatable) Python so the
/// interpreter is identical on every machine, never the user's system one.
async fn run_uv(app: &tauri::AppHandle, uv: &Path, uv_args: &[&str]) -> bool {
    let mut child = match Command::new(uv)
        .args(uv_args)
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(stderr) = child.stderr.take() {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            // uv's progress lines are terse and safe to show; skip blanks.
            if !line.is_empty() {
                let _ = app.emit(
                    "setup",
                    SetupEvent::Status { message: line.to_string() },
                );
            }
        }
    }
    matches!(child.wait().await, Ok(s) if s.success())
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
        "draft" => {
            let items = v
                .get("items")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|it| DraftItem {
                            claim: it
                                .get("claim")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            verdict: it
                                .get("verdict")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            detail: it
                                .get("detail")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(AnswerEvent::Draft { claims: items })
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
