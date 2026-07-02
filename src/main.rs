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
    let streaming = RwSignal::new(false);
    let has_key = RwSignal::new(true); // assume present until startup says otherwise
    let key_input = RwSignal::new(String::new());
    // Settings panel (model / embedding / base URL / key).
    let show_settings = RwSignal::new(false);
    let model_input = RwSignal::new(String::new());
    let embedding_input = RwSignal::new(String::new());
    let apibase_input = RwSignal::new(String::new());
    let qdrant_url_input = RwSignal::new(String::new());
    let qdrant_key_input = RwSignal::new(String::new());

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

    // ---- startup: config -> status -> collections -----------------------
    spawn_local(async move {
        // Remembered collection.
        let mut remembered: Option<String> = None;
        if let Ok(v) = invoke("get_config", args(serde_json::json!({}))).await {
            if let Ok(cfg) = serde_wasm_bindgen::from_value::<Config>(v) {
                remembered = cfg.last_collection;
                has_key.set(cfg.has_api_key);
                model_input.set(cfg.model);
                embedding_input.set(cfg.embedding);
                apibase_input.set(cfg.api_base);
                qdrant_url_input.set(cfg.qdrant_url);
                // First run with no key configured: open Settings so the user
                // can add a key or point at a local (Ollama) model.
                if !cfg.has_api_key {
                    show_settings.set(true);
                }
            }
        }

        // Is Zotero up?
        let up = invoke("zotero_status", args(serde_json::json!({})))
            .await
            .ok()
            .and_then(|v| serde_wasm_bindgen::from_value::<bool>(v).ok())
            .unwrap_or(false);
        zotero_ok.set(up);

        if !up {
            return; // dropdown stays empty; status shows "Waiting for Zotero..."
        }

        // Collections.
        if let Ok(v) = invoke("list_collections", args(serde_json::json!({}))).await {
            if let Ok(list) = serde_wasm_bindgen::from_value::<Vec<Collection>>(v) {
                // Preselect remembered collection if still present, else first.
                let preselect = remembered
                    .filter(|id| list.iter().any(|c| &c.id() == id))
                    .or_else(|| list.first().map(|c| c.id()));
                if let Some(id) = preselect {
                    selected.set(id);
                }
                collections.set(list);
                status.set("Indexed ✓".to_string());
            }
        }
    });

    // ---- handlers -------------------------------------------------------
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
        answer.set(String::new());
        refs.set(Vec::new());
        streaming.set(true);
        status.set("Indexing…".to_string());
        spawn_local(async move {
            let res = invoke(
                "ask",
                args(serde_json::json!({
                    "library": library, "collectionKey": key, "question": q,
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
                <span class="brand">"PaperDock"</span>
                <button
                    class="gear"
                    title="Settings"
                    on:click=move |_| show_settings.update(|s| *s = !*s)
                >
                    "⚙"
                </button>
            </header>

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
                </div>
            })}

            <div class="field">
                <span class="labelrow">
                    <span class="label">"Collection"</span>
                    <button
                        class="idxbtn"
                        title="Pre-embed every paper in this collection into the shared index"
                        prop:disabled=move || streaming.get()
                        on:click=move |_| index()
                    >
                        "⟳ Index"
                    </button>
                </span>
                <select
                    class="collection"
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
                    if !zotero_ok.get() {
                        "Waiting for Zotero…".to_string()
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
                {move || {
                    (!refs.get().is_empty())
                        .then(|| view! { <span class="refs-label">"Sources"</span> })
                }}
                <For
                    each=move || refs.get()
                    key=|r| r.item_key.clone()
                    let:r
                >
                    {
                        let item_key = r.item_key.clone();
                        let passages = r.passages.clone();
                        view! {
                            <div class="source">
                                <button
                                    class="chip"
                                    on:click=move |_| {
                                        let item_key = item_key.clone();
                                        // Refs belong to the selected collection's
                                        // library; group items need the group path.
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
                                    {r.citation.clone()}
                                </button>
                                {passages.into_iter().map(|p| view! {
                                    <div class="passage">
                                        {(!p.page.is_empty()).then(|| view! {
                                            <span class="pageno">{format!("p. {}", p.page)}</span>
                                        })}
                                        <span class="quote">{format!("“{}”", p.snippet)}</span>
                                    </div>
                                }).collect_view()}
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
