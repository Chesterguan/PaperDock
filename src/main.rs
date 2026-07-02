//! PaperDock v1 — Leptos CSR frontend.
//!
//! One window, one centered column: collection dropdown, status line, ask
//! input, streamed answer, clickable citation chips. Talks to the Tauri 2
//! backend via `window.__TAURI__` (withGlobalTauri). See the frozen Rust
//! module contract in the project spec.

use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

// ---------------------------------------------------------------------------
// Tauri bindings (withGlobalTauri). Exact pattern mandated by the contract.
// ---------------------------------------------------------------------------
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen, catch)]
    async fn listen(event: &str, handler: &JsValue) -> Result<JsValue, JsValue>;
}

/// Build a Tauri args object from a JSON value. Tauri v2 maps camelCase JS arg
/// keys onto snake_case Rust command params.
fn args(v: serde_json::Value) -> JsValue {
    serde_wasm_bindgen::to_value(&v).unwrap_or(JsValue::NULL)
}

/// Build a click handler that opens an external URL in the default browser.
fn open_link(url: &'static str) -> impl Fn(web_sys::MouseEvent) + Clone {
    move |ev: web_sys::MouseEvent| {
        ev.prevent_default();
        spawn_local(async move {
            let _ = invoke("open_url", args(serde_json::json!({ "url": url }))).await;
        });
    }
}

// ---------------------------------------------------------------------------
// Wire types (mirror the backend's Serialize shapes).
// ---------------------------------------------------------------------------
#[derive(Clone, Deserialize)]
struct Collection {
    key: String,
    name: String,
    num_items: u32,
    #[serde(default)]
    library: String,
    #[serde(default)]
    library_name: String,
}

impl Collection {
    /// Stable id encoding library + key ("groups/6597011::ABCD1234").
    fn id(&self) -> String {
        format!("{}::{}", self.library, self.key)
    }
}

#[derive(Clone, Deserialize)]
struct Config {
    #[serde(default)]
    last_collection: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    embedding: String,
    #[serde(default)]
    api_base: String,
    #[serde(default)]
    qdrant_url: String,
    // True when a key (env or saved) exists, so the UI can hide the key prompt.
    #[serde(default)]
    has_api_key: bool,
}

#[derive(Clone, Deserialize)]
struct RefItem {
    item_key: String,
    citation: String,
    #[serde(default)]
    passages: Vec<Passage>,
}

#[derive(Clone, Deserialize)]
struct Passage {
    #[serde(default)]
    page: String,
    #[serde(default)]
    snippet: String,
}

/// One completed Q&A in the conversation thread.
#[derive(Clone)]
struct Turn {
    id: usize,
    question: String,
    answer: String,
    refs: Vec<RefItem>,
}

/// Flattened `AnswerEvent` (serde tag = "kind", rename_all = "lowercase").
#[derive(Deserialize)]
struct AnswerEvent {
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    items: Option<Vec<RefItem>>,
    #[serde(default)]
    message: Option<String>,
}

