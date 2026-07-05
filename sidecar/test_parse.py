"""Self-checks for the manuscript parsers. Run: .venv/bin/python test_parse.py"""
import os
import tempfile
import zipfile

from paperdock_worker import parse_docx, parse_tex, _parse_bib

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
    assert len(rows) == 1, rows                       # only the cited paragraph
    assert rows[0]["keys"] == ["ABCD1234"], rows[0]
    assert "Metformin reduces mortality" in rows[0]["claim"]
    assert "ICU patients" in rows[0]["claim"]         # text after the field included
    assert rows[0]["cites_raw"] == ["Metformin in sepsis"]


TEX = r"Metformin cuts mortality \cite{smith2020} in sepsis. No cite here."
BIB = "@article{smith2020,\n title={Metformin in sepsis}, doi={10.1/x}, author={Smith, J}, year={2020}\n}"


def test_parse_tex_resolves_cite_to_doi():
    with tempfile.TemporaryDirectory() as tmp:
        tp = os.path.join(tmp, "m.tex")
        open(tp, "w").write(TEX)
        bp = os.path.join(tmp, "m.bib")
        open(bp, "w").write(BIB)
        rows = parse_tex(tp, bp)
    assert len(rows) == 1, rows
    assert rows[0]["dois"] == ["10.1/x"], rows[0]
    assert "Metformin cuts mortality" in rows[0]["claim"]
    assert "\\cite" not in rows[0]["claim"]           # cite command stripped
    assert any("Metformin in sepsis" in c for c in rows[0]["cites_raw"])


def test_parse_tex_multi_cite_and_unresolved():
    tex = r"Both agents work \cite{a2020,ghost1999}."
    bib = "@article{a2020, title={A study}, doi={10.2/y}, year={2020}\n}"
    with tempfile.TemporaryDirectory() as tmp:
        tp = os.path.join(tmp, "m.tex"); open(tp, "w").write(tex)
        bp = os.path.join(tmp, "m.bib"); open(bp, "w").write(bib)
        rows = parse_tex(tp, bp)
    assert rows[0]["dois"] == ["10.2/y"], rows[0]
    assert "ghost1999" in rows[0]["cites_raw"], rows[0]  # unresolved key surfaced


def test_parse_tex_strips_preamble_and_commands():
    tex = (r"\documentclass{article}" "\n" r"\begin{document}" "\n"
           r"\section{Intro} Metformin cuts mortality \cite{a2020} in sepsis." "\n"
           r"\end{document}")
    bib = "@article{a2020, title={A study}, doi={10.2/y}, year={2020}\n}"
    with tempfile.TemporaryDirectory() as tmp:
        tp = os.path.join(tmp, "m.tex"); open(tp, "w").write(tex)
        bp = os.path.join(tmp, "m.bib"); open(bp, "w").write(bib)
        rows = parse_tex(tp, bp)
    assert len(rows) == 1, rows
    c = rows[0]["claim"]
    assert "documentclass" not in c and "\\" not in c, c   # no LaTeX markup
    assert "section" not in c.lower(), c
    assert "Metformin cuts mortality" in c, c
    assert "in sepsis" in c, c


if __name__ == "__main__":
    test_parse_docx_extracts_claim_and_key()
    test_parse_tex_resolves_cite_to_doi()
    test_parse_tex_multi_cite_and_unresolved()
    test_parse_tex_strips_preamble_and_commands()
    print("test_parse: all passed")
