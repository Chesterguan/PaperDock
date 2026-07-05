# Manuscript Citation Audit — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the v0.3 `check_draft` demo with a production manuscript-audit engine: import a `.docx`/`.tex` → parse every `(claim, cited paper)` → verify whether the cited paper supports the claim → correctable, auditable report.

**Architecture:** Reuse the existing embed/retrieve/verdict/cache engine unchanged. Add (1) manuscript parsers in the Python worker (`parse` cmd, stdlib only), (2) a whole-manuscript `audit` cmd that indexes each cited paper **once** and verdicts all its claims with bounded concurrency, streaming one event per claim, (3) a Rust `parse_manuscript` + `run_audit` command pair, (4) a Leptos review→results UI. Retrieval is scoped to the cited paper by building a **single-paper `Docs`** per cited paper (approach A). Delete `check_draft`.

**Tech Stack:** Python 3.12 worker (paper-qa, stdlib `zipfile`/`xml.etree`/`re`), Rust/Tauri 2, Leptos 0.8 CSR.

## Global Constraints

- No new Python deps — `.docx`/`.bib`/`.tex` parsing uses stdlib only (`zipfile`, `xml.etree.ElementTree`, `re`, `json`).
- Verdict labels verbatim: `SUPPORTED`, `PARTIALLY SUPPORTED`, `NOT SUPPORTED`, `INSUFFICIENT EVIDENCE`.
- Reuse `CHECK_QA` prompt and `verdict_of()` — do not fork them.
- Bounded concurrency default 4; `ollama/*` models → 1.
- Tier A (CrossRef) stays the existing Rust `verify_reference`; never blocks; runs once per unique cited paper.
- No auto-fetch of OA PDFs (v1.0 non-goal).
- Never leak a stack trace to the UI — human message on every error path.
- Personal libraries stay local (pickle); only `groups_*` scopes touch Qdrant (existing `use_qdrant` rule — do not change).

---

### Task 1: Worker `.docx` parser → claims + Zotero keys

**Files:**
- Modify: `sidecar/paperdock_worker.py` (add `parse_docx`, wire `cmd:"parse"`)
- Test: `sidecar/test_parse.py`

**Interfaces:**
- Produces: `parse_docx(path: str) -> list[dict]` where each dict = `{"claim": str, "keys": [str], "cites_raw": [str]}`. `keys` = Zotero item keys extracted from `ADDIN ZOTERO_ITEM CSL_CITATION` field codes; `cites_raw` = human citation strings (fallback for Tier A when no key).

**Background:** Zotero's Word plugin stores each citation as a field. In `word/document.xml` the field text lives in `<w:instrText>` runs and reads `ADDIN ZOTERO_ITEM CSL_CITATION {…json…}`. The JSON has `citationItems[].itemData` and, crucially, `citationItems[].uris` like `http://zotero.org/users/0/items/ABCD1234` — the trailing segment is the Zotero item key. The claim = the paragraph text the field sits in.

- [ ] **Step 1: Write the failing test** (a tiny hand-built .docx fixture is created by the test)

```python
# sidecar/test_parse.py
import io, zipfile, os, tempfile
from paperdock_worker import parse_docx

DOC_XML = '''<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:r><w:t>Metformin reduces mortality in sepsis</w:t></w:r>
<w:r><w:fldChar w:fldCharType="begin"/></w:r>
<w:r><w:instrText> ADDIN ZOTERO_ITEM CSL_CITATION {"citationItems":[{"uris":["http://zotero.org/users/0/items/ABCD1234"],"itemData":{"title":"Metformin in sepsis"}}]} </w:instrText></w:r>
<w:r><w:fldChar w:fldCharType="end"/></w:r>
<w:r><w:t> in ICU patients.</w:t></w:r></w:p>
<w:p><w:r><w:t>A sentence with no citation at all.</w:t></w:r></w:p>
</w:body></w:document>'''

def _make_docx(tmp):
    p = os.path.join(tmp, "m.docx")
    with zipfile.ZipFile(p, "w") as z:
        z.writestr("word/document.xml", DOC_XML)
    return p

def test_parse_docx_extracts_claim_and_key():
    with tempfile.TemporaryDirectory() as tmp:
        rows = parse_docx(_make_docx(tmp))
    assert len(rows) == 1                      # only the cited paragraph is a claim
    assert rows[0]["keys"] == ["ABCD1234"]
    assert "Metformin reduces mortality" in rows[0]["claim"]
    assert "ICU patients" in rows[0]["claim"]  # text after the field is included
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd sidecar && .venv/bin/python -m pytest test_parse.py -v` (or `python -m pytest` if pytest present; else `.venv/bin/python test_parse.py` with an `if __name__` asserts block)
Expected: FAIL — `ImportError: cannot import name 'parse_docx'`

