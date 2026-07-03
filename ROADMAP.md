# Roadmap

PaperDock is a **Zotero companion** for working around your own knowledge base —
ask, fact-check, and vet citations against the papers you already keep in Zotero.
It is deliberately *not* trying to replace mature paper-reading apps. This roadmap
is a direction, not a promise; priorities shift with use and feedback.

## Shipped

- **v0.1** — Ask mode: grounded, cited answers over a Zotero collection; streamed
  responses; multi-turn follow-ups; first-run Python setup via `uv`; universal
  (Intel + Apple Silicon) macOS build; shared vs. local embeddings (group
  libraries → shared Qdrant, personal → on-device).
- **v0.2** — Key-free, configurable distribution (`.paperdock` lab config, no
  baked keys); **Check citation** (does a paper support a claim?) with
  single-source targeting; **Verify reference** (CrossRef prescreen for
  fabricated citations).

## Near term

- **Signing + notarization** — remove the Gatekeeper "unidentified developer" /
  "damaged" friction so installs are double-click clean.
- **Windows + Linux builds** — a GitHub Actions matrix; platform-gate the paths
  that are currently macOS-only (`open`, venv layout, disk check, `uv` binary).
- **Citation-check depth** —
  - chain **Verify → fetch open-access PDF → Check** so a cited paper you don't
    have locally can still be support-checked;
  - **whole-draft** input: paste a manuscript, auto-extract claim→citation pairs,
    batch-check the bibliography.
- **Polish** — auto-update, true per-token streaming, in-app error toasts.

## Exploring (undecided)

- **A Zotero plugin.** The current standalone app may eventually live inside
  Zotero itself. Undecided — depends on what the plugin API allows and whether it
  serves the "companion" goal better than a separate app.
- Non-macOS-first distribution, richer evidence UI, notes/annotation awareness.

Have an idea? Open an issue — see [CONTRIBUTING.md](CONTRIBUTING.md).
