#!/usr/bin/env python3
"""PaperDock sidecar worker — real PaperQA backend.

Speaks newline-delimited JSON over stdin/stdout. Reads ONE `ask` request,
builds an in-memory PaperQA index from the PDFs Rust resolved, runs a grounded
query, streams the answer, and reports which papers were actually cited
(mapped back to their Zotero item keys via PaperQA's `dockey`).

Run with the bundled venv: `sidecar/.venv/bin/python` (PaperQA needs 3.11+).

Protocol — one JSON object per line.
  in : {"id","cmd":"ask","question","index_name","cache_dir","model",
        "docs":[{"path","zotero_key","citation"}]}
  out: {"id","type":"status","text"}
       {"id","type":"token","text"}
       {"id","type":"references","items":[{"zotero_key","citation"}]}
       {"id","type":"done"} | {"id","type":"error","message"}

PaperQA reads its LLM/embedding API key from the environment (LiteLLM
convention, e.g. OPENAI_API_KEY for gpt-* models). The worker inherits the
app's environment; if the key is missing it returns a human message, never a
stack trace.

ponytail: the index is rebuilt in-memory per ask (fresh process each time, so
nothing persists). For 5-10 papers that's fine; persist a PaperQA index under
`cache_dir` if re-embedding latency/cost becomes a problem.
ponytail: tokens are the finished answer streamed word-by-word — PaperQA runs
several LLM calls per query, so true per-token streaming would interleave them.
Stream the final answer only; wire a callback to the answer LLM if real-time
typing matters.
"""
import asyncio
import json
import os
import sys
import time


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def _is_openai(m: str) -> bool:
    m = (m or "").lower()
    return (m.startswith(("gpt", "o1", "o3", "text-embedding", "openai/"))
            or m in ("davinci", "babbage"))


def _missing_key_message(model: str, embedding: str):
    """Return a human message if an obviously-required API key is absent.

    Local models (ollama/…, or anything behind a custom api_base) need no key.
    """
    needs_openai = _is_openai(model) or _is_openai(embedding)
    if needs_openai and not os.environ.get("OPENAI_API_KEY"):
        return ("No OpenAI API key found. Add it in Settings, or switch to a "
                "local model (e.g. ollama/llama3.1 + ollama/nomic-embed-text).")
    if (model or "").lower().startswith("claude") \
            and not os.environ.get("ANTHROPIC_API_KEY"):
        return ("No Anthropic API key found. Add it in Settings, or switch to "
                "a local model.")
    return None


def _is_transient(exc) -> bool:
    s = repr(exc).lower()
    name = type(exc).__name__.lower()
    return any(t in name or t in s for t in (
        "ratelimit", "rate limit", "429", "timeout", "timed out",
        "connection", "serviceunavailable", "503", "overloaded"))


def _human_error(exc) -> str:
    """Turn a raw exception into a specific, actionable message."""
    s = repr(exc).lower()
    name = type(exc).__name__.lower()
    if "ratelimit" in name or "rate limit" in s or "429" in s:
        return ("The AI service is rate-limited right now (shared key is busy). "
                "Wait a few seconds and ask again.")
    if "timeout" in name or "timed out" in s or "timeout" in s:
        return "The AI service timed out. Please try again."
    if ("authentication" in name or "api key" in s or "unauthorized" in s
            or "401" in s or "403" in s):
        return "The AI service rejected the API key — check it in Settings (⚙)."
    if "connection" in name or "could not connect" in s or "connect" in s:
        return "Couldn't reach the AI service. Check your network / VPN."
    if "context" in s and ("length" in s or "maximum" in s or "token" in s):
        return ("The question pulled in too much text for the model. "
                "Try a narrower question or a smaller collection.")
    return ("The AI service hit an unexpected error. Try again; if it keeps "
            "happening, check the model/gateway in Settings (⚙).")


async def _aquery_with_retry(docs, question, settings):
    """Query once; retry a single time on a transient (rate-limit/timeout) error."""
    for attempt in range(2):
        try:
            return await docs.aquery(question, settings=settings)
        except Exception as exc:
            if attempt == 0 and _is_transient(exc):
                sys.stderr.write("transient query error, retrying: %r\n" % exc)
                await asyncio.sleep(2.5)
                continue
            raise


