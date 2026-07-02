# PaperDock Admin-Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship PaperDock key-free; an admin exports a `.paperdock` lab-config file that members open with one double-click to configure the app.

**Architecture:** A new `LabConfig` (shared-subset) struct in `config.rs` with pure `to_lab_config`/`apply_lab_config` methods (unit-tested). Two Tauri commands open native dialogs (via `tauri-plugin-dialog`, invoked from Rust) to write/read the file. macOS file-association double-click is caught in the `run()` loop via `RunEvent::Opened`. The generic build bundles no keys.

**Tech Stack:** Rust, Tauri 2, Leptos 0.8 (CSR), `tauri-plugin-dialog`, serde_json.

## Global Constraints

- Config field names are exact: `last_collection`, `zotero_data_dir`, `model`, `embedding`, `api_base`, `qdrant_url`, `qdrant_api_key`, `api_key`. `model`/`embedding` are `String`; the rest of the optionals are `Option<String>`.
- `.paperdock` file is plain JSON. Fields: `lab_name`, `model`, `embedding`, `api_base`, `qdrant_url`, `qdrant_api_key`, `api_key` (all optional), `default_collection` (optional, `"<library>::<key>"`).
- Merge rule: `apply_lab_config` overwrites a shared field **only when the lab file provides it (Some)**; always preserves `zotero_data_dir`; sets `last_collection` from `default_collection` **only if `last_collection` is currently `None`**.
- Never bake real keys into a released binary. Keep `team_config.example.json` as docs.
- Keys/`.paperdock` files are secrets — copy warnings verbatim into UI/README.
- v0.1 is frozen; do not modify its behavior beyond what these tasks specify.
- Dev interpreter still resolves to `sidecar/.venv` so `env_status` is true in dev (setup screen won't show).

---

### Task 1: Spike — file association + `RunEvent::Opened` wiring

Prove the one technical unknown before building on it: a double-clicked `.paperdock` file delivers its path to the backend, on both cold start and while running.

**Files:**
- Modify: `src-tauri/tauri.conf.json` (bundle `fileAssociations`)
- Modify: `src-tauri/src/lib.rs` (restructure `run()` end to catch `RunEvent::Opened`)

**Interfaces:**
- Produces: an app that logs `paperdock file opened: <path>` to stderr when a `.paperdock` file is double-clicked. Later tasks replace the log with real handling at the same call site.

- [ ] **Step 1: Register the file association**

In `src-tauri/tauri.conf.json`, add to the `"bundle"` object (sibling of `resources`):

```json
    "fileAssociations": [
      {
        "ext": ["paperdock"],
        "name": "PaperDock Lab Config",
        "role": "Editor"
      }
    ],
```

- [ ] **Step 2: Restructure `run()` to process `RunEvent::Opened`**

In `src-tauri/src/lib.rs`, replace the terminal `.run(tauri::generate_context!()).expect("error while running PaperDock");` with:

```rust
        .build(tauri::generate_context!())
        .expect("error while building PaperDock")
        .run(|app, event| {
            if let tauri::RunEvent::Opened { urls } = event {
                for url in urls {
                    // macOS delivers file:// URLs for associated files.
                    if let Ok(path) = url.to_file_path() {
                        if path.extension().and_then(|e| e.to_str()) == Some("paperdock") {
                            // Task 3 replaces this with real import + event emit.
                            let _ = app;
                            eprintln!("paperdock file opened: {}", path.display());
                        }
                    }
                }
            }
        });
```

- [ ] **Step 3: Build the app**

Run: `cd /Users/ziyuanguan/IC3/PaperDock && npx @tauri-apps/cli build --bundles app 2>&1 | tail -5`
Expected: `Finished` / `Bundling PaperDock.app`, no errors.

- [ ] **Step 4: Verify the double-click delivers the path (cold start)**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
APP="src-tauri/target/release/bundle/macos/PaperDock.app"
echo '{"lab_name":"spike"}' > /tmp/spike.paperdock
# Launch the app by opening the file, capture stderr.
"$APP/Contents/MacOS/PaperDock" /tmp/spike.paperdock 2>/tmp/pd_stderr.txt &
sleep 4; pkill -f "PaperDock.app/Contents/MacOS/PaperDock"
grep "paperdock file opened" /tmp/pd_stderr.txt
```
Expected: a line `paperdock file opened: /tmp/spike.paperdock` (or the macOS `open`-delivered path). If the direct-arg form does not trigger `Opened`, test via Finder association: `open -a "$APP" /tmp/spike.paperdock` after copying the app to `/Applications`. Document which path works — Task 3 depends on it.

- [ ] **Step 5: Commit**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
git add src-tauri/tauri.conf.json src-tauri/src/lib.rs
git commit -m "feat: register .paperdock file association + RunEvent::Opened hook"
```

---

### Task 2: `LabConfig` struct + pure convert/merge methods (unit-tested)

**Files:**
- Modify: `src-tauri/src/config.rs` (add struct + two methods + `#[cfg(test)]` module)

**Interfaces:**
- Produces:
  - `pub struct LabConfig` with `pub` fields: `lab_name: Option<String>`, `model: Option<String>`, `embedding: Option<String>`, `api_base: Option<String>`, `qdrant_url: Option<String>`, `qdrant_api_key: Option<String>`, `api_key: Option<String>`, `default_collection: Option<String>`. Derives `Clone, serde::Serialize, serde::Deserialize`, each field `#[serde(default)]`.
  - `pub fn Config::to_lab_config(&self, include_key: bool) -> LabConfig`
  - `pub fn Config::apply_lab_config(&mut self, lab: &LabConfig)`
  - `pub fn LabConfig::summary_name(&self) -> String` (returns `lab_name` or `"your lab"`).

- [ ] **Step 1: Write the failing tests**

Add to `src-tauri/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            last_collection: Some("users/0::PERSONAL".into()),
            zotero_data_dir: "/Users/me/Zotero".into(),
            model: "openai/gpt-oss-120b".into(),
            embedding: "openai/nomic".into(),
            api_base: Some("https://api.ai.it.ufl.edu/v1".into()),
            qdrant_url: Some("https://q.example".into()),
            qdrant_api_key: Some("QKEY".into()),
            api_key: Some("LLMKEY".into()),
        }
    }

    #[test]
    fn to_lab_config_includes_key_when_asked() {
        let lab = sample().to_lab_config(true);
        assert_eq!(lab.api_key.as_deref(), Some("LLMKEY"));
        assert_eq!(lab.qdrant_api_key.as_deref(), Some("QKEY"));
        assert_eq!(lab.model.as_deref(), Some("openai/gpt-oss-120b"));
        // Per-machine fields never travel.
        assert!(lab.default_collection.is_none());
    }

    #[test]
    fn to_lab_config_omits_key_when_not_asked() {
        let lab = sample().to_lab_config(false);
        assert!(lab.api_key.is_none());
        assert_eq!(lab.qdrant_api_key.as_deref(), Some("QKEY")); // shared, still travels
    }

    #[test]
    fn apply_overwrites_shared_preserves_local() {
        let mut cfg = Config::default(); // zotero_data_dir = default, last_collection None
        let dir_before = cfg.zotero_data_dir.clone();
        let lab = LabConfig {
            lab_name: Some("Smith Lab".into()),
            model: Some("openai/gpt-oss-120b".into()),
            embedding: Some("openai/nomic".into()),
            api_base: Some("https://api.ai.it.ufl.edu/v1".into()),
            qdrant_url: Some("https://q.example".into()),
            qdrant_api_key: Some("QKEY".into()),
            api_key: Some("LLMKEY".into()),
            default_collection: Some("groups/6597011::__all__".into()),
        };
        cfg.apply_lab_config(&lab);
        assert_eq!(cfg.model, "openai/gpt-oss-120b");
        assert_eq!(cfg.qdrant_api_key.as_deref(), Some("QKEY"));
        assert_eq!(cfg.zotero_data_dir, dir_before); // preserved
        assert_eq!(cfg.last_collection.as_deref(), Some("groups/6597011::__all__")); // set: was None
    }

    #[test]
    fn apply_does_not_clobber_existing_collection() {
        let mut cfg = sample(); // last_collection = users/0::PERSONAL
        let lab = LabConfig {
            default_collection: Some("groups/6597011::__all__".into()),
            ..LabConfig::default()
        };
        cfg.apply_lab_config(&lab);
        assert_eq!(cfg.last_collection.as_deref(), Some("users/0::PERSONAL")); // kept
    }
}
```

Note: `LabConfig` needs `#[derive(Default)]` for the `..LabConfig::default()` in the last test.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /Users/ziyuanguan/IC3/PaperDock && cargo test --manifest-path src-tauri/Cargo.toml config:: 2>&1 | tail -15`
Expected: compile error `cannot find type LabConfig` / `no method to_lab_config`.

- [ ] **Step 3: Implement `LabConfig` + methods**

Add to `src-tauri/src/config.rs` (after the `Config` `impl` block):

```rust
/// The SHARED subset of config an admin distributes to members as a
/// `.paperdock` file. Per-machine fields (zotero_data_dir, personal
/// last_collection) never travel. `api_key` is optional so a lab can require
/// members to use their own.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LabConfig {
    #[serde(default)]
    pub lab_name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub embedding: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub qdrant_url: Option<String>,
    #[serde(default)]
    pub qdrant_api_key: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub default_collection: Option<String>,
}

