# Demo assets

Materials for recording the PaperDock v1.0 landing/README demo.

## `demo-manuscript.docx`
A Word manuscript with **6 cited claims** (Zotero citation field codes), designed
to produce a compelling **4 SUPPORTED + 2 caught mis-citation** result:

| # | Claim | Cited | Expected |
|---|-------|-------|----------|
| 1 | Reproducing RWE studies across databases → variable results | Wang 2022 | ✅ SUPPORTED |
| 2 | Independent teams reproducing a study → discordant conclusions | Ostropolets 2023 | ✅ SUPPORTED |
| 3 | Computable phenotypes don't always transport across sites | Pacheco 2023 | ✅ SUPPORTED |
| 4 | Next-gen phenotyping needs design desiderata | Chapman 2021 | ✅ SUPPORTED |
| 5 | RSV has seasonal circulation | **Wang 2022** (RWE paper — wrong) | ❌ NOT SUPPORTED |
| 6 | Metformin reduces mortality in sepsis | **Ostropolets 2023** (wrong) | ❌ INSUFFICIENT |

**Note:** the citation keys are the specific items in Chester's `NIH Replication
Prize – Track 1` Zotero collection. To reuse with a different library, rebuild the
docx swapping the `zotero.org/users/0/items/<KEY>` keys (see the generator in the
session history) — or just cite the same 5 open papers.

## Shot list (~30–40s)
1. App open, `NIH Replication Prize` collection selected, **Audit manuscript** mode.
2. Click **Import manuscript** → pick `demo-manuscript.docx`.
3. The 6 extracted claims appear (each with its cited paper) — pause a beat.
4. Click **Run audit**. Verdicts stream in one by one.
5. **The payoff:** 4 green SUPPORTED (with source passages) + **2 red NOT SUPPORTED**
   — the tool caught two claims whose cited paper doesn't back them.
6. (optional) Click **Export** to show the Markdown report.

Pre-warm the index once (run the audit, then re-import) before the take so verdicts
stream fast instead of waiting on cold embedding.