- [ ] **Step 3: Implement `parse_docx`**

```python
import zipfile
import xml.etree.ElementTree as ET

_W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
_ZOTERO_KEY = re.compile(r"zotero\.org/(?:users|groups)/\d+/items/([A-Z0-9]+)")

def _para_text_and_fields(p):
    """Return (visible_text, [instrText strings]) for one <w:p>."""
    texts, fields = [], []
    for node in p.iter():
        if node.tag == _W + "t" and node.text:
            texts.append(node.text)
        elif node.tag == _W + "instrText" and node.text:
            fields.append(node.text)
    return "".join(texts), fields

def parse_docx(path):
    with zipfile.ZipFile(path) as z:
        xml = z.read("word/document.xml")
    root = ET.fromstring(xml)
    rows = []
    for p in root.iter(_W + "p"):
        text, fields = _para_text_and_fields(p)
        claim = " ".join(text.split()).strip()
        if not claim:
            continue
        keys, raw = [], []
        for f in fields:
            if "ZOTERO_ITEM" not in f:
                continue
            for k in _ZOTERO_KEY.findall(f):
                if k not in keys:
                    keys.append(k)
            m = re.search(r"\{.*\}", f, re.S)   # CSL JSON for human citation fallback
            if m:
                try:
                    data = json.loads(m.group(0))
                    for ci in data.get("citationItems", []):
                        t = (ci.get("itemData") or {}).get("title")
                        if t:
                            raw.append(t)
                except Exception:
                    pass
        if keys or raw:                          # a claim is a paragraph that cites
            rows.append({"claim": claim, "keys": keys, "cites_raw": raw})
    return rows
```

- [ ] **Step 4: Run test to verify it passes** — Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add sidecar/paperdock_worker.py sidecar/test_parse.py
git commit -m "feat(worker): parse .docx Zotero citations into (claim, keys)"
```

---

### Task 2: Worker `.tex` + `.bib` parser → claims + DOI/title

**Files:**
- Modify: `sidecar/paperdock_worker.py` (add `parse_tex`)
- Test: `sidecar/test_parse.py` (append)

**Interfaces:**
- Produces: `parse_tex(tex_path: str, bib_path: str|None) -> list[dict]` — same row shape as `parse_docx` but `keys` is empty (no Zotero key in LaTeX); each row also carries `"dois": [str]` and `"cites_raw": [str]` (title/first-author-year) resolved from the `.bib` so Rust can match to Zotero.

- [ ] **Step 1: Write the failing test**

```python
from paperdock_worker import parse_tex

TEX = r"Metformin cuts mortality \cite{smith2020} in sepsis. No cite here."
BIB = "@article{smith2020, title={Metformin in sepsis}, doi={10.1/x}, author={Smith, J}, year={2020}}"

def test_parse_tex_resolves_cite_to_doi():
    import tempfile, os
    with tempfile.TemporaryDirectory() as tmp:
        tp = os.path.join(tmp, "m.tex"); open(tp, "w").write(TEX)
        bp = os.path.join(tmp, "m.bib"); open(bp, "w").write(BIB)
        rows = parse_tex(tp, bp)
    assert len(rows) == 1
    assert rows[0]["dois"] == ["10.1/x"]
    assert "Metformin cuts mortality" in rows[0]["claim"]
    assert any("Metformin in sepsis" in c for c in rows[0]["cites_raw"])
```

- [ ] **Step 2: Run test to verify it fails** — Expected: FAIL `cannot import name 'parse_tex'`

- [ ] **Step 3: Implement `parse_tex`**

```python
_BIB_ENTRY = re.compile(r"@\w+\s*\{\s*([^,]+),(.*?)\n\}", re.S)
_BIB_FIELD = re.compile(r"(\w+)\s*=\s*[{\"]((?:[^{}]|\{[^{}]*\})*)[}\"]", re.S)
_CITE = re.compile(r"\\cite[a-zA-Z]*\*?(?:\[[^\]]*\])*\{([^}]*)\}")

def _parse_bib(path):
    """bibkey -> {'doi','title','author','year'}."""
    out = {}
    if not path or not os.path.exists(path):
        return out
    txt = open(path, encoding="utf-8", errors="ignore").read()
    for key, body in _BIB_ENTRY.findall(txt):
        fields = {k.lower(): " ".join(v.split())
                  for k, v in _BIB_FIELD.findall(body)}
        out[key.strip()] = fields
    return out

