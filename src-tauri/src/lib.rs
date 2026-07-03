mod config;
mod sidecar;
mod zotero;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use tauri::{Emitter, Manager, State};

use config::Config;
use sidecar::{AnswerEvent, ChildSlot};
use zotero::{Collection, DocRef};

/// Shared application state managed by Tauri.
pub struct AppState {
    /// Persisted user config (last collection, data dir, model).
    pub config: Mutex<Config>,
    /// Handle to the currently running worker child, so `cancel` can kill it.
    pub child: ChildSlot,
    /// Absolute path to the Python sidecar worker, resolved at startup.
    pub worker_path: String,
}

/// Monotonic counter used to tag each ask request with a unique id.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

// ----- Tauri commands ------------------------------------------------------

/// True when the local Zotero HTTP API is reachable.
#[tauri::command]
async fn zotero_status() -> bool {
    zotero::is_running().await
}

/// List the user's Zotero collections.
#[tauri::command]
async fn list_collections() -> Result<Vec<Collection>, String> {
    zotero::list_collections().await
}

/// Resolve the collection's PDFs and spawn a streaming answer task.
///
/// Returns immediately; the answer arrives as `"answer"` events. If the
/// collection has no resolvable PDFs, an error event is emitted instead.
#[tauri::command]
async fn ask(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library: String,
    collection_key: String,
    question: String,
    #[allow(non_snake_case)] history: Option<String>,
) -> Result<(), String> {
    spawn_worker(
        app,
        state,
        "ask",
        library,
        collection_key,
        question,
        history.unwrap_or_default(),
        None,
    )
    .await
}

/// Citation-check: judge whether the collection's papers support a claim.
/// Reuses the whole embed/retrieve pipeline with a verdict prompt (worker
/// swaps the answer prompt when `cmd == "check"`). `claim` rides the `question`
/// slot; no conversation history.
#[tauri::command]
async fn check(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library: String,
    collection_key: String,
    claim: String,
    #[allow(non_snake_case)] source_key: Option<String>,
) -> Result<(), String> {
    let sk = source_key.filter(|s| !s.is_empty());
    spawn_worker(app, state, "check", library, collection_key, claim, String::new(), sk).await
}

/// Draft batch citation-check: extract claims from a pasted draft and judge each
/// against the collection. Results arrive as a `draft` answer event.
#[tauri::command]
async fn check_draft(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library: String,
    collection_key: String,
    draft: String,
) -> Result<(), String> {
    spawn_worker(app, state, "check_draft", library, collection_key, draft, String::new(), None).await
}

/// Pick a draft file (.txt/.md/.tex/.pdf) and return its text — for Check draft.
#[tauri::command]
async fn pick_draft_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let file = app
        .dialog()
        .file()
        .add_filter("Draft", &["txt", "md", "markdown", "tex", "pdf"])
        .blocking_pick_file();
    let Some(file) = file else { return Ok(String::new()) };
    let path = file
        .into_path()
        .map_err(|_| "Could not resolve the file path.".to_string())?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "pdf" {
        let py = sidecar::interpreter(&app, &state.worker_path);
        let out = std::process::Command::new(py)
            .arg("-c")
            .arg("import sys,pypdf;r=pypdf.PdfReader(sys.argv[1]);print('\\n'.join((p.extract_text() or '') for p in r.pages))")
            .arg(&path)
            .output()
            .map_err(|_| "Could not read the PDF.".to_string())?;
        if !out.status.success() {
            return Err("Could not extract text from that PDF.".to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        std::fs::read_to_string(&path).map_err(|_| "Could not read the file.".to_string())
    }
}

/// One checkable paper (has a PDF) in a collection — for the citation-check
/// source picker.
#[derive(serde::Serialize)]
struct PaperRef {
    key: String,
    citation: String,
}

/// Closest CrossRef match for a reference — the "is this citation real?" prescreen.
#[derive(serde::Serialize)]
struct RefMatch {
    found: bool,
    /// % of the matched title's significant words present in the input reference.
    /// High = the closest real paper matches what you cited; low = CrossRef's
    /// best guess is unrelated, so the citation may be fabricated/mis-cited.
    confidence: u8,
    title: String,
    doi: String,
    authors: String,
    year: String,
}

/// Fraction (0-100) of the matched title's significant words that appear in the
/// reference the user pasted. CrossRef always returns a top hit, so this is what
/// separates "real match" from "unrelated best-guess".
fn title_overlap(reference: &str, title: &str) -> u8 {
    const STOP: &[&str] = &[
        "the", "of", "and", "for", "with", "from", "using", "via", "based",
    ];
    let words = |s: &str| -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2 && !STOP.contains(w))
            .map(|w| w.to_string())
            .collect()
    };
    let tw = words(title);
    let rw = words(reference);
    if tw.is_empty() || rw.is_empty() {
        return 0;
    }
    let inter = tw.iter().filter(|w| rw.contains(*w)).count();
    // Bidirectional: the match must both look like the title AND cover the
    // distinctive words of the pasted reference. Taking the min rejects an
    // adversarial fake that merely shares generic terms with a real paper.
    let title_cov = inter * 100 / tw.len();
    let ref_cov = inter * 100 / rw.len();
    title_cov.min(ref_cov) as u8
}

