# PaperDock v1.0 — Manuscript Citation Audit (design)

Date: 2026-07-04
Status: approved design → next: writing-plans

## One line

Import a structured manuscript → parse each **claim + the specific paper it
cites** → verify whether that cited paper actually supports the claim → produce
an auditable, correctable report. "A reviewer in a box."

## Why this exists (and what it replaces)

The differentiator is **Tier B: claim support** — "you cited Smith 2020, but
Smith 2020 does not support this sentence." Everyone else does "chat with
papers"; almost nobody verifies that a *specific* cited paper backs a *specific*
claim. That is exactly what PaperQA's retrieve+judge engine is uniquely good at.

The current v0.3 `check_draft` is a **demo, not production**. Its hard limits:
- `claims = claims[:8]` — a real manuscript has 50–150 claims; it sees the first 8.
- Claims are LLM-extracted with **no link to their citations**.
- It judges each claim against the **whole collection**, so it can never say
  "the paper you cited doesn't support this."

v1.0 is a **replacement, not an addition**. It deletes both demo modes and
collapses them into one real engine:

| Existing | Fate in v1.0 |
|---|---|
| `check_draft` (8-claim demo) | **Deleted** — replaced by manuscript audit. |
| `check` (single claim vs whole collection) | **Deleted/merged** — a single claim is a 1-item audit pinned to a chosen paper. |
| `ask` (Q&A / chat with papers) | Kept — separate product surface. |

Result: citation-check == **one** manuscript-audit engine. N claims; N=1 is the
manual single-claim case pinned to one paper. No parallel check modes.

## Business framing (informs scope, builds nothing)

Selling LLM tokens would kill the local-first thesis, so we don't. The realistic
paths that preserve local-first: (1) open-core sold to *institutions* on
deployment + support (they bring their own gateway), or (2) don't commercialize —
grant/prize-funded open research infrastructure (repo is already tied to an NIH
Replication Prize collection). Default to (2), keep (1) as an upgrade if adoption
proves out. **v1.0 ships zero commercial machinery** (YAGNI). Its only job is to
be undeniably good at this one thing.

## Pipeline

1. **Import** — user picks a `.docx` or `.tex` (+ `.bib`) file.

2. **Parse claims + citations** (the new hard part):
   - `.docx`: unzip → `word/document.xml`. Zotero's Word plugin writes citations
     as field codes `ADDIN ZOTERO_ITEM CSL_CITATION {…json…}` → parse the JSON to
     get the **exact Zotero item key(s)** and the sentence the field is attached
     to. The sentence carrying (or immediately preceding) the field = the claim.
   - `.tex`: match `\cite{key}` / `\citep` / `\citet` → bibkey → resolve in the
     `.bib` to DOI/title. Sentence containing the `\cite` = the claim.
   - Output: `[(claim_sentence, [cited_paper_ref])]`.

3. **Resolve paper → PDF** — Zotero item key → local PDF path (reuse
   `zotero.rs`). `.tex` side: DOI/title → match against the Zotero library → PDF.
   No PDF found → that citation runs **Tier A only** (see degradation).

4. **Index once, reuse** — embed each cited paper's PDF **once**, cached per
   paper keyed by Zotero dockey + embedding model, reused across all claims and
   across re-audits (sits on the existing personal→local-pickle / group→Qdrant
   split). 100 claims over the same papers do not re-embed.

5. **Verify per claim (Tier B)** — for each `(claim, cited paper)`: retrieve
   **only from that paper's chunks** (approach A — per-paper scoped index, see
   below) → verdict prompt → one of **SUPPORTED / PARTIALLY SUPPORTED / NOT
   SUPPORTED / INSUFFICIENT EVIDENCE**, plus the **supporting passage(s)** and a
   confidence. Reuses the existing `CHECK_QA` prompt and `verdict_of`.

   **Concurrency:** per-claim verifications are independent, so they run with
   **bounded concurrency** (`asyncio.gather` + semaphore). This is a pure
   wall-clock win — parallelism does not change any single verdict, so no
   quality-for-speed tradeoff. Bound is small and configurable (default ~4) to
   respect gateway rate limits; for local `ollama/*` (single model instance,
   effectively serial) drop to 1. Results stream into their claim rows as each
   finishes (out-of-order completion is fine).