def _split_sentences(text):
    # naive but adequate: split on . ! ? followed by space+capital
    return re.split(r"(?<=[.!?])\s+(?=[A-Z\\])", text)

def parse_tex(tex_path, bib_path=None):
    bib = _parse_bib(bib_path)
    text = open(tex_path, encoding="utf-8", errors="ignore").read()
    text = re.sub(r"(?<!\\)%.*", "", text)         # strip comments
    rows = []
    for sent in _split_sentences(text):
        bibkeys = [k.strip() for grp in _CITE.findall(sent)
                   for k in grp.split(",") if k.strip()]
        if not bibkeys:
            continue
        claim = " ".join(_CITE.sub("", sent).split()).strip()
        dois, raw = [], []
        for bk in bibkeys:
            f = bib.get(bk, {})
            if f.get("doi"):
                dois.append(f["doi"])
            title = f.get("title")
            if title:
                yr = f.get("year", "")
                au = (f.get("author", "").split(",")[0] or "").strip()
                raw.append(" ".join(x for x in (au, yr, title) if x))
            elif not f:
                raw.append(bk)                     # unresolved bibkey — still surface it
        if claim:
            rows.append({"claim": claim, "keys": [], "dois": dois, "cites_raw": raw})
    return rows
```

- [ ] **Step 4: Run test — Expected: PASS**

- [ ] **Step 5: Commit**

```bash
git add sidecar/paperdock_worker.py sidecar/test_parse.py
git commit -m "feat(worker): parse .tex/.bib \\cite into (claim, dois)"
```

---

### Task 3: Worker `parse` command dispatch

**Files:**
- Modify: `sidecar/paperdock_worker.py` (`main()` dispatch + a `handle_parse(req)`)
- Test: manual (stdin JSON)

**Interfaces:**
- Consumes: `{"id","cmd":"parse","path": "...docx|.tex", "bib_path": "...bib"?}`
- Produces: `{"id","type":"claims","items":[{"claim","keys","dois"?,"cites_raw"}]}` then `{"type":"done"}`; `{"type":"error","message"}` on unreadable file.

- [ ] **Step 1: Implement dispatch** (no LLM, so it's synchronous & fast)

```python
def handle_parse(req):
    qid = req["id"]
    path = req.get("path") or ""
    try:
        if path.lower().endswith(".docx"):
            items = parse_docx(path)
        elif path.lower().endswith((".tex", ".latex")):
            items = parse_tex(path, req.get("bib_path"))
        else:
            send({"id": qid, "type": "error",
                  "message": "Import a .docx or .tex file."})
            return
    except Exception as exc:
        sys.stderr.write("parse failed: %r\n" % exc)
        send({"id": qid, "type": "error",
              "message": "Couldn't read that file — is it a valid .docx/.tex?"})
        return
    if not items:
        send({"id": qid, "type": "error",
              "message": "No cited claims found. (Word: citations must be "
                         "inserted with the Zotero plugin, not typed by hand.)"})
        return
    send({"id": qid, "type": "claims", "items": items})
    send({"id": qid, "type": "done"})
```

Wire in `main()`: add `parse` — but it is NOT an `answer_real` command (no asyncio needed):
```python
            if req.get("cmd") == "parse":
                handle_parse(req)
            elif req.get("cmd") in ("ask", "index", "check", "audit"):
                asyncio.run(answer_real(req))
```
(Note: `check_draft` removed from this tuple — see Task 6.)

- [ ] **Step 2: Manual test**

Run: `printf '{"id":"p1","cmd":"parse","path":"/abs/test.docx"}\n' | sidecar/.venv/bin/python sidecar/paperdock_worker.py`
Expected: a `"claims"` line then `"done"`.

- [ ] **Step 3: Commit**

```bash
git add sidecar/paperdock_worker.py
git commit -m "feat(worker): parse command dispatch"
```

---

### Task 4: Worker `audit` command — per-paper scoped, concurrent verdicts

**Files:**
- Modify: `sidecar/paperdock_worker.py` (new `handle_audit`, called from `answer_real` path or standalone async)
- Test: `sidecar/test_audit.py` (orchestration shape with a stubbed query)

**Interfaces:**
- Consumes: `{"id","cmd":"audit","claims":[{"idx":int,"claim":str,"key":str}], "docs":[{path,zotero_key,citation}], model/embedding/api_base/... same as ask}`. Each claim's `key` is the Zotero key of the ONE paper it cites (Rust resolves this before calling; claims whose paper has no PDF are NOT in this list — they're Tier-A-only, handled in Rust/UI).
- Produces, per claim as it finishes: `{"id","type":"claim_result","idx":int,"verdict":str,"detail":str,"passages":[{page,snippet}]}`; a final `{"type":"done"}`.

**Approach A:** group claims by `key`; for each cited paper build a **single-paper `Docs`** (only that paper's PDF added), so retrieval is structurally confined to it. Cache per single paper (existing digest cache already keys by the doc set — a 1-paper set gets its own cache file, reused across audits). Verdicts run with a `Semaphore`.

- [ ] **Step 1: Write the failing test** (stub the per-claim verify so no LLM/network)

```python
# sidecar/test_audit.py
import asyncio, paperdock_worker as w