/// Verify a reference against CrossRef (refchecker-style prescreen): is it a
/// real published paper? Returns the closest match's metadata, or found=false.
#[tauri::command]
async fn verify_reference(reference: String) -> Result<RefMatch, String> {
    let q = reference.trim();
    if q.is_empty() {
        return Err("Paste a reference to verify.".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("PaperDock/0.2 (mailto:paperdock@example.com)")
        .build()
        .map_err(|_| "Could not start the lookup.".to_string())?;
    let resp = client
        .get("https://api.crossref.org/works")
        .query(&[
            ("query.bibliographic", q),
            ("rows", "1"),
            ("select", "title,DOI,author,issued"),
        ])
        .send()
        .await
        .map_err(|_| "Could not reach CrossRef — check your connection.".to_string())?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| "CrossRef returned an unexpected response.".to_string())?;
    let Some(item) = v
        .get("message")
        .and_then(|m| m.get("items"))
        .and_then(|i| i.get(0))
    else {
        return Ok(RefMatch {
            found: false,
            confidence: 0,
            title: String::new(),
            doi: String::new(),
            authors: String::new(),
            year: String::new(),
        });
    };
    let title = item
        .get("title")
        .and_then(|t| t.get(0))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let doi = item
        .get("DOI")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let year = item
        .get("issued")
        .and_then(|i| i.get("date-parts"))
        .and_then(|d| d.get(0))
        .and_then(|d| d.get(0))
        .and_then(|y| y.as_i64())
        .map(|y| y.to_string())
        .unwrap_or_default();
    let authors = item
        .get("author")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .take(3)
                .filter_map(|au| au.get("family").and_then(|f| f.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let confidence = title_overlap(q, &title);
    Ok(RefMatch {
        found: !title.is_empty(),
        confidence,
        title,
        doi,
        authors,
        year,
    })
}

/// List the papers (with PDFs) in a collection, so Check mode can target one.
#[tauri::command]
async fn list_collection_papers(
    state: State<'_, AppState>,
    library: String,
    collection_key: String,
) -> Result<Vec<PaperRef>, String> {
    let data_dir = {
        state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?
            .zotero_data_dir
            .clone()
    };
    let resolved = zotero::collection_docs(&library, &collection_key, &data_dir).await?;
    Ok(resolved
        .docs
        .into_iter()
        .map(|d| PaperRef {
            key: d.zotero_key,
            citation: d.citation,
        })
        .collect())
}

/// Pre-embed a collection into the shared index (no LLM query), so later asks
/// are instant and the group's shared vectors stay complete.
#[tauri::command]
async fn index_collection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    library: String,
    collection_key: String,
) -> Result<(), String> {
    spawn_worker(
        app,
        state,
        "index",
        library,
        collection_key,
        String::new(),
        String::new(),
        None,
    )
    .await
}

/// Shared path for ask + index: resolve the collection's PDFs and spawn the
/// worker with the given command. Returns immediately; progress/results arrive
/// as `"answer"` events.
async fn spawn_worker(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cmd: &'static str,
    library: String,
    collection_key: String,
    question: String,
    history: String,
    source_key: Option<String>,
) -> Result<(), String> {
    // Snapshot the config values we need without holding the lock across await.
    let (model, embedding, api_base, data_dir, api_key, qdrant_url, qdrant_key) = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?;
        (
            cfg.model.clone(),
            cfg.embedding.clone(),
            cfg.api_base.clone().unwrap_or_default(),
            cfg.zotero_data_dir.clone(),
            cfg.api_key.clone().unwrap_or_default(),
            cfg.qdrant_url.clone().unwrap_or_default(),
            cfg.qdrant_api_key.clone().unwrap_or_default(),
        )
    };

    let resolved = zotero::collection_docs(&library, &collection_key, &data_dir).await?;
    let mut docs: Vec<DocRef> = resolved.docs;
    let skipped = resolved.skipped;
    // Citation-check may target ONE paper (the specific cited source) instead of
    // the whole collection.
    if let Some(sk) = &source_key {
        docs.retain(|d| &d.zotero_key == sk);
    }

    // Scope the shared Qdrant index per library+collection so group and
    // personal collections with the same key never collide.
    let scope = format!("{}_{}", library.replace('/', "_"), collection_key);

    if docs.is_empty() {
        let msg = if skipped.is_empty() {
            "No PDFs found in this collection.".to_string()
        } else {
            format!(
                "None of the {} papers have a PDF downloaded. Open them in Zotero and \
                 sync/download the PDFs, then try again.",
                skipped.len()
            )
        };
        let _ = app.emit("answer", AnswerEvent::Error { message: msg });
        return Ok(());
    }

    // Partial coverage: some papers have no PDF on disk — tell the user which,
    // so a silently-narrowed answer never reads as complete.
    if !skipped.is_empty() {
        let total = docs.len() + skipped.len();
        let mut names = skipped.clone();
        names.truncate(6);
        let more = skipped.len().saturating_sub(names.len());
        let mut list = names.join(", ");
        if more > 0 {
            list.push_str(&format!(", +{more} more"));
        }
        let _ = app.emit(
            "answer",
            AnswerEvent::Notice {
                message: format!(
                    "Answering from {} of {} papers — {} have no PDF downloaded: {}",
                    docs.len(),
                    total,
                    skipped.len(),
                    list
                ),
            },
        );
    }

    let n = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let request_id = format!("q{n}");

    let cache_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve cache directory: {e}"))?
        .join("paperqa_index")
        .to_string_lossy()
        .into_owned();

    let worker_path = state.worker_path.clone();
    let child_slot = state.child.clone();
    let app_handle = app.clone();

    tauri::async_runtime::spawn(async move {
        if let Err(message) = sidecar::run_ask(
            app_handle.clone(),
            worker_path,
            cmd,
            request_id,
            question,
            history,
            model,
            embedding,
            api_base,
            cache_dir,
            api_key,
            qdrant_url,
            qdrant_key,
            scope,
            docs,
            child_slot,
        )
        .await
        {
            let _ = app_handle.emit("answer", AnswerEvent::Error { message });
        }
    });

    Ok(())
}

/// Kill the running worker, if any.
#[tauri::command]
async fn cancel(state: State<'_, AppState>) -> Result<(), String> {
    sidecar::cancel(state.child.clone()).await;
    Ok(())
}

/// True once the Python reading environment is ready (skips first-run setup).
#[tauri::command]
fn env_status(app: tauri::AppHandle, state: State<'_, AppState>) -> bool {
    sidecar::env_ready(&app, &state.worker_path)
}

/// Provision the Python environment on first run. Preflights disk/space, then
/// streams `setup` events (status / done / error) as `uv` works.
#[tauri::command]
async fn setup_env(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let worker_path = state.worker_path.clone();
    sidecar::setup_env(app, worker_path).await;
    Ok(())
}

/// Open the given item in the Zotero desktop app via `zotero://select`.
/// `library` is "users/0" (My Library) or "groups/<id>" — group items need the
/// group path in the URI or Zotero selects the wrong/no item.
#[tauri::command]
fn open_in_zotero(library: String, item_key: String) -> Result<(), String> {
    let uri = if library.starts_with("groups/") {
        format!("zotero://select/{library}/items/{item_key}")
    } else {
        format!("zotero://select/library/items/{item_key}")
    };
    std::process::Command::new("open")
        .arg(uri)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Could not open Zotero.".to_string())
}

/// Frontend-safe view of config — never includes the raw API key.
#[derive(serde::Serialize)]
struct UiConfig {
    last_collection: Option<String>,
    model: String,
    embedding: String,
    api_base: String,
    qdrant_url: String,
    has_qdrant_key: bool,
    /// True if a usable key exists (env var or saved), so the UI can hide the
    /// key prompt.
    has_api_key: bool,
    /// User's research field (feedback dimension).
    field: String,
    /// Feedback opt-in: null = not asked, true/false = decided.
    tele_consent: Option<bool>,
}

/// Open an external URL in the default browser (used for the PaperQA credit).
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    // Only allow http(s) so a crafted URL can't run `open` on something local.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Refusing to open a non-web URL.".to_string());
    }
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Could not open the link.".to_string())
}

