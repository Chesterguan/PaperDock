# Contributing to PaperDock

Thanks for your interest! PaperDock is a small, focused **Zotero companion**, so
contributions that keep it simple and local-first are especially welcome.

## Ways to help

- **Report bugs / rough edges** — open an issue with your macOS version, what you
  did, and what happened (screenshots help).
- **Suggest features** — check the [roadmap](ROADMAP.md) first, then open an issue
  describing the use case. Small, composable additions over big rewrites.
- **Send a PR** — for anything non-trivial, open an issue first so we can agree on
  the approach before you spend time.

## Project layout

PaperDock is a thin native shell around
[PaperQA](https://github.com/Future-House/paper-qa):

- `src/` — Leptos (Rust → WASM) frontend (the UI).
- `src-tauri/` — Tauri (Rust) backend: config, Zotero client, sidecar bridge,
  the `ask` / `check` / `verify_reference` commands.
- `sidecar/paperdock_worker.py` — the Python worker that embeds PDFs, retrieves
  evidence, and calls the LLM (via PaperQA + LiteLLM).

## Dev setup

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk
npx @tauri-apps/cli dev          # runs the app with hot reload
```

The Python environment is created automatically on first run (via `uv`); for
local worker hacking, a venv lives under `sidecar/.venv`.

## Before you push

- `cargo check --manifest-path src-tauri/Cargo.toml` (backend) and
  `cargo check --target wasm32-unknown-unknown` (frontend) must pass.
- `cargo test --manifest-path src-tauri/Cargo.toml` — keep the config/merge tests
  green; add tests for new non-trivial logic.
- Keep it lazy: prefer the standard library / an existing dependency over new
  ones; the shortest change that works is usually the right one.

## Ground rules

- **Never commit secrets.** API keys live in the app's config dir and in
  `.paperdock` files, both gitignored — never in tracked files.
- Be kind and constructive. This is a research-support side project, not a
  support obligation.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