def test_audit_groups_by_paper_and_bounds_concurrency(monkeypatch):
    seen = []
    async def fake_verify(claim, key, docs_for_key, settings):
        seen.append((claim, key))
        return {"verdict": "SUPPORTED", "detail": "ok", "passages": []}
    monkeypatch.setattr(w, "_verify_one", fake_verify)
    # two claims cite paper A, one cites B
    claims = [{"idx":0,"claim":"c0","key":"A"},
              {"idx":1,"claim":"c1","key":"B"},
              {"idx":2,"claim":"c2","key":"A"}]
    out = asyncio.run(w._run_audit(claims, docs_by_key={"A":object(),"B":object()},
                                   settings=None, concurrency=4))
    assert {r["idx"] for r in out} == {0,1,2}
    assert all(r["verdict"] == "SUPPORTED" for r in out)
    assert sorted(k for _,k in seen) == ["A","A","B"]
```

- [ ] **Step 2: Run — Expected: FAIL** (`_run_audit`/`_verify_one` missing)

- [ ] **Step 3: Implement** `_verify_one`, `_run_audit`, `handle_audit`

```python
async def _verify_one(claim, key, docs_for_key, settings):
    """Verdict for one claim against ONE cited paper's Docs (scoped retrieval)."""
    try:
        session = await docs_for_key.aquery(claim, settings=settings)
        ans = (session.answer or "").strip()
        passages = []
        for ctx in getattr(session, "contexts", [])[:2]:
            t = getattr(ctx, "text", None)
            name = getattr(t, "name", "") or ""
            page = name.split(" pages ", 1)[1].strip() if " pages " in name else ""
            snip = " ".join((getattr(ctx, "context", None)
                             or getattr(t, "text", "") or "").split())[:400]
            if snip:
                passages.append({"page": page, "snippet": snip})
    except Exception as exc:
        sys.stderr.write("verify failed (%s): %r\n" % (key, exc))
        return {"verdict": "INSUFFICIENT EVIDENCE",
                "detail": "Could not verify this claim.", "passages": []}
    return {"verdict": verdict_of(ans),
            "detail": " ".join(ans.split())[:240], "passages": passages}

async def _run_audit(claims, docs_by_key, settings, concurrency=4, on_result=None):
    sem = asyncio.Semaphore(concurrency)
    results = []
    async def one(c):
        async with sem:
            r = await _verify_one(c["claim"], c["key"], docs_by_key[c["key"]], settings)
            r["idx"] = c["idx"]
            results.append(r)
            if on_result:
                on_result(r)
    await asyncio.gather(*(one(c) for c in claims if c["key"] in docs_by_key))
    return results
```

`handle_audit(req)` (in `answer_real` when `cmd=="audit"`, AFTER settings are built and `CHECK_QA` is set — reuse that block): build one single-paper `Docs` per unique key by calling the existing add/cache logic with a one-element doc list; then:
```python
    concurrency = 1 if model.startswith("ollama/") else 4
    def emit(r):
        send({"id": qid, "type": "claim_result", **r})
    await _run_audit(claims, docs_by_key, settings, concurrency, on_result=emit)
    send({"id": qid, "type": "done"})