6. **Tier A prescreen (cheap, on every citation)** — reuse v0.2's CrossRef
   reference-validity check (commit `507ff5d`): is the citation a real paper with
   correct metadata? Runs regardless of PDF availability; for no-PDF citations it
   is the only available signal.

7. **Report** — per row: claim · cited paper · Tier A (real? ✓/✗) · Tier B
   verdict + source passage + confidence · open-in-Zotero · human flag/override.
   Exportable.

## Retrieval scoping — approach A (per-paper mini-index)

paper-qa's `similarity_search` has **no metadata filter**, so "retrieve only from
the cited paper" cannot be a query-time filter. Chosen: **A — per-paper scoped
index.** Each cited paper's cached chunks are loaded into an ephemeral,
single-paper `Docs` for verification, so retrieval is *structurally* confined to
that paper. This is the only approach that guarantees "the verdict judged the
paper you actually cited" — which is the entire selling point. (Rejected B:
whole-collection top-k then filter by dockey — misses when the cited paper's
relevant chunk falls outside global top-k; recall is unstable.)

Embeddings are still cached per paper (step 4), so per-paper indexing does not
re-embed on re-audit.

## UX — transparent parse, one-by-one check, correctable

1. **Show the extracted claims first.** After parse (step 2), display the list of
   `(claim, cited paper)` pairs before checking. The parse step is fuzzy; making
   it visible turns the weakness into a trust feature.
2. **User can correct the parse:** remove a false claim, edit a claim's text, or
   **add a missed claim** (with its citation). Then run the audit on the reviewed
   list.
3. **Check one by one, streaming.** Results appear per claim as each completes —
   clear and explicit progress, not a single opaque wait. (Reuses the existing
   Tauri `status`/`done`/`error` event pipeline; adds per-claim result events.)
4. **Feedback per verdict.** User can flag a verdict as wrong or mark a claim as
   mis-extracted/missing. v1.0 feedback is **local**: it corrects *this* audit
   (drop/add/re-run + flag). Optional telemetry to the existing feedback
   collector is a later add, not v1.0.

## Trust surface

Every verdict shows the **actual source passage** it is based on, a confidence,
and a human override. No black-box verdicts. A verdict the user can't trace to a
passage doesn't count.

## Degradation (never blocks)

- Cited PDF in Zotero → full **Tier B**.
- No PDF → **Tier A only** (CrossRef "is the cite real / metadata correct") + a
  "no PDF — claim support not verified" badge.
- **No auto-fetch of OA PDFs in v1.0** (Unpaywall/OpenAlex deferred — YAGNI).

## Reuse vs. new

**Reuse:** verdict engine (`CHECK_QA`, `verdict_of`), embed/retrieve + persist
(pickle/Qdrant personal/group split), CrossRef Tier A (v0.2 `507ff5d`),
`zotero.rs` PDF resolution, Tauri event streaming.

**New:** `.docx` parser (Zotero field codes → item keys + claim sentences),
`.tex`+`.bib` parser (`\cite` → bibkey → DOI/title → Zotero match), per-paper
scoped verification (approach A), audit orchestration
(parse → review → index → per-claim verify → report), UI (import, claims
review/edit panel, per-claim streaming results, flag/add-missed, export).

**Delete:** `check_draft` (8-claim), `check` (whole-collection single claim).

## Risks (stated honestly)

- `.docx` citations only carry item keys if the author used **Zotero's Word
  plugin**; hand-typed citations have no field code → degrade (parser should say
  so, not silently miss them).
- `.tex` → Zotero matching (bibkey / DOI / title) is fuzzy; unmatched → Tier A only.
- 100+ claims is still 100+ LLM verdict calls even with a reused index — real
  time/cost. Inherent to full audit (chosen); surface progress + a running count,
  never a silent cap.

## Non-goals (v1.0)

Auto-fetching OA PDFs; plain-text-paste auditing (no reliable citation
resolution); Windows/Linux; commercial/licensing machinery; feedback telemetry
to a server.