/// Put an HTML note on the clipboard (with a plain-text fallback) so pasting into
/// a Zotero note — which is HTML rich text — renders nicely.
#[tauri::command]
fn copy_html(html: String, plain: String) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|_| "Could not access the clipboard.".to_string())?;
    cb.set()
        .html(html.as_str(), Some(plain.as_str()))
        .map_err(|_| "Could not write to the clipboard.".to_string())?;
    Ok(())
}

/// Where a note landed after saving to Zotero.
#[derive(serde::Serialize)]
struct SavedNote {
    /// Library it went to (e.g. "My Library" or a group name).
    location: String,
    /// True if a shared group library (warn the user).
    is_group: bool,
    /// `zotero://select/...` link to open the note.
    link: String,
}

/// Save an HTML note DIRECTLY into Zotero via the connector, then locate it to
/// return where it landed + a `zotero://` link. Note: the connector saves to
/// Zotero's currently-open collection (we can't override the target), so we
/// report the destination and flag shared group libraries.
#[tauri::command]
async fn save_to_zotero(html: String) -> Result<SavedNote, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| "Could not start the save.".to_string())?;
    let sid = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let save = client
        .post("http://localhost:23119/connector/saveItems")
        .header("X-Zotero-Connector-API-Version", "3")
        .header("User-Agent", "PaperDock")
        .json(&serde_json::json!({
            "sessionID": format!("paperdock-{sid}"),
            "items": [{ "itemType": "note", "note": html }],
        }))
        .send()
        .await
        .map_err(|_| "Zotero isn't reachable — make sure it's open.".to_string())?;
    if !save.status().is_success() {
        return Err("Zotero didn't accept the note.".to_string());
    }
    // Let Zotero persist, then find the just-created note (newest across libraries).
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;

    // Candidate libraries: personal + each group.
    let mut candidates: Vec<(String, String, bool)> =
        vec![("users/0".to_string(), "My Library".to_string(), false)];
    if let Ok(resp) = client
        .get("http://localhost:23119/api/users/0/groups")
        .send()
        .await
    {
        if let Ok(v) = resp.json::<serde_json::Value>().await {
            if let Some(arr) = v.as_array() {
                for g in arr {
                    if let (Some(id), Some(name)) = (
                        g.get("id").and_then(|x| x.as_u64()),
                        g.get("data")
                            .and_then(|d| d.get("name"))
                            .and_then(|n| n.as_str()),
                    ) {
                        candidates.push((format!("groups/{id}"), name.to_string(), true));
                    }
                }
            }
        }
    }
    // Pick the globally-newest note — that's the one we just created.
    let mut best: Option<(String, SavedNote)> = None;
    for (path, name, is_group) in &candidates {
        let url = format!(
            "http://localhost:23119/api/{path}/items?itemType=note&sort=dateAdded&direction=desc&limit=1"
        );
        let Ok(resp) = client.get(&url).send().await else { continue };
        let Ok(v) = resp.json::<serde_json::Value>().await else { continue };
        let Some(it) = v.as_array().and_then(|a| a.first()) else { continue };
        let data = it.get("data");
        let key = data
            .and_then(|d| d.get("key"))
            .and_then(|k| k.as_str())
            .unwrap_or("");
        let date = data
            .and_then(|d| d.get("dateAdded"))
            .and_then(|k| k.as_str())
            .unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let link = if *is_group {
            let gid = path.trim_start_matches("groups/");
            format!("zotero://select/groups/{gid}/items/{key}")
        } else {
            format!("zotero://select/library/items/{key}")
        };
        let newer = best.as_ref().map(|(d, _)| date > d.as_str()).unwrap_or(true);
        if newer {
            best = Some((
                date.to_string(),
                SavedNote {
                    location: name.clone(),
                    is_group: *is_group,
                    link,
                },
            ));
        }
    }
    best.map(|(_, n)| n)
        .ok_or_else(|| "Saved to Zotero, but couldn't locate the note.".to_string())
}

