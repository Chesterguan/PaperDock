# PaperDock — Admin-Config (v0.2) Design

**Date:** 2026-07-02
**Status:** Approved design, pre-implementation
**Context:** PaperDock is becoming an open-source tool. v0.1 shipped with a lab's
keys **baked into the DMG** (fine for one trusted lab, wrong for OSS / multi-lab).
v0.2 removes baked keys: the app ships key-free, and each lab's **admin configures
once and distributes a small config file** that members open with one double-click.

## Goal

An admin sets up their lab's backend (LLM + shared vector store) **once** and hands
members a file. Members do **~nothing** — install the generic app once, double-click
the config file, done. No baked keys in any released binary.

## Non-goals (explicitly out of scope)

- v0.1 is not touched — it stays baked and already distributed.
- No auth server / per-user accounts / hosted key custody.
- We do **not** automate joining a Zotero group library — that is a Zotero-account
  action the member does in Zotero. The app auto-discovers group libraries the
  member already belongs to (existing `list_libraries`).
- No encryption of the config file (admin controls private distribution).

## The lab-config file

- Extension **`.paperdock`**, plain JSON, human-readable.
- Contains only the **shared** backend config:
  - `lab_name` (label, shown on import)
  - `model`, `embedding`, `api_base`
  - `qdrant_url`, `qdrant_key`
  - `api_key` — **optional** (included by admin's choice at export)
  - `default_collection` — optional (`"<library>::<key>"`, e.g. the shared group's
    All-items) so members land on the right collection if they've joined the group.
- **Excluded** (per-machine / per-user, never travels): `zotero_data_dir`,
  personal `last_collection`.
- Backend-agnostic: works for any LiteLLM backend (NaviGator, OpenAI, Ollama,
  self-hosted Qdrant) — not NaviGator-specific.
- The file may contain live keys → the export UI warns "share this privately."

## Admin export

- In the ⚙ Settings panel: an **"Export lab config…"** button and a checkbox
  **"Include LLM key"** (default **on**).
- Click → native save dialog → writes `<lab_name>.paperdock` (falls back to
  `paperdock-lab.paperdock` if no lab name set).
- With the checkbox off, `api_key` is omitted from the file — members import the
  shared config then add their own NaviGator key in Settings.

## Member import — two paths

1. **Double-click the `.paperdock` file.** macOS file association (registered via
   Tauri `bundle.fileAssociations`) opens the app with the file. The app applies
   it and shows **"Lab config imported ✓ (<lab_name>)"**. Works both when the app
   is already running and on cold start.
2. **First-run import screen.** When config is empty (no keys, nothing imported),
   the app shows: **"Import your lab config"** (file picker) + a smaller
   **"I'm the admin — set up manually"** (opens Settings). This replaces v0.1's
   auto-open-Settings-on-no-key behavior.

After import, config is merged and saved; the UI refreshes to the normal main
screen. Members still open Zotero and (if using a shared group) join that group in
Zotero — surfaced by the app's existing status messages and the README.

## What ships

- Remove `team_config.json` (real keys) from `tauri.conf.json` `bundle.resources`.
- Keep `team_config.example.json` committed as documentation of the shape.
- Generic release build contains **no keys**. First run → import screen.
- The dormant bundled-team-config seed path (`config.rs::bundled_team_config`) is
  retained but finds nothing in the generic build; harmless.

## Components & data flow

- **`config.rs`**
  - `struct LabConfig` — the shared-subset fields above.
  - `Config::to_lab_config(include_key) -> LabConfig` — build from current config.
  - `Config::apply_lab_config(&LabConfig)` — **merge** into the live config
    (overwrites the shared backend fields; always leaves `zotero_data_dir`
    intact; sets `last_collection` from `default_collection` **only if the member
    has none yet**, so a re-import never clobbers a member's own later choice),
    then `save`.
- **Tauri commands** (in `lib.rs`)
  - `export_lab_config(path: String, include_key: bool) -> Result<(), String>`
  - `import_lab_config(path: String) -> Result<LabConfigSummary, String>` —
    reads + parses + applies; returns `{ lab_name }` for the confirmation toast.
- **File-open handling** (`lib.rs::run`)
  - Handle `tauri::RunEvent::Opened { urls }`: for a `.paperdock` path, call the
    same apply logic, then `emit("lab-imported", { lab_name })`.
  - Cold-start (app launched by double-click) and warm (already running) both route
    through here.
- **Frontend (`main.rs`)**
  - Settings: "Export lab config…" button + "Include LLM key" checkbox → invoke
    `export_lab_config` (via a save-dialog path).
  - First-run import screen (new signals: `needs_config`, reuse setup-screen styling).
  - "Import lab config" button → open-dialog → `import_lab_config`.
  - Listen for `lab-imported` → refresh config state + toast.

## Error handling

- Unreadable / non-JSON / wrong-shape file → human message
  ("That doesn't look like a PaperDock lab config file."), config untouched.
- Missing required fields (e.g. no `qdrant_url` but `api_base` present) is allowed —
  partial configs merge what they have; the app's existing status messages guide the
  rest. Only a totally unparseable file is rejected.
- Export with no config set → button disabled or a "nothing to export yet" message.
- File dialogs are native (Tauri dialog plugin); a cancelled dialog is a no-op.

## Testing

- **Round-trip:** `to_lab_config(true)` → write → read → `apply_lab_config` yields
  the same shared fields; with `include_key=false`, `api_key` is absent.
- **Merge safety:** importing a lab config does not wipe `zotero_data_dir` or a
  personal `last_collection` (unless `default_collection` is set).
- **Bad input:** garbage / wrong-JSON file → rejected, config unchanged.
- **File association:** double-click a `.paperdock` on cold start and while running
  both import and show the toast (verified in the real app).

## Technical unknown to verify first

Tauri 2's macOS **file-open** delivery for a double-clicked associated file,
especially **cold start** (`RunEvent::Opened` vs. deep-link plugin vs. launch
args). The plan's first step spikes this in a throwaway build before the rest.

## Documentation deliverable (required)

Add a dedicated **README section for v0.2+ users: "Setting up your keys"** — written
plainly so a non-technical PI can follow it. It must cover:

- **Admin (set up a lab once):** open Settings, fill model / embedding / API base /
  LLM key / Qdrant URL + key (with a worked NaviGator example and a self-hosted /
  OpenAI / Ollama example, since it's OSS and backend-agnostic). Then
  **Export lab config…**, with/without the LLM key, and share the `.paperdock` file
  **privately** (it contains keys).
- **Member (join a lab):** install the app, **double-click the `.paperdock` file**
  (or use the first-run Import screen). If the lab uses personal LLM keys, get your
  own NaviGator key and paste it in Settings.
- **Where to get keys:** how to obtain a NaviGator key (UF), and the equivalent for
  a generic OSS user (OpenAI key / local Ollama needs none / a Qdrant Cloud free
  cluster).
- A clear note that the `.paperdock` file and any bundled keys are **secrets**.

This README section supersedes v0.1's "keys are baked, do nothing" assumption.

## Rollout

- v0.2 generic DMG (key-free) → the new import flow.
- Chester's lab: export current config once → distribute the `.paperdock` file +
  the generic DMG. No rebuild-per-lab, no toolchain needed by any admin.