```
Reuse the existing per-paper cache/add code path — factor the "build a Docs for this doc list" block (lines ~256-339) into `async def _build_docs(doc_list, settings, ...)-> Docs` and call it once per key. `CHECK_QA` applies (`cmd=="audit"` added to the prompt-swap condition on line 233).

- [ ] **Step 4: Run — Expected: PASS**

- [ ] **Step 5: Commit**

```bash
git add sidecar/paperdock_worker.py sidecar/test_audit.py
git commit -m "feat(worker): audit command — per-paper scoped concurrent verdicts"
```

---

### Task 5: Rust — `parse_manuscript` + `run_audit` commands, delete `check_draft`

**Files:**
- Modify: `src-tauri/src/lib.rs` (add two commands; resolve keys; register; remove `check_draft`)
- Modify: `src-tauri/src/sidecar.rs` (a `run_parse` that sends the `parse` line and collects the `claims` event; `run_audit` reuses `run_ask` with a `claims` payload)

**Interfaces:**
- `parse_manuscript(app, state, path, bib_path) -> Vec<AuditClaim>` where `AuditClaim { idx, claim, key: Option<String>, citation: String, has_pdf: bool }`. For `.docx`, `key` = the Zotero key from the worker; for `.tex`, resolve `dois`/`cites_raw` against the library (match `verify_reference`-style or Zotero title match) → `key`. `has_pdf` = does that key resolve to a PDF on disk.
- `run_audit(app, state, library, collection_key, claims: Vec<AuditClaim>) -> ()` — spawn worker `audit`; only claims with `has_pdf` go in `claims`; resolve their PDFs via `zotero::collection_docs` (reuse) → `docs`. Emits `answer` events (`claim_result`, `done`).

- [ ] **Step 1:** Add `AuditClaim` struct + `parse_manuscript` (calls `sidecar::run_parse`; for `.tex` rows, resolve DOI/title → Zotero key by scanning the library's items; mark `has_pdf`).
- [ ] **Step 2:** Add `run_audit` (build `claims` JSON `[{idx,claim,key}]` for `has_pdf` rows; resolve those keys' PDFs; spawn worker `audit`).
- [ ] **Step 3:** Delete `check_draft` command + remove from `generate_handler!`. Register `parse_manuscript`, `run_audit`. Keep `verify_reference` (Tier A), `list_collection_papers`, `check` (single-claim path can call `run_audit` with one item later; leave `check` for now — do NOT delete in the same task to keep the diff reviewable).
- [ ] **Step 4:** `cargo check -p paperdock` — Expected: clean.
- [ ] **Step 5: Commit** `feat(rust): parse_manuscript + run_audit commands`

---

### Task 6: UI — import → review claims → per-claim results → export

**Files:**
- Modify: `src/main.rs` (audit mode UI, replace draft box), `src/styles.css` (`.audit-*`)

**Interfaces:** consumes `answer` events `claims` (→ editable review list), `claim_result` (→ fill row), Tier A via existing `verify_reference` invoke per unique cited paper.

- [ ] **Step 1:** Add an "Audit" mode button; import button → `parse_manuscript` → populate an editable claim list (each row: claim text editable, cited paper, remove; a "+ add missed claim" control that lets the user paste a claim and pick a paper from `list_collection_papers`).
- [ ] **Step 2:** "Run audit" → `run_audit`; render one row per claim; fill verdict + passages from `claim_result` events (match on `idx`); show a per-paper Tier A badge (call `verify_reference` on each unique citation string); a "flag" toggle per row (local state); a running `done/total` counter.
- [ ] **Step 3:** Export button → build a Markdown report from the rows (claim · paper · Tier A · verdict · passage · flagged) → save via a Tauri file dialog (reuse existing dialog plugin).
- [ ] **Step 4:** Remove the old draft box + `draft_items`/`submit_draft`/`pick_draft` wiring and the `check_draft` invoke. Keep `ask` and single-claim `check` modes.
- [ ] **Step 5:** `trunk build` — Expected: clean.
- [ ] **Step 6: Commit** `feat(ui): manuscript audit — review + per-claim results + export`

---

### Task 7: End-to-end test + report

- [ ] Build a real `.docx` in Word with 3–5 Zotero citations to papers in the NIH collection (5 have PDFs) + one citation to a paper with no PDF; and a `.tex`+`.bib` equivalent.
- [ ] Run the app; import each; confirm: claims list shows, PDFs resolve, audit streams per-claim verdicts, no-PDF citation shows Tier-A-only badge, export produces a readable report.
- [ ] Red-team: unreadable file, hand-typed (no-field) .docx, empty file, cancel mid-audit, 0 cited claims — each surfaces a human message, no stack trace.
- [ ] Write the result report for the user (what works E2E, what's stubbed, latency for N claims).

## Self-review notes

- Spec coverage: parse (T1/T2/T3), full-audit + reuse + concurrency (T4), per-paper approach A (T4 single-paper Docs), missing-PDF degrade→Tier A (T5 `has_pdf` + T6 badge, reuse `verify_reference`), review/correct UX (T6), per-claim streaming (T4 events + T6), export/trust passages (T4 passages + T6), delete demo (T5/T6). ✓
- Concurrency default 4 / ollama 1: T4 Step 3. ✓
- Reuse `CHECK_QA`/`verdict_of`/cache/Qdrant split: T4. ✓
- Non-goals (OA fetch, plain-text) excluded. ✓