/// Open a `zotero://` link (used to jump to a saved note).
#[tauri::command]
fn open_zotero_uri(uri: String) -> Result<(), String> {
    if !uri.starts_with("zotero://") {
        return Err("Not a Zotero link.".to_string());
    }
    std::process::Command::new("open")
        .arg(uri)
        .spawn()
        .map(|_| ())
        .map_err(|_| "Could not open Zotero.".to_string())
}

/// Return the frontend-safe config (no secrets).
#[tauri::command]
fn get_config(state: State<'_, AppState>) -> UiConfig {
    let cfg = state.config.lock().map(|c| c.clone()).unwrap_or_default();
    let has_saved = cfg.api_key.as_deref().is_some_and(|k| !k.trim().is_empty());
    let has_env = std::env::var("OPENAI_API_KEY").is_ok();
    UiConfig {
        last_collection: cfg.last_collection,
        model: cfg.model,
        embedding: cfg.embedding,
        api_base: cfg.api_base.unwrap_or_default(),
        qdrant_url: cfg.qdrant_url.unwrap_or_default(),
        has_qdrant_key: cfg
            .qdrant_api_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty()),
        has_api_key: has_saved || has_env,
        field: cfg.field,
        tele_consent: cfg.tele_consent,
    }
}

/// URL of the feedback collector (a Cloudflare Worker on *.workers.dev).
/// Empty = telemetry disabled at build time; feedback POSTs are skipped.
const FEEDBACK_URL: &str = "";

