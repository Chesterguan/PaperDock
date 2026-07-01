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
    use_qdrant = bool(qdrant_url and collection_key)
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
            sys.stderr.write("add failed for %s: %r\n" % (path, exc))

    if not docs.docs:
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

    send({"id": qid, "type": "status", "text": "Thinking…"})
    session = await docs.aquery(question, settings=settings)

    text = (session.answer or "").strip()
    if not text:
        text = "No grounded answer could be drawn from these papers."
    for word in text.split(" "):
        send({"id": qid, "type": "token", "text": word + " "})
        time.sleep(0.01)  # cosmetic: makes the answer visibly stream in

    used = session.get_unique_docs_from_contexts(score_threshold=1)
    if not used:
        used = session.get_unique_docs_from_contexts()
    items, seen = [], set()
    for doc in used:
        key = getattr(doc, "dockey", None)
        if not key or key in seen:
            continue
        seen.add(key)
        items.append({"zotero_key": key,
                      "citation": getattr(doc, "citation", None) or key})
    send({"id": qid, "type": "references", "items": items})
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
            if req.get("cmd") in ("ask", "index"):
                asyncio.run(answer_real(req))
            else:
                send({"id": req.get("id"), "type": "error",
                      "message": "Unknown command."})
        except Exception as exc:  # never leak a stack trace to the UI
            send({"id": req.get("id"), "type": "error",
                  "message": "The query engine failed. See logs for details."})
            sys.stderr.write("worker error: %r\n" % exc)


if __name__ == "__main__":
    main()
