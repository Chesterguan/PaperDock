"""Self-check for audit orchestration (no LLM/network — verify is stubbed).
Run: .venv/bin/python test_audit.py"""
import asyncio
import paperdock_worker as w


def test_audit_groups_by_paper_and_streams_all():
    seen = []

    async def fake_verify(claim, key, docs_for_key, settings):
        seen.append((claim, key))
        return {"verdict": "SUPPORTED", "detail": "ok", "passages": []}

    orig = w._verify_one
    w._verify_one = fake_verify
    try:
        emitted = []
        claims = [{"idx": 0, "claim": "c0", "key": "A"},
                  {"idx": 1, "claim": "c1", "key": "B"},
                  {"idx": 2, "claim": "c2", "key": "A"},
                  {"idx": 3, "claim": "c3", "key": "MISSING"}]  # no Docs → skipped
        out = asyncio.run(w._run_audit(
            claims, docs_by_key={"A": object(), "B": object()},
            settings=None, concurrency=4, on_result=emitted.append))
    finally:
        w._verify_one = orig

    # claim 3 (MISSING key) is skipped; 0,1,2 verified and streamed
    assert {r["idx"] for r in out} == {0, 1, 2}, out
    assert len(emitted) == 3, emitted
    assert all(r["verdict"] == "SUPPORTED" for r in out)
    assert sorted(k for _, k in seen) == ["A", "A", "B"], seen


def test_concurrency_bound_respected():
    """With concurrency=1 the semaphore must serialize — peak in-flight == 1."""
    state = {"cur": 0, "peak": 0}

    async def fake_verify(claim, key, docs_for_key, settings):
        state["cur"] += 1
        state["peak"] = max(state["peak"], state["cur"])
        await asyncio.sleep(0.005)
        state["cur"] -= 1
        return {"verdict": "SUPPORTED", "detail": "", "passages": []}

    orig = w._verify_one
    w._verify_one = fake_verify
    try:
        claims = [{"idx": i, "claim": "c", "key": "A"} for i in range(6)]
        asyncio.run(w._run_audit(claims, {"A": object()}, None, concurrency=1))
    finally:
        w._verify_one = orig
    assert state["peak"] == 1, state


if __name__ == "__main__":
    test_audit_groups_by_paper_and_streams_all()
    test_concurrency_bound_respected()
    print("test_audit: all passed")
