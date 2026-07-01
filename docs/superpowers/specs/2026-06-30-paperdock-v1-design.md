# PaperDock v1 — Design Spec

Date: 2026-06-30
Status: Approved (3 core decisions confirmed by user)

## Summary

Native macOS research companion: pick a Zotero collection, ask a question, get a
grounded answer with citations that open the source paper in Zotero. Tauri 2 +
Leptos front, Rust backend, PaperQA (Python) as the QA engine behind a thin
sidecar.

## Three load-bearing decisions

1. **Model config via PaperQA's built-in LiteLLM.** PaperQA already routes through
   LiteLLM. v1 stores ONE model string in local config (default + no picker UI),
   honoring "no model selection". Cloud vs. local is just the model string
   (`gpt-4o`, `claude-...`, or `ollama/llama3.2`). Pluggability is free — we build
   no provider UI.

2. **PaperQA bridge = thin Python sidecar over stdio JSON-lines.** PaperQA is
   Python with no Rust port. Tauri spawns a bundled Python worker; Rust and the
   worker exchange newline-delimited JSON over stdin/stdout. Chosen over the `pqa`
   CLI because streaming tokens and structured citations (the two hardest PRD
   features) come free from the Python API but require fragile scraping from the
   CLI. HTTP server and PyO3 rejected (port/lifecycle overhead; packaging hell).

3. **Zotero source = local HTTP API on :23119 (Zotero 7+).** Validated live
   (returns 200, `X-Zotero-Version` present). Consistent with the PRD's "Waiting
   for Zotero..." detection and the `zotero://select` open-in-Zotero feature, both
   of which already assume Zotero is running.

## Zotero data flow (verified against live library)

- Detect: `GET http://localhost:23119/api/` → reachable = running.
- Collections: `GET /api/users/0/collections` → `{key, data.name, meta.numItems}`.
- Papers: `GET /api/users/0/collections/<key>/items/top`.
- PDF for a paper: `GET /api/users/0/items/<key>/children` → attachment with
  `contentType == application/pdf`:
  - `imported_file` / `imported_url`: path = `<dataDir>/storage/<attachKey>/<filename>`
    (verified: these files exist on disk).
  - `linked_file`: path = `data.path` (absolute).
- Open in Zotero: `zotero://select/library/items/<itemKey>` via macOS `open`.
- `dataDir` defaults to `~/Zotero`, stored in config (PRD "remember cache path").

## Sidecar protocol (stdio, one JSON object per line)

Rust → worker (request):
```json
{"id":"q1","cmd":"ask","question":"...","index_name":"<collection-key>",
 "cache_dir":"<path>","model":"<litellm-model>",
 "docs":[{"path":"...","zotero_key":"ABC123","citation":"Rawlings 2009"}]}
```
Worker → Rust (stream of lines for that id):
```json
{"id":"q1","type":"status","text":"Indexing..."}
{"id":"q1","type":"token","text":"Three papers compare..."}
{"id":"q1","type":"references","items":[{"zotero_key":"ABC123","citation":"Rawlings 2009"}]}
{"id":"q1","type":"done"}
{"id":"q1","type":"error","message":"human-readable"}
```
Rust forwards `token`/`status`/`references`/`done`/`error` to the front via Tauri
events. The worker contract is identical for the mock and real PaperQA — only the
worker's internals change.

## v1 worker = MOCK (PaperQA deferred)

This machine has Python 3.9; PaperQA needs 3.11+ and isn't installed (user will
add PaperQA later). The shipped worker is pure-stdlib: it streams a placeholder
answer assembled from the REAL passed-in citations and echoes them as references.
This exercises the entire pipeline end-to-end. Swapping in real PaperQA is a
drop-in on the Python side (same protocol), zero Rust/UI changes.

## Components

- `src-tauri/` — Tauri 2 backend (Rust):
  - `zotero.rs` — reqwest client: status, collections, items, attachment paths.
  - `sidecar.rs` — spawn worker, write requests, read JSON lines, emit events.
  - `config.rs` — JSON config in app config dir: last_collection, window size,
    zotero_data_dir, cache_dir, model.
  - `lib.rs` — Tauri commands: `zotero_status`, `list_collections`, `ask`,
    `cancel`, `open_in_zotero`, `get_config`, `set_config`.
- `src/` — Leptos (wasm) front: one window — collection dropdown, status line,
  ask input, streaming answer, references list. Dark/minimal per PRD visual spec.
- `sidecar/paperdock_worker.py` — mock worker (→ real PaperQA later).

## Out of scope per PRD (v1)

No sqlite store — PaperQA owns the index cache (incremental/skip-unchanged is its
job); config is a small JSON file. No PDF viewer, notes, highlights, write-back,
plugins, cloud, model picker, multi-provider.

## Error handling

Human-readable only. "Zotero is not running." / "No PDFs found in this
collection." / "PaperQA index not available." Never surface stack traces;
worker `error` lines carry a clean message, internals go to a log file.

## Verification items (confirm during build)

- [x] Zotero 7 local API reachable by default (live: 200).
- [x] storage/<key>/<filename> path scheme (live: files exist).
- [ ] Tauri 2 sidecar bundling of the Python worker on macOS.
- [ ] Leptos + Trunk + Tauri 2 dev/build wiring.