/// Record a 👍/👎 on the last answer. Persists the user's consent + field, and
/// (only with consent + a configured collector) POSTs a minimal, content-free
/// event: rating, coarse field, app version. Never sends papers or questions.
#[tauri::command]
async fn submit_feedback(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    rating: String,
    field: String,
    consent: bool,
) -> Result<(), String> {
    {
        let mut cfg = state.config.lock().map_err(|_| "config".to_string())?;
        cfg.field = field.clone();
        cfg.tele_consent = Some(consent);
        let _ = cfg.save(&app);
    }
    if consent && !FEEDBACK_URL.is_empty() {
        let body = serde_json::json!({
            "rating": rating,
            "field": field,
            "version": env!("CARGO_PKG_VERSION"),
        });
        // Fire-and-forget; a failed send must never disrupt the user.
        if let Ok(client) = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(6))
            .build()
        {
            let _ = client.post(FEEDBACK_URL).json(&body).send().await;
        }
    }
    Ok(())
}

/// Save the LiteLLM API key entered in the UI (empty string clears it).
#[tauri::command]
fn set_api_key(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> Result<(), String> {
    let trimmed = key.trim().to_string();
    let cfg = {
        let mut guard = state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?;
        guard.api_key = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        guard.clone()
    };
    cfg.save(&app)
}

/// Save model / embedding / base-url settings (empty base = provider default).
#[tauri::command]
fn set_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    model: String,
    embedding: String,
    api_base: String,
    qdrant_url: String,
    qdrant_key: String,
) -> Result<(), String> {
    let cfg = {
        let mut guard = state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?;
        let m = model.trim();
        let e = embedding.trim();
        if !m.is_empty() {
            guard.model = m.to_string();
        }
        if !e.is_empty() {
            guard.embedding = e.to_string();
        }
        let opt = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        guard.api_base = opt(&api_base);
        guard.qdrant_url = opt(&qdrant_url);
        // Blank key field leaves the saved key untouched (so you don't have to
        // re-paste it every time); to clear it, blank the URL too.
        if let Some(k) = opt(&qdrant_key) {
            guard.qdrant_api_key = Some(k);
        }
        if guard.qdrant_url.is_none() {
            guard.qdrant_api_key = None;
        }
        guard.clone()
    };
    cfg.save(&app)
}