# Verdict prompt shared by Check-citation and Check-draft. Uses only
# {question}/{context}/{example_citation}; PaperQA passes extra format kwargs
# which str.format() harmlessly ignores.
CHECK_QA = (
    "You are fact-checking a claim against the provided source excerpts.\n\n"
    "Claim: {question}\n\n"
    "Context (excerpts from the source papers):\n{context}\n\n"
    "Using ONLY the context, judge whether the sources support the claim. "
    "Begin your answer with a verdict on its own line — exactly one of: "
    "SUPPORTED, PARTIALLY SUPPORTED, NOT SUPPORTED, or INSUFFICIENT EVIDENCE. "
    "Then justify it in 1-3 sentences, quoting the key evidence and citing "
    "the source with a citation key like {example_citation}. If the context "
    "does not address the claim, answer INSUFFICIENT EVIDENCE."
)


def verdict_of(text):
    """Extract the verdict label from a check answer (first line / start)."""
    t = (text or "").strip().upper()
    # Longest/compound labels first so "SUPPORTED" doesn't shadow the others.
    for v in ("PARTIALLY SUPPORTED", "NOT SUPPORTED", "INSUFFICIENT EVIDENCE",
              "SUPPORTED"):
        if t.startswith(v) or v in t[:70]:
            return v
    return "INSUFFICIENT EVIDENCE"