impl LabConfig {
    pub fn summary_name(&self) -> String {
        self.lab_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "your lab".to_string())
    }
}

impl Config {
    /// Build a shareable LabConfig from the current config. `include_key`
    /// controls whether the LLM api_key travels (labs using personal keys omit).
    pub fn to_lab_config(&self, include_key: bool) -> LabConfig {
        LabConfig {
            lab_name: None,
            model: Some(self.model.clone()),
            embedding: Some(self.embedding.clone()),
            api_base: self.api_base.clone(),
            qdrant_url: self.qdrant_url.clone(),
            qdrant_api_key: self.qdrant_api_key.clone(),
            api_key: if include_key { self.api_key.clone() } else { None },
            default_collection: None,
        }
    }

    /// Merge a lab config into this config: overwrite each shared field only
    /// when the lab file provides it; preserve zotero_data_dir always; set
    /// last_collection from default_collection only if none is set yet.
    pub fn apply_lab_config(&mut self, lab: &LabConfig) {
        if let Some(m) = &lab.model {
            self.model = m.clone();
        }
        if let Some(e) = &lab.embedding {
            self.embedding = e.clone();
        }
        if lab.api_base.is_some() {
            self.api_base = lab.api_base.clone();
        }
        if lab.qdrant_url.is_some() {
            self.qdrant_url = lab.qdrant_url.clone();
        }
        if lab.qdrant_api_key.is_some() {
            self.qdrant_api_key = lab.qdrant_api_key.clone();
        }
        if lab.api_key.is_some() {
            self.api_key = lab.api_key.clone();
        }
        if self.last_collection.is_none() {
            if let Some(dc) = &lab.default_collection {
                self.last_collection = Some(dc.clone());
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd /Users/ziyuanguan/IC3/PaperDock && cargo test --manifest-path src-tauri/Cargo.toml config:: 2>&1 | tail -15`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
git add src-tauri/src/config.rs
git commit -m "feat: LabConfig struct + to/apply_lab_config with unit tests"
```

---

### Task 3: Export/import commands + wire double-click to real import

**Files:**
- Modify: `src-tauri/Cargo.toml` (add `tauri-plugin-dialog`)
- Modify: `src-tauri/capabilities/default.json` (add `dialog:default`)
- Modify: `src-tauri/src/lib.rs` (plugin init, two commands, RunEvent handler, register commands)

**Interfaces:**
- Consumes: `Config::to_lab_config`, `Config::apply_lab_config`, `LabConfig`, `LabConfig::summary_name` (Task 2); the `RunEvent::Opened` hook (Task 1); `AppState { config: Mutex<Config> }` (existing).
- Produces:
  - `#[tauri::command] async fn export_lab_config(app, state, include_key: bool) -> Result<String, String>` — opens a save dialog, writes JSON, returns the written path (or `""` if cancelled).
  - `#[tauri::command] async fn import_lab_config(app, state) -> Result<String, String>` — opens an open dialog, parses+applies+saves, returns `lab_name` summary (or `""` if cancelled).
  - Emits `app.emit("lab-imported", <summary_name String>)` after any successful import (dialog or double-click).

- [ ] **Step 1: Add the dialog plugin dependency**

In `src-tauri/Cargo.toml`, under `[dependencies]`, add:

```toml
tauri-plugin-dialog = "2"
```

Run: `cd /Users/ziyuanguan/IC3/PaperDock && cargo fetch --manifest-path src-tauri/Cargo.toml 2>&1 | tail -3`
Expected: fetches `tauri-plugin-dialog`, no error.

- [ ] **Step 2: Grant the dialog capability**

In `src-tauri/capabilities/default.json`, change the `permissions` array to:

```json
  "permissions": [
    "core:default",
    "dialog:default"
  ]
```

- [ ] **Step 3: Initialize the plugin and add the shared import helper**

In `src-tauri/src/lib.rs`, add the plugin to the builder (in `run()`, right after `tauri::Builder::default()`):

```rust
        .plugin(tauri_plugin_dialog::init())
```

Add this helper function (near the other command fns), used by both the command and the double-click handler:

```rust
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
```

- [ ] **Step 4: Add the two commands**

Add to `src-tauri/src/lib.rs`:

```rust
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
```

- [ ] **Step 5: Replace the spike log with a real import in the `Opened` handler**

In `run()`'s `RunEvent::Opened` arm (from Task 1), replace the `eprintln!` line with:

```rust
                            let _ = apply_lab_file(app, &path);
```

- [ ] **Step 6: Register both commands**

In the `tauri::generate_handler![...]` list in `run()`, add `export_lab_config,` and `import_lab_config,`.

- [ ] **Step 7: Build to verify it compiles**

Run: `cd /Users/ziyuanguan/IC3/PaperDock && cargo build --manifest-path src-tauri/Cargo.toml 2>&1 | tail -8`
Expected: `Finished`, no errors.

- [ ] **Step 8: Commit**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
git add src-tauri/Cargo.toml src-tauri/capabilities/default.json src-tauri/src/lib.rs
git commit -m "feat: export/import_lab_config commands + double-click import"
```

---

### Task 4: Frontend — export button, first-run import screen, lab-imported toast

**Files:**
- Modify: `src/main.rs` (Settings export UI; first-run import screen; `lab-imported` listener; adjust the no-key startup branch)
- Modify: `styles.css` (reuse `.setup-*`; add `.toast` if needed)

**Interfaces:**
- Consumes: commands `export_lab_config(include_key: bool) -> String`, `import_lab_config() -> String`; event `"lab-imported"` (payload = lab name String); existing `invoke`, `args`, `listen`, `spawn_local`, `has_key`, `show_settings` signals.
- Produces: user-facing export/import controls and a first-run "no config" gate.

- [ ] **Step 1: Add signals + `lab-imported` listener**

In `App()` near the other signals, add:

```rust
    let needs_config = RwSignal::new(false); // true when no key configured (fresh install)
    let export_key = RwSignal::new(true);    // "Include LLM key" checkbox
    let toast = RwSignal::new(String::new()); // transient confirmation
```

After the `setup` listener block, add a `lab-imported` listener (mirrors the existing pattern):

```rust
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
```

- [ ] **Step 2: Gate first-run on missing config instead of auto-opening Settings**

In the startup `spawn_local` config read, replace the existing:

```rust
                if !cfg.has_api_key {
                    show_settings.set(true);
                }
```

with:

```rust
                if !cfg.has_api_key {
                    needs_config.set(true);
                }
```

- [ ] **Step 3: Add the first-run import screen**

In the `view!`, right after the setup-overlay block (so setup takes priority over config), add:

```rust
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
```

- [ ] **Step 4: Add export controls to the Settings panel**

Inside the `show_settings` block in `view!` (near the existing Settings fields/buttons), add:

```rust
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
```

Note: `event_target_checked` is in `leptos::prelude`; confirm it's imported (add to the `use leptos::...` if missing).

- [ ] **Step 5: Render the toast**

Near the top of the main view (after `<header>`), add:

```rust
            {move || (!toast.get().is_empty()).then(|| view! {
                <div class="toast">{toast.get()}</div>
            })}
```

- [ ] **Step 6: Add minimal CSS**

Append to `styles.css`:

```css
.toast {
  margin: 8px 0;
  padding: 8px 12px;
  font-size: 12.5px;
  color: var(--accent);
  background: var(--accent-dim);
  border-radius: 6px;
}
.lab-export { margin-top: 14px; padding-top: 12px; border-top: 1px solid var(--border); }
.lab-export-row { display: flex; gap: 8px; align-items: center; font-size: 12.5px; color: var(--text-dim); margin-bottom: 10px; }
```

- [ ] **Step 7: Build the frontend**

Run: `cd /Users/ziyuanguan/IC3/PaperDock && cargo check --target wasm32-unknown-unknown 2>&1 | tail -6`
Expected: `Finished`, no errors. (If `event_target_checked` is unresolved, add it to the leptos import and re-run.)

- [ ] **Step 8: Commit**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
git add src/main.rs styles.css
git commit -m "feat: lab-config export UI + first-run import screen + toast"
```

---

### Task 5: Ship key-free — drop baked keys from the bundle

**Files:**
- Modify: `src-tauri/tauri.conf.json` (remove `team_config.json` from `resources`)
- Modify: `.gitignore` (no change needed — `team_config.json` already ignored; verify)

**Interfaces:**
- Consumes: the first-run import screen (Task 4) — with no bundled keys, a fresh install has `has_api_key = false` → the import screen shows.

- [ ] **Step 1: Remove the baked team config from bundle resources**

In `src-tauri/tauri.conf.json`, delete the `"team_config.json": "team_config.json",` line from the `resources` map. The map keeps `bin/uv`, `paperdock_worker.py`, `requirements.lock`.

- [ ] **Step 2: Build and confirm no keys ship**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
npx @tauri-apps/cli build --bundles app 2>&1 | tail -3
RES="src-tauri/target/release/bundle/macos/PaperDock.app/Contents/Resources"
test ! -f "$RES/team_config.json" && echo "OK: no team_config.json bundled" || echo "FAIL: keys still bundled"
ls "$RES" | grep -E "uv|worker|requirements" && echo "OK: other resources intact"
```
Expected: `OK: no team_config.json bundled` and the other three resources present.

- [ ] **Step 3: Commit**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
git add src-tauri/tauri.conf.json
git commit -m "feat: ship key-free — remove baked team_config from bundle"
```

---

### Task 6: README "Setting up your keys (v0.2+)" section

**Files:**
- Modify: `README.md` (add a new section before Troubleshooting)

**Interfaces:**
- Consumes: nothing (docs). Describes Tasks 3–5 behavior.

- [ ] **Step 1: Add the section**

Insert into `README.md` immediately before the `## Troubleshooting` heading:

```markdown
## Setting up your keys (v0.2+)

PaperDock ships without any keys. Your lab's **admin** configures the backend
once and shares a small `.paperdock` file; everyone else just opens it.

### Admin — set up your lab once
1. Open PaperDock → **⚙ Settings** and fill in:
   - **Model / Embedding / API base / LLM key** for your provider. Examples:
     - **UF NaviGator:** base `https://api.ai.it.ufl.edu/v1`, model
       `openai/gpt-oss-120b`, embedding `openai/nomic-embed-text-v1.5`, plus your
       NaviGator key.
     - **OpenAI:** base blank, model `gpt-4o`, embedding
       `text-embedding-3-small`, your OpenAI key.
     - **Local Ollama:** base `http://localhost:11434`, model
       `ollama/llama3.1`, embedding `ollama/nomic-embed-text`, no key needed.
   - **Qdrant URL + key** (optional) for a shared team vector store — a free
     [Qdrant Cloud](https://qdrant.tech/) cluster works. Leave blank to keep
     every member's embeddings local.
2. Click **Export lab config…**. Leave **"Include LLM key"** checked to give
   members a zero-setup file; uncheck it if members use their own keys.
3. Share the resulting `.paperdock` file **privately** (email/Slack/OneDrive to
   named people) — it contains your keys.

### Member — join a lab
1. Install PaperDock (see Install above).
2. **Double-click the `.paperdock` file** your admin sent — the app configures
   itself and shows "Lab config imported ✓". (Or use **Import lab config…** on
   the first-run screen / in Settings.)
3. If your lab uses personal keys, get your own key from your provider (e.g.
   apply at NaviGator) and paste it in **⚙ Settings**.

### Using a shared team library
The shared vector store is scoped to a **Zotero group library**. To share
embeddings with your lab, join that group in Zotero (Zotero → your account →
Groups). Personal-library papers always embed **locally** on your own machine.

> **Security:** the `.paperdock` file and any API keys are secrets. Anyone with
> them can use your LLM quota and read/modify your shared vector store. Share the
> file only with people you trust; don't post it publicly or commit it to git.
```

- [ ] **Step 2: Commit**

```bash
cd /Users/ziyuanguan/IC3/PaperDock
git add README.md
git commit -m "docs: add v0.2+ key setup guide (admin export / member import)"
```

---

## Final verification (after all tasks)

- [ ] `cargo test --manifest-path src-tauri/Cargo.toml config::` → 4 passed.
- [ ] `cargo check --target wasm32-unknown-unknown` → clean.
- [ ] Build `app`, delete `~/Library/Application Support/com.paperdock.app/config.json`, launch → first-run **import screen** appears (not the old auto-Settings).
- [ ] Real-app round trip: in Settings **Export lab config…** → get a `.paperdock`; wipe config; **double-click** that file → "Lab config imported ✓", main screen loads with the model/keys restored.

## Notes for the implementer

- The dialog `blocking_save_file`/`blocking_pick_file` calls run inside async commands (worker threads), which is allowed; do not call them on the main thread.
- If Task 1's spike shows cold-start `Opened` doesn't fire for direct-arg launch, the Finder association (`open -a`) is the real user path and is what matters; note the finding and keep the in-app **Import** button as the guaranteed fallback.
- Do not add a JS dialog API — dialogs are opened from Rust, so `withGlobalTauri` needs no dialog exposure.
- Keep `config.rs::bundled_team_config` and its self-heal — harmless with no bundled file; removing it is out of scope.
```