/// Rotating tips shown during the one-time setup wait — get the user's Zotero
/// ready so they can ask a question the moment setup finishes.
const SETUP_TIPS: &[&str] = &[
    "While this installs — open Zotero (7+). PaperDock reads your library through it.",
    "Tip: in Zotero, download the PDFs for the papers you want to ask about.",
    "Tip: group papers into a Zotero collection to focus your questions.",
    "Answers cite their sources — click a citation to open that paper in Zotero.",
];

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    // ---- reactive state -------------------------------------------------
    let collections = RwSignal::new(Vec::<Collection>::new());
    let selected = RwSignal::new(String::new());
    let zotero_ok = RwSignal::new(true);
    let status = RwSignal::new(String::new());
    let question = RwSignal::new(String::new());
    let answer = RwSignal::new(String::new());
    let refs = RwSignal::new(Vec::<RefItem>::new());
    let active_source = RwSignal::new(0usize); // which Sources tab is open
    let streaming = RwSignal::new(false);
    // Conversation thread: `asked` is the current in-progress question; `history`
    // holds completed turns above it.
    let asked = RwSignal::new(String::new());
    let history = RwSignal::new(Vec::<Turn>::new());
    let turn_id = RwSignal::new(0usize);
    let notice = RwSignal::new(String::new()); // coverage heads-up (skipped PDFs)
    let has_key = RwSignal::new(true); // assume present until startup says otherwise
    let key_input = RwSignal::new(String::new());
    // Settings panel (model / embedding / base URL / key).
    let show_settings = RwSignal::new(false);
    let model_input = RwSignal::new(String::new());
    let embedding_input = RwSignal::new(String::new());
    let apibase_input = RwSignal::new(String::new());
    let qdrant_url_input = RwSignal::new(String::new());
    let qdrant_key_input = RwSignal::new(String::new());
    // First-run Python environment setup.
    let env_ready = RwSignal::new(true); // assume ready until startup says otherwise
    let setup_running = RwSignal::new(false);
    let setup_status = RwSignal::new(String::new());
    let setup_error = RwSignal::new(String::new());
    let tip_idx = RwSignal::new(0usize); // rotates the "get ready" tips
    let remembered = RwSignal::new(Option::<String>::None); // last collection to preselect
    let collections_ready = RwSignal::new(false); // collections have been fetched at least once
    let needs_config = RwSignal::new(false); // true when no key configured (fresh install)
    let export_key = RwSignal::new(true);    // "Include LLM key" checkbox
    let toast = RwSignal::new(String::new()); // transient confirmation

    // ---- single "answer" event listener, wired once at startup ----------
    {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
            // Tauri event object: { event, id, payload }.
            let payload = js_sys::Reflect::get(&ev, &JsValue::from_str("payload"))
                .unwrap_or(JsValue::NULL);
            let Ok(e) = serde_wasm_bindgen::from_value::<AnswerEvent>(payload) else {
                return;
            };
            match e.kind.as_str() {
                "status" => {
                    if let Some(t) = e.text {
                        status.set(t);
                    }
                }
                "token" => {
                    if let Some(t) = e.text {
                        answer.update(|a| a.push_str(&t));
                    }
                }
                "references" => {
                    if let Some(items) = e.items {
                        refs.set(items);
                        active_source.set(0);
                    }
                }
                "notice" => {
                    if let Some(t) = e.message {
                        notice.set(t);
                    }
                }
                "done" => {
                    streaming.set(false);
                    status.set("Indexed ✓".to_string());
                }
                "error" => {
                    streaming.set(false);
                    let msg = e.message.unwrap_or_else(|| "Something went wrong.".into());
                    status.set(msg);
                }
                _ => {}
            }
        });
        spawn_local(async move {
            // If the event permission is missing, listen() rejects — surface a
            // human message instead of silently never receiving answers.
            if listen("answer", cb.as_ref()).await.is_err() {
                status.set("Could not connect to the answer stream.".to_string());
            }
            cb.forget(); // app-lifetime listener
        });
    }

    // ---- first-run setup: "setup" events + env readiness check ----------
    {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
            let payload = js_sys::Reflect::get(&ev, &JsValue::from_str("payload"))
                .unwrap_or(JsValue::NULL);
            let Ok(e) = serde_wasm_bindgen::from_value::<AnswerEvent>(payload) else {
                return;
            };
            match e.kind.as_str() {
                "status" => {
                    if let Some(m) = e.message {
                        setup_status.set(m);
                    }
                }
                "done" => {
                    setup_running.set(false);
                    env_ready.set(true);
                }
                "error" => {
                    setup_running.set(false);
                    setup_error
                        .set(e.message.unwrap_or_else(|| "Setup failed. Try again.".into()));
                }
                _ => {}
            }
        });
        spawn_local(async move {
            let _ = listen("setup", cb.as_ref()).await;
            cb.forget();
        });
    }

    // ---- lab config import: "lab-imported" event (double-click or manual) --
    {
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
            let payload = js_sys::Reflect::get(&ev, &JsValue::from_str("payload"))
                .unwrap_or(JsValue::NULL);
            let name = payload.as_string().unwrap_or_default();
            needs_config.set(false);
            has_key.set(true);
            show_settings.set(false);
            toast.set(if name.is_empty() {
                "Lab config imported ✓".to_string()
            } else {
                format!("Lab config imported ✓ ({name})")
            });
        });
        spawn_local(async move {
            let _ = listen("lab-imported", cb.as_ref()).await;
            cb.forget();
        });
    }
    spawn_local(async move {
        // If the reading environment isn't provisioned yet, the setup screen
        // shows. Assume ready on any hiccup so we never block a working dev app.
        let ready = invoke("env_status", args(serde_json::json!({})))
            .await
            .ok()
            .and_then(|v| serde_wasm_bindgen::from_value::<bool>(v).ok())
            .unwrap_or(true);
        env_ready.set(ready);
    });
    // Re-check Zotero and, the moment it comes up, load collections. Safe to
    // call repeatedly: it only fetches collections while none are loaded yet,
    // so opening Zotero AFTER launch recovers without a restart.
    let refresh = move || {
        spawn_local(async move {
            let up = invoke("zotero_status", args(serde_json::json!({})))
                .await
                .ok()
                .and_then(|v| serde_wasm_bindgen::from_value::<bool>(v).ok())
                .unwrap_or(false);
            zotero_ok.set(up);
            if !up || !collections.get_untracked().is_empty() {
                return;
            }
            if let Ok(v) = invoke("list_collections", args(serde_json::json!({}))).await {
                if let Ok(list) = serde_wasm_bindgen::from_value::<Vec<Collection>>(v) {
                    let preselect = remembered
                        .get_untracked()
                        .filter(|id| list.iter().any(|c| &c.id() == id))
                        .or_else(|| list.first().map(|c| c.id()));
                    if let Some(id) = preselect {
                        selected.set(id);
                    }
                    collections.set(list);
                    collections_ready.set(true);
                }
            }
        });
    };

    // ---- startup: read config, then poll Zotero until it's up -----------
    spawn_local(async move {
        if let Ok(v) = invoke("get_config", args(serde_json::json!({}))).await {
            if let Ok(cfg) = serde_wasm_bindgen::from_value::<Config>(v) {
                remembered.set(cfg.last_collection);
                has_key.set(cfg.has_api_key);
                model_input.set(cfg.model);
                embedding_input.set(cfg.embedding);
                apibase_input.set(cfg.api_base);
                qdrant_url_input.set(cfg.qdrant_url);
                // First run with no key configured: show the "connect to your
                // lab" gate (import a .paperdock config, or set up manually).
                if !cfg.has_api_key {
                    needs_config.set(true);
                }
            }
        }
        refresh(); // first check now; the interval keeps retrying afterwards
    });

    // Heartbeat: rotate setup tips while provisioning, and keep re-checking
    // Zotero so "Waiting for Zotero…" clears on its own once the user opens it.
    leptos::leptos_dom::helpers::set_interval(
        move || {
            if !env_ready.get() {
                tip_idx.update(|i| *i = i.wrapping_add(1));
            }
            refresh();
        },
        std::time::Duration::from_secs(3),
    );

    // ---- handlers -------------------------------------------------------
    // Kick off first-run Python environment provisioning. Progress/errors
    // arrive as `setup` events (wired above).
    let start_setup = move || {
        setup_error.set(String::new());
        setup_running.set(true);
        setup_status.set("Preparing…".to_string());
        spawn_local(async move {
            let _ = invoke("setup_env", args(serde_json::json!({}))).await;
        });
    };

    // Pre-embed the selected collection into the shared index (no LLM query).
    // Idempotent — only missing papers get embedded. Uses the streaming flag so
    // it can't overlap an ask, and the Stop button can cancel it.
    let index = move || {
        let id = selected.get();
        if id.is_empty() || streaming.get() {
            return;
        }
        let (library, key) = id.split_once("::").unwrap_or(("users/0", id.as_str()));
        let (library, key) = (library.to_string(), key.to_string());
        answer.set(String::new());
        refs.set(Vec::new());
        streaming.set(true);
        status.set("Preparing shared index…".to_string());
        spawn_local(async move {
            let res = invoke(
                "index_collection",
                args(serde_json::json!({ "library": library, "collectionKey": key })),
            )
            .await;
            if let Err(e) = res {
                streaming.set(false);
                let msg = serde_wasm_bindgen::from_value::<String>(e)
                    .unwrap_or_else(|_| "Could not index this collection.".into());
                status.set(msg);
            }
        });
    };

    let on_collection_change = move |ev: web_sys::Event| {
        let k = event_target_value(&ev);
        selected.set(k.clone());
        // New collection = new conversation.
        history.set(Vec::new());
        asked.set(String::new());
        answer.set(String::new());
        refs.set(Vec::new());
        notice.set(String::new());
        spawn_local(async move {
            let _ = invoke("set_last_collection", args(serde_json::json!({ "key": k }))).await;
        });
        // Auto pre-embed the newly selected collection (background, idempotent).
        index();
    };

    let submit = move || {
        let q = question.get();
        let id = selected.get();
        if q.trim().is_empty() || id.is_empty() || streaming.get() {
            return;
        }
        // id is "<library>::<collectionKey>".
        let (library, key) = id.split_once("::").unwrap_or(("users/0", id.as_str()));
        let (library, key) = (library.to_string(), key.to_string());
        // Archive the previous completed answer into the thread.
        let prev = answer.get();
        if !prev.is_empty() {
            let n = turn_id.get();
            turn_id.set(n + 1);
            history.update(|h| {
                h.push(Turn {
                    id: n,
                    question: asked.get(),
                    answer: prev,
                    refs: refs.get(),
                })
            });
        }
        // Multi-turn context: last 2 completed turns, answers truncated. Kept
        // short on purpose — enough for follow-ups, bounded so it can't grow
        // unbounded (cost / context-length). Retrieval stays on the clean
        // question; the worker only feeds this to the answer LLM.
        let turns = history.get();
        let recent: Vec<_> = turns.iter().rev().take(2).collect();
        let mut hist_text = String::new();
        for t in recent.into_iter().rev() {
            let a: String = t.answer.chars().take(400).collect();
            let ell = if t.answer.chars().count() > 400 { "…" } else { "" };
            hist_text.push_str(&format!("Q: {}\nA: {}{}\n\n", t.question, a, ell));
        }

        asked.set(q.clone());
        question.set(String::new()); // clear input, ready for the next question
        answer.set(String::new());
        refs.set(Vec::new());
        active_source.set(0);
        notice.set(String::new());
        streaming.set(true);
        status.set("Indexing…".to_string());
        spawn_local(async move {
            let res = invoke(
                "ask",
                args(serde_json::json!({
                    "library": library, "collectionKey": key, "question": q,
                    "history": hist_text,
                })),
            )
            .await;
            if let Err(e) = res {
                streaming.set(false);
                let msg = serde_wasm_bindgen::from_value::<String>(e)
                    .unwrap_or_else(|_| "Could not start the query.".into());
                status.set(msg);
            }
        });
    };

    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" {
            ev.prevent_default();
            submit();
        }
    };

    let save_settings = move || {
        let (m, e, b, k) = (
            model_input.get(),
            embedding_input.get(),
            apibase_input.get(),
            key_input.get(),
        );
        let (qurl, qkey) = (qdrant_url_input.get(), qdrant_key_input.get());
        spawn_local(async move {
            let _ = invoke(
                "set_settings",
                args(serde_json::json!({
                    "model": m, "embedding": e, "apiBase": b,
                    "qdrantUrl": qurl, "qdrantKey": qkey,
                })),
            )
            .await;
            qdrant_key_input.set(String::new());
            // Save the key too if one was typed in the panel.
            if !k.trim().is_empty() {
                if invoke("set_api_key", args(serde_json::json!({ "key": k })))
                    .await
                    .is_ok()
                {
                    has_key.set(true);
                    key_input.set(String::new());
                }
            }
            show_settings.set(false);
            status.set("Settings saved ✓".to_string());
        });
    };

    let cancel = move || {
        spawn_local(async move {
            let _ = invoke("cancel", args(serde_json::json!({}))).await;
            streaming.set(false);
            status.set("Stopped.".to_string());
        });
    };

    // ---- view -----------------------------------------------------------
    view! {
        <main class="app">
            <header class="topbar">
                <div class="brand">
                    <svg class="logo" viewBox="0 0 24 24" fill="none"
                        stroke="currentColor" stroke-width="1.8"
                        stroke-linecap="round" stroke-linejoin="round">
                        // a paper sheet with a folded corner, "docked" on an
                        // accent underline — paper + dock.
                        <path d="M7 3h6l4 4v11H7z"/>
                        <path d="M13 3v4h4"/>
                        <line x1="10" y1="11" x2="14" y2="11"/>
                        <line x1="10" y1="14" x2="13" y2="14"/>
                        <line class="dock" x1="5" y1="21" x2="19" y2="21"/>
                    </svg>
                    <span class="wordmark">"PaperDock"</span>
                </div>
                <button
                    class="gear"
                    title="Settings — model, gateway, API key, shared vector DB"
                    on:click=move |_| show_settings.update(|s| *s = !*s)
                >
                    "⚙"
                </button>
            </header>

            {move || (!toast.get().is_empty()).then(|| view! {
                <div class="toast">{toast.get()}</div>
            })}

            {move || (!env_ready.get()).then(|| view! {
                <div class="setup-overlay">
                    <div class="setup-card">
                        <h2>"First-time setup"</h2>
                        <p class="setup-lead">
                            "PaperDock needs a one-time Python environment to read \
                             your papers — about 300 MB. It downloads once (needs \
                             internet) and is cached for good."
                        </p>
                        {move || {
                            let err = setup_error.get();
                            if !err.is_empty() {
                                view! {
                                    <p class="setup-err">{err}</p>
                                    <button class="ask" on:click=move |_| start_setup()>
                                        "Try again"
                                    </button>
                                }.into_any()
                            } else if setup_running.get() {
                                view! {
                                    <div class="setup-progress">
                                        <span class="spinner"></span>
                                        <span class="setup-line">{move || setup_status.get()}</span>
                                    </div>
                                    <div class="setup-tip">
                                        {move || SETUP_TIPS[tip_idx.get() % SETUP_TIPS.len()]}
                                    </div>
                                    {move || if zotero_ok.get() {
                                        view! {
                                            <div class="setup-check ok">"✓ Zotero connected"</div>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <div class="setup-check warn">
                                                "○ Open Zotero (7+) so PaperDock can read your library"
                                            </div>
                                        }.into_any()
                                    }}
                                    <p class="setup-hint">
                                        "This can take a minute. Keep the app open."
                                    </p>
                                }.into_any()
                            } else {
                                view! {
                                    <button class="ask" on:click=move |_| start_setup()>
                                        "Set up now"
                                    </button>
                                }.into_any()
                            }
                        }}
                    </div>
                </div>
            })}

            {move || (env_ready.get() && needs_config.get()).then(|| view! {
                <div class="setup-overlay">
                    <div class="setup-card">
                        <h2>"Connect to your lab"</h2>
                        <p class="setup-lead">
                            "Your lab admin sent you a "<b>".paperdock"</b>" config file. "
                            "Double-click it to set up PaperDock, or import it here."
                        </p>
                        <button class="ask" on:click=move |_| {
                            spawn_local(async move {
                                let _ = invoke("import_lab_config", args(serde_json::json!({}))).await;
                            });
                        }>"Import lab config…"</button>
                        <p class="setup-hint">
                            "No file yet? Ask your admin, or "
                            <a href="#" on:click=move |_| { needs_config.set(false); show_settings.set(true); }>
                                "set it up manually"
                            </a>"."
                        </p>
                    </div>
                </div>
            })}

            {move || show_settings.get().then(|| view! {
                <div class="settings">
                    <label class="slabel">"Chat model"</label>
                    <input class="sinput" prop:value=move || model_input.get()
                        placeholder="gpt-4o   or   ollama/llama3.1"
                        on:input=move |ev| model_input.set(event_target_value(&ev)) />
                    <label class="slabel">"Embedding model"</label>
                    <input class="sinput" prop:value=move || embedding_input.get()
                        placeholder="text-embedding-3-small   or   ollama/nomic-embed-text"
                        on:input=move |ev| embedding_input.set(event_target_value(&ev)) />
                    <label class="slabel">"Server / gateway URL (optional)"</label>
                    <input class="sinput" prop:value=move || apibase_input.get()
                        placeholder="https://api.ai.it.ufl.edu/v1  ·  or  http://homeai:11434"
                        on:input=move |ev| apibase_input.set(event_target_value(&ev)) />
                    <label class="slabel">"API key — NaviGator / OpenAI (blank for local)"</label>
                    <input class="sinput" type="password" prop:value=move || key_input.get()
                        placeholder="team key"
                        on:input=move |ev| key_input.set(event_target_value(&ev)) />
                    <label class="slabel">"Shared vector DB — Qdrant URL (optional)"</label>
                    <input class="sinput" prop:value=move || qdrant_url_input.get()
                        placeholder="https://xxxx.cloud.qdrant.io"
                        on:input=move |ev| qdrant_url_input.set(event_target_value(&ev)) />
                    <label class="slabel">"Qdrant API key (blank keeps saved)"</label>
                    <input class="sinput" type="password" prop:value=move || qdrant_key_input.get()
                        placeholder="•••"
                        on:input=move |ev| qdrant_key_input.set(event_target_value(&ev)) />
                    <button class="keysave" on:click=move |_| save_settings()>"Save"</button>

                    <div class="lab-export">
                        <label class="lab-export-row">
                            <input type="checkbox"
                                prop:checked=move || export_key.get()
                                on:change=move |ev| export_key.set(event_target_checked(&ev)) />
                            "Include LLM key (uncheck so members use their own)"
                        </label>
                        <button class="ask" on:click=move |_| {
                            let inc = export_key.get();
                            spawn_local(async move {
                                let _ = invoke("export_lab_config",
                                    args(serde_json::json!({ "includeKey": inc }))).await;
                            });
                        }>"Export lab config…"</button>
                        <p class="setup-hint">"This file contains your keys — share it privately."</p>
                    </div>
                </div>
            })}

            <div class="field">
                <span class="labelrow">
                    <span class="label" title="Pick a Zotero collection or a whole library to ask about">
                        "Collection"
                    </span>
                    <button
                        class="idxbtn"
                        title="Read all papers into the shared index now, so later questions answer instantly. Runs in the background."
                        prop:disabled=move || streaming.get()
                        on:click=move |_| index()
                    >
                        "⟳ Index"
                    </button>
                </span>
                <select
                    class="collection"
                    title="Your Zotero libraries and collections. Group libraries are shared with your team."
                    prop:value=move || selected.get()
                    on:change=on_collection_change
                >
                    <For
                        each=move || collections.get()
                        key=|c| c.id()
                        let:c
                    >
                        <option value=c.id()>
                            {format!("{} / {} ({})", c.library_name, c.name, c.num_items)}
                        </option>
                    </For>
                </select>
            </div>

            <div
                class="status"
                class:waiting=move || !zotero_ok.get()
            >
                {move || {
                    // Make the blocker unambiguous: you need Zotero running AND a
                    // collection before a question can run.
                    if !zotero_ok.get() {
                        "Waiting for Zotero — open Zotero (7+) so PaperDock can read your library.".to_string()
                    } else if !collections_ready.get() {
                        "Connected to Zotero — loading your collections…".to_string()
                    } else if collections.get().is_empty() {
                        "Connected, but no Zotero collections found. Put some papers in a collection in Zotero and it will appear here.".to_string()
                    } else if selected.get().is_empty() {
                        "Pick a collection above to get started.".to_string()
                    } else {
                        status.get()
                    }
                }}
            </div>

            <div class="askrow">
                <input
                    class="ask"
                    type="text"
                    placeholder="Ask your library."
                    autofocus
                    prop:value=move || question.get()
                    prop:disabled=move || streaming.get()
                    on:input=move |ev| question.set(event_target_value(&ev))
                    on:keydown=on_keydown
                />
                {move || streaming.get().then(|| view! {
                    <button class="stop" title="Stop" on:click=move |_| cancel()>"Stop"</button>
                })}
            </div>

            // Current question (shown above its streaming answer).
            {move || (!asked.get().is_empty())
                .then(|| view! { <div class="turn-q">{asked.get()}</div> })}

            // Coverage heads-up when some papers had no PDF.
            {move || (!notice.get().is_empty())
                .then(|| view! { <div class="notice">{notice.get()}</div> })}

            <div class="answer">
                // While waiting (no tokens yet) show a spinner next to the live
                // status so a slow first-run (embedding) never looks frozen.
                {move || {
                    (streaming.get() && answer.get().is_empty())
                        .then(|| view! { <span class="spinner"></span> })
                }}
                // Show the streamed answer. Before any token arrives, echo the
                // live status (Reading…/Thinking…/errors) so the big area isn't
                // blank — but at idle ("Indexed ✓") show a prompt instead of
                // duplicating the status line.
                {move || {
                    let a = answer.get();
                    if !a.is_empty() {
                        view! { <span>{a}</span> }.into_any()
                    } else if streaming.get() {
                        view! { <span class="answer-hint">{status.get()}</span> }.into_any()
                    } else {
                        let s = status.get();
                        let idle = s.is_empty() || s == "Indexed ✓";
                        let hint = if idle {
                            "Ask a question to get a cited answer from your papers.".to_string()
                        } else {
                            s
                        };
                        view! { <span class="answer-hint">{hint}</span> }.into_any()
                    }
                }}
                {move || streaming.get().then(|| view! { <span class="cursor"></span> })}
            </div>

            <div class="refs">
                // One tab per cited paper; the panel shows the active paper's
                // evidence. Keeps sources compact instead of a long scroll.
                {move || {
                    let items = refs.get();
                    if items.is_empty() {
                        return ().into_any();
                    }
                    view! {
                        <span class="refs-label">"Sources"</span>
                        <div class="tabs">
                            {items.iter().enumerate().map(|(i, r)| {
                                let cit = r.citation.clone();
                                view! {
                                    <button
                                        class="tab"
                                        title="Show the passages from this paper that support the answer"
                                        class:active=move || active_source.get() == i
                                        on:click=move |_| active_source.set(i)
                                    >
                                        {cit}
                                    </button>
                                }
                            }).collect_view()}
                        </div>
                        {move || {
                            let items = refs.get();
                            if items.is_empty() {
                                return ().into_any();
                            }
                            let i = active_source.get().min(items.len() - 1);
                            let r = items[i].clone();
                            let item_key = r.item_key.clone();
                            view! {
                                <div class="tabpanel">
                                    {r.passages.into_iter().map(|p| view! {
                                        <div class="passage">
                                            {(!p.page.is_empty()).then(|| view! {
                                                <span class="pageno">
                                                    {format!("p. {}", p.page)}
                                                </span>
                                            })}
                                            <span class="quote">{p.snippet}</span>
                                        </div>
                                    }).collect_view()}
                                    <button
                                        class="openz"
                                        on:click=move |_| {
                                            let item_key = item_key.clone();
                                            let library = selected
                                                .get()
                                                .split_once("::")
                                                .map(|(l, _)| l.to_string())
                                                .unwrap_or_else(|| "users/0".to_string());
                                            spawn_local(async move {
                                                let _ = invoke(
                                                    "open_in_zotero",
                                                    args(serde_json::json!({
                                                        "library": library, "itemKey": item_key,
                                                    })),
                                                ).await;
                                            });
                                        }
                                    >
                                        "Open in Zotero →"
                                    </button>
                                </div>
                            }.into_any()
                        }}
                    }.into_any()
                }}
            </div>

            // Conversation history — earlier turns, newest first, below the
            // current answer.
            <div class="thread">
                <For
                    each=move || { let mut v = history.get(); v.reverse(); v }
                    key=|t| t.id
                    let:t
                >
                    {
                        let refs_t = t.refs.clone();
                        view! {
                            <div class="turn">
                                <div class="turn-q">{t.question.clone()}</div>
                                <div class="turn-a">{t.answer.clone()}</div>
                                {(!refs_t.is_empty()).then(|| view! {
                                    <div class="turn-src">
                                        <span class="srclabel">"Sources"</span>
                                        {refs_t.into_iter().map(|r| {
                                            let item_key = r.item_key.clone();
                                            view! {
                                                <button
                                                    class="chip"
                                                    title="Open this paper in Zotero"
                                                    on:click=move |_| {
                                                        let item_key = item_key.clone();
                                                        let library = selected
                                                            .get()
                                                            .split_once("::")
                                                            .map(|(l, _)| l.to_string())
                                                            .unwrap_or_else(|| "users/0".to_string());
                                                        spawn_local(async move {
                                                            let _ = invoke(
                                                                "open_in_zotero",
                                                                args(serde_json::json!({
                                                                    "library": library,
                                                                    "itemKey": item_key,
                                                                })),
                                                            ).await;
                                                        });
                                                    }
                                                >
                                                    {r.citation.clone()}
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                })}
                            </div>
                        }
                    }
                </For>
            </div>

            <footer class="credit">
                {format!("PaperDock v{} · grounded by ", env!("CARGO_PKG_VERSION"))}
                <a href="#" on:click=open_link("https://github.com/Future-House/paper-qa")>
                    "PaperQA"
                </a>
                " · "
                <a href="#" on:click=open_link("https://github.com/chesterguan/PaperDock")>
                    "GitHub"
                </a>
            </footer>
        </main>
    }
}