/// Remember the last selected collection and persist config.
#[tauri::command]
fn set_last_collection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> Result<(), String> {
    let cfg = {
        let mut guard = state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?;
        guard.last_collection = Some(key);
        guard.clone()
    };
    cfg.save(&app)
}

/// Read a `.paperdock` file, merge it into the live config, persist, and tell
/// the UI. Returns the lab's display name. Never leaks a raw parse error.
fn apply_lab_file(app: &tauri::AppHandle, path: &std::path::Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|_| "Could not read that lab config file.".to_string())?;
    let lab: config::LabConfig = serde_json::from_str(&raw)
        .map_err(|_| "That doesn't look like a PaperDock lab config file.".to_string())?;
    let name = lab.summary_name();
    let saved = {
        let state = app.state::<AppState>();
        let mut guard = state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?;
        guard.apply_lab_config(&lab);
        guard.clone()
    };
    saved.save(app)?;
    let _ = app.emit("lab-imported", name.clone());
    Ok(name)
}

/// Export the shared config as a `.paperdock` file the admin distributes.
/// Opens a save dialog; returns the written path, or "" if cancelled.
#[tauri::command]
async fn export_lab_config(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    include_key: bool,
) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let lab = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| "Config is unavailable.".to_string())?;
        cfg.to_lab_config(include_key)
    };
    let json = serde_json::to_string_pretty(&lab)
        .map_err(|_| "Could not build the lab config.".to_string())?;
    let file = app
        .dialog()
        .file()
        .add_filter("PaperDock lab config", &["paperdock"])
        .set_file_name("lab.paperdock")
        .blocking_save_file();
    let Some(file) = file else { return Ok(String::new()) };
    let path = file
        .into_path()
        .map_err(|_| "Could not resolve the save path.".to_string())?;
    std::fs::write(&path, json).map_err(|_| "Could not write the file.".to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Import a `.paperdock` file chosen via an open dialog. Returns the lab name.
#[tauri::command]
async fn import_lab_config(
    app: tauri::AppHandle,
    _state: State<'_, AppState>,
) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;
    let file = app
        .dialog()
        .file()
        .add_filter("PaperDock lab config", &["paperdock"])
        .blocking_pick_file();
    let Some(file) = file else { return Ok(String::new()) };
    let path = file
        .into_path()
        .map_err(|_| "Could not resolve the file path.".to_string())?;
    apply_lab_file(&app, &path)
}

// ----- Setup helpers -------------------------------------------------------

/// Resolve the sidecar worker path, trying dev-relative locations first, then
/// the bundled resource directory.
fn resolve_worker_path(app: &tauri::AppHandle) -> String {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("../sidecar/paperdock_worker.py"),
        PathBuf::from("sidecar/paperdock_worker.py"),
    ];
    if let Ok(res_dir) = app.path().resource_dir() {
        candidates.push(res_dir.join("paperdock_worker.py"));
    }
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    candidates[0].to_string_lossy().into_owned()
}

/// Build, configure, and run the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle();
            let worker_path = resolve_worker_path(handle);
            let cfg = Config::load(handle);
            app.manage(AppState {
                config: Mutex::new(cfg),
                child: Arc::new(tokio::sync::Mutex::new(None)),
                worker_path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            zotero_status,
            list_collections,
            ask,
            check,
            check_draft,
            pick_draft_file,
            list_collection_papers,
            verify_reference,
            copy_html,
            save_to_zotero,
            open_zotero_uri,
            submit_feedback,
            index_collection,
            cancel,
            env_status,
            setup_env,
            open_in_zotero,
            open_url,
            get_config,
            set_last_collection,
            set_api_key,
            set_settings,
            export_lab_config,
            import_lab_config,
        ])
        .build(tauri::generate_context!())
        .expect("error while building PaperDock")
        .run(|app, event| {
            if let tauri::RunEvent::Opened { urls } = event {
                for url in urls {
                    // macOS delivers file:// URLs for associated files.
                    if let Ok(path) = url.to_file_path() {
                        if path.extension().and_then(|e| e.to_str()) == Some("paperdock") {
                            let _ = apply_lab_file(app, &path);
                        }
                    }
                }
            }
        });
}