async def answer_real(req):
    qid = req["id"]
    index_only = req.get("cmd") == "index"  # pre-embed into Qdrant, no LLM query
    docs_in = req.get("docs") or []
    question = (req.get("question") or "").strip()
    model = req.get("model") or "gpt-4o"
    embedding = req.get("embedding") or "text-embedding-3-small"
    api_base = (req.get("api_base") or "").strip()
    cache_dir = req.get("cache_dir") or os.path.join(
        os.path.expanduser("~"), ".paperdock_cache")

    if not docs_in:
        send({"id": qid, "type": "error",
              "message": "No PDFs found in this collection."})
        return
    if not index_only and not question:
        send({"id": qid, "type": "error", "message": "Please type a question."})
        return

    # Use the key entered in the UI only when the env var isn't already set.
    ui_key = (req.get("api_key") or "").strip()
    if ui_key and not os.environ.get("OPENAI_API_KEY"):
        os.environ["OPENAI_API_KEY"] = ui_key

    # Point LiteLLM at a self-hosted / gateway backend if given. Ollama models
    # read OLLAMA_API_BASE; an OpenAI-compatible gateway (e.g. UF Navigator at
    # https://api.ai.it.ufl.edu/v1, used via openai/<model>) reads OPENAI_API_BASE.
    # Set both — LiteLLM only consults the one matching the model prefix.
    if api_base:
        os.environ.setdefault("OLLAMA_API_BASE", api_base)
        os.environ.setdefault("OPENAI_API_BASE", api_base)
        os.environ.setdefault("OPENAI_BASE_URL", api_base)

    missing = _missing_key_message(model, embedding)
    if missing:
        send({"id": qid, "type": "error", "message": missing})
        return

    from paperqa import Docs, Settings

    settings = Settings(
        llm=model,
        summary_llm=model,
        embedding=embedding,
        temperature=0.0,
    )
    settings.verbosity = 0
    # PaperQA has FOUR model roles that each default to gpt-4o — point every one
    # at the configured model, else a local (Ollama) setup still tries to reach
    # OpenAI for enrichment/agent calls and fails with "missing credentials".
    settings.parsing.enrichment_llm = model
    settings.agent.agent_llm = model
    # Text-only parsing: the default sends PDF figures/tables to a *vision* LLM,
    # which needs a multimodal model (and pillow). Off = pure text RAG that works
    # with any local text model, is faster, and drops the image dependency.
    from paperqa.settings import MultimodalOptions
    settings.parsing.multimodal = MultimodalOptions.OFF

    # Local models are slow per call. PaperQA's default does one summary LLM call
    # per retrieved chunk (evidence_k=10) plus the answer — ~11 calls. On a local
    # model each call can exceed the 60s LiteLLM timeout. For local backends skip
    # the per-chunk summary (raw passages go to the answer) and retrieve fewer
    # chunks, collapsing it to ~1 call, and raise the timeout generously.
    if api_base:
        # Any custom backend can be slower than OpenAI — give it room.
        import litellm
        litellm.request_timeout = 600
    if model.startswith("ollama/"):
        # Genuinely-slow local models: skip the per-chunk summary LLM call
        # (raw passages go straight to the answer) and retrieve a bit less,
        # collapsing ~11 calls to ~1. A fast gateway (Navigator) keeps summaries.
        settings.answer.evidence_skip_summary = True
        settings.answer.evidence_k = 8
        settings.answer.answer_max_sources = 6
    # We already supply citation + dockey per doc (mapped to Zotero), so skip
    # PaperQA's per-doc metadata LLM call — saves a request per paper and the
    # "Failed to parse … citation" noise.
    settings.parsing.use_doc_details = False

    # Multi-turn: give the ANSWER LLM the last couple of Q&A turns so follow-ups
    # ("that method", "why?") resolve. Injected into the answer prompt only —
    # retrieval still runs on the clean question below, so old topics don't
    # pollute the PDF evidence search.
    # ponytail: 2-turn cap is enforced frontend-side; brace-escape here so LaTeX
    # or JSON braces in a prior answer can't break the qa template's .format().
    history_text = (req.get("history") or "").strip()
    if history_text and not index_only:
        safe = history_text.replace("{", "{{").replace("}", "}}")
        settings.prompts.qa = (
            "Earlier in this conversation (context; the user may refer back to "
            "it):\n" + safe + "\n\n" + settings.prompts.qa
        )

    # Citation-check mode: reuse the whole embed+retrieve pipeline, but the
    # `question` is a CLAIM and the answer LLM judges SUPPORT instead of
    # answering. Swap the qa prompt for a verdict prompt (retrieval still runs
    # on the clean claim). Uses only {question}/{context}/{example_citation};
    # PaperQA passes extra format kwargs, which str.format() harmlessly ignores.
    if req.get("cmd") in ("check", "check_draft"):
        settings.prompts.qa = CHECK_QA

    # Where does the vector index live?
    #  - Qdrant Cloud (if configured): a SHARED index, scoped per Zotero
    #    collection + embedding model. Papers embedded by anyone in the org are
    #    reused by everyone — new papers embed once, old ones load from the cloud.
    #  - else: a local pickle keyed by the exact paper set + embedding model.
    import hashlib
    import pickle
    import re

    qdrant_url = (req.get("qdrant_url") or "").strip()
    qdrant_key = (req.get("qdrant_key") or "").strip()
    collection_key = (req.get("collection_key") or "").strip()
    # Only SHARED group libraries go to the team Qdrant cloud. Personal libraries
    # (scope "users_0_…") stay in a LOCAL on-disk index so private papers never
    # leave the machine. (collection_key is "<library>_<zoteroKey>" from Rust;
    # group libraries are "groups_<id>_…".) NaviGator has no usable per-user
    # vector store — its virtual key is locked to llm_api_routes — so local it is.
    use_qdrant = bool(qdrant_url and collection_key and collection_key.startswith("groups_"))
    emb_tag = re.sub(r"[^A-Za-z0-9]+", "_", str(settings.embedding))[:40].strip("_")

    docs = None
    if use_qdrant:
        try:
            from qdrant_client import AsyncQdrantClient
            from paperqa import QdrantVectorStore
            coll = "pd_%s_%s" % (collection_key, emb_tag)
            client = AsyncQdrantClient(url=qdrant_url, api_key=qdrant_key or None)
            if await client.collection_exists(coll):
                send({"id": qid, "type": "status", "text": "Loading shared index…"})
                docs = await QdrantVectorStore.load_docs(client, coll)
            else:
                docs = Docs(texts_index=QdrantVectorStore(
                    client=client, collection_name=coll))
        except Exception as exc:  # network/creds problem → fall back to local
            sys.stderr.write("qdrant init failed, using local cache: %r\n" % exc)
            use_qdrant = False
            docs = None

    keys = sorted((d.get("zotero_key") or "") for d in docs_in)
    digest = hashlib.sha1(
        ("|".join(keys) + "@" + str(settings.embedding)).encode()
    ).hexdigest()[:16]
    cache_file = os.path.join(cache_dir, "index_%s.pkl" % digest)
    if docs is None and not use_qdrant and os.path.exists(cache_file):
        try:
            with open(cache_file, "rb") as fh:
                docs = pickle.load(fh)
        except Exception as exc:
            sys.stderr.write("cache load failed: %r\n" % exc)
            docs = None
    if docs is None:
        docs = Docs()

    present = {getattr(dd, "dockey", None) for dd in docs.docs.values()}
    to_add = [d for d in docs_in if d.get("zotero_key") not in present]

    n_add = len(to_add)
    added_now = 0
    had_transient_add_error = False
    verb = "Indexing" if index_only else "Reading"
    for i, d in enumerate(to_add, 1):
        # Live progress so a first-run (embedding) doesn't look frozen.
        send({"id": qid, "type": "status",
              "text": "%s paper %d/%d…" % (verb, i, n_add)})
        path = d.get("path")
        if not path or not os.path.exists(path):
            continue
        try:
            # Pin citation/docname/dockey so PaperQA does no network metadata
            # lookup and the cited docs map straight back to Zotero keys.
            await docs.aadd(
                path,
                citation=d.get("citation"),
                docname=d.get("citation") or d.get("zotero_key"),
                dockey=d.get("zotero_key"),
                settings=settings,
            )
            added_now += 1
        except Exception as exc:  # one bad PDF shouldn't sink the whole query
            if _is_transient(exc):
                had_transient_add_error = True
            sys.stderr.write("add failed for %s: %r\n" % (path, exc))

    if not docs.docs:
        # Distinguish "genuinely no readable PDFs" from a transient API hiccup
        # while embedding (rate limit / timeout), which is retryable.
        if had_transient_add_error:
            send({"id": qid, "type": "error",
                  "message": "The AI service was busy while indexing (rate limit / "
                             "timeout). Wait a few seconds and try again."})
        else:
            send({"id": qid, "type": "error",
                  "message": "Could not read any PDFs in this collection."})
        return

    # Persist. Qdrant is written automatically when aquery builds the index, so
    # only the local pickle needs an explicit save.
    if added_now and not use_qdrant:
        try:
            os.makedirs(cache_dir, exist_ok=True)
            with open(cache_file, "wb") as fh:
                pickle.dump(docs, fh)
        except Exception as exc:
            sys.stderr.write("cache save failed: %r\n" % exc)

    # Index-only (pre-embed): flush any new embeddings to the shared Qdrant index
    # and stop — no LLM query. Makes later asks instant and keeps the group's
    # shared index complete.
    if index_only:
        if use_qdrant:
            try:
                await docs._build_texts_index(settings.get_embedding_model())
            except Exception as exc:
                sys.stderr.write("index flush failed: %r\n" % exc)
        send({"id": qid, "type": "status",
              "text": "Indexed %d paper%s ✓" % (
                  len(docs.docs), "" if len(docs.docs) == 1 else "s")})
        send({"id": qid, "type": "done"})
        return

    # Draft batch check: pull the checkable claims out of a pasted draft, then
    # judge each against the collection. Emits one "draft" result + counts.
    if req.get("cmd") == "check_draft":
        import re as _re
        import litellm
        draft = question[:20000]
        send({"id": qid, "type": "status", "text": "Extracting claims…"})
        ex_prompt = (
            "Extract up to 8 checkable factual claims from the text below. "
            "Return ONLY a JSON array of short standalone sentences — no prose, "
            "no numbering.\n\nText:\n" + draft
        )
        claims = []
        try:
            r = await litellm.acompletion(
                model=model, temperature=0.0,
                api_base=(api_base or None),
                messages=[{"role": "user", "content": ex_prompt}])
            raw = r["choices"][0]["message"]["content"]
            m = _re.search(r"\[.*\]", raw, _re.S)
            if m:
                claims = [str(c).strip() for c in json.loads(m.group(0))
                          if str(c).strip()]
        except Exception as exc:
            sys.stderr.write("claim extract failed: %r\n" % exc)
        claims = claims[:8]
        if not claims:
            send({"id": qid, "type": "error",
                  "message": "Couldn't find checkable claims in that text."})
            return
        items = []
        for i, claim in enumerate(claims, 1):
            send({"id": qid, "type": "status",
                  "text": "Checking claim %d/%d…" % (i, len(claims))})
            try:
                session = await docs.aquery(claim, settings=settings)
                ans = (session.answer or "").strip()
            except Exception as exc:
                ans = "INSUFFICIENT EVIDENCE"
                sys.stderr.write("draft claim failed: %r\n" % exc)
            items.append({"claim": claim, "verdict": verdict_of(ans),
                          "detail": " ".join(ans.split())[:240]})
        send({"id": qid, "type": "draft", "items": items})
        send({"id": qid, "type": "done"})
        return

    send({"id": qid, "type": "status",
          "text": "Checking…" if req.get("cmd") == "check" else "Thinking…"})
    session = await _aquery_with_retry(docs, question, settings)

    text = (session.answer or "").strip()
    if not text:
        text = "No grounded answer could be drawn from these papers."
    for word in text.split(" "):
        send({"id": qid, "type": "token", "text": word + " "})
        time.sleep(0.01)  # cosmetic: makes the answer visibly stream in

    # Build references (with cited passages). Best-effort: the answer already
    # streamed, so a hiccup here must NEVER sink the whole response.
    try:
        used = session.get_unique_docs_from_contexts(score_threshold=1)
        if not used:
            used = session.get_unique_docs_from_contexts()

        # Cited passages per doc (raw text + page + score) so the UI shows the
        # evidence, not just a citation. Free from session.contexts.
        from collections import defaultdict
        by_key = defaultdict(list)
        for ctx in session.contexts:
            text = getattr(ctx, "text", None)
            k = getattr(getattr(text, "doc", None), "dockey", None)
            if not text or not k:
                continue
            name = getattr(text, "name", None) or ""
            page = name.split(" pages ", 1)[1].strip() if " pages " in name else ""
            # Prefer PaperQA's evidence summary (clean, complete, question-
            # relevant). Fall back to a cleaned raw excerpt (local models that
            # skip the summary), wrapped in ellipses to read as an excerpt.
            summary = " ".join((getattr(ctx, "context", None) or "").split())
            if summary:
                snippet = summary[:700]
            else:
                raw = " ".join((getattr(text, "text", None) or "").split())
                snippet = ("…" + raw[:300].strip() + "…") if raw else ""
            by_key[k].append({
                "page": page,
                "snippet": snippet,
                "score": getattr(ctx, "score", 0) or 0,
            })

        items, seen = [], set()
        for doc in used:
            key = getattr(doc, "dockey", None)
            if not key or key in seen:
                continue
            seen.add(key)
            passages = sorted(by_key.get(key, []), key=lambda p: -p["score"])[:3]
            items.append({"zotero_key": key,
                          "citation": getattr(doc, "citation", None) or key,
                          "passages": passages})
        send({"id": qid, "type": "references", "items": items})
    except Exception as exc:
        sys.stderr.write("references build failed (answer already sent): %r\n" % exc)

    send({"id": qid, "type": "done"})


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            send({"id": None, "type": "error", "message": "Malformed request."})
            continue
        try:
            if req.get("cmd") in ("ask", "index", "check", "check_draft"):
                asyncio.run(answer_real(req))
            else:
                send({"id": req.get("id"), "type": "error",
                      "message": "Unknown command."})
        except Exception as exc:  # never leak a stack trace to the UI
            send({"id": req.get("id"), "type": "error",
                  "message": _human_error(exc)})
            sys.stderr.write("worker error: %r\n" % exc)


if __name__ == "__main__":
    main()
