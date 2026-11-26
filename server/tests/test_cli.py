"""The one-shot CLI the server reads: `status`, `estimate --json` + plain, `speed --model --json`."""

from __future__ import annotations

import pytest

from engine import cli
from conftest import _cfg


def test_status_parses_the_engines_lines(corpus, index):
    status = cli.status(_cfg(index, (corpus,)))
    assert status["index"] == str(index / "index.sqlite")
    assert status["vectors"] >= 15
    assert status["build_tag"] and status["build_tag"].startswith("b")
    assert "embeddinggemma-2@" in status["fingerprints"]
    assert status["index_size"]
    kinds = {k["kind"]: k["files"] for k in status["kinds"]}
    assert kinds.get("image") == 12 and kinds.get("video") == 3


def test_estimate_is_the_engines_own_paragraph(corpus, index):
    text = cli.estimate_text(_cfg(index, (corpus,)))
    assert "first index" in text and "images" in text
    assert "can't read" not in text


def test_estimate_json_answers_for_the_server_as_parent(corpus, corpus_root, index):
    # `estimate --json` is a ParentWatch helper: live parent (pytest here; uvicorn in prod)
    # → one JSON line per folder; parent PID 1 → it exits at once, "the app is gone", by
    # design. The Stage-2 "silent estimate --json" report was that PID-1 invocation, not an
    # engine fault (reconnaissance §15 item 8). The server is the parent, so it uses --json.
    answer = cli.estimate_json(_cfg(index, (corpus_root,)))
    assert answer["readable"] is True
    assert answer["skipped"] >= 1 and answer["left_out"]          # 29 skipped files, counted
    media = cli.estimate_json(_cfg(index, (corpus,)))
    assert media["files"]["image"] == 12


@pytest.mark.slow
def test_find_answers_pictures_like_the_reference(corpus, index):
    """#162/#121 (2026-11-25): `digup find <photo>` embeds the reference with the FULL-shape
    media encoder at its stored twin's budget and ranks the vector pool by cosine under the
    SAME 2.0σ gate as a text search. The gate's law is the engine's own: a file qualifies by
    STANDING OUT (AGENTS.md: "fewer strong hits on tiny corpora = the 2.0σ gate over a small
    indexed set, identical on every platforms — the server never invents numbers"). Measured
    on this suite's 15-file pool the reference's neighbors do not clear it, so the honest
    answer is EMPTY (the engine refused to guess) — pinned verbatim below, with the model-
    free refusals pinned model-free in the next test. Slow: three full-shape model loads."""
    photo = f"{corpus}/photos/wakeboard-logan.jpg"
    cfg = _cfg(index, (corpus,))
    rows = cli.find(cfg, photo, ("--json",))
    assert {r["kind"] for r in rows} <= {"image", "screenshot"}, "find answers pictures only"
    assert photo not in [r["path"] for r in rows], "the reference drops out of its own answer"
    assert rows == [], \
        "measured: the suite pool's wakeboard neighbors do not clear the 2.0σ gate — \
         the engine's z-gate is its answer, and find_similar relays it, it does not pad it"

    # `--ext` is the same post-ranking KEEP as search's: it cannot conjure rows (the
    # equality pin is the law: kept rows are exactly the ranked rows with that extension).
    jpgs = cli.find(cfg, photo, ("--json", "--ext", "jpg"))
    assert all(r["path"].endswith(".jpg") for r in jpgs)
    assert [r["path"] for r in jpgs] == [r["path"] for r in rows if r["path"].endswith(".jpg")]

    # `--strong` keeps only the rows the engine counted strong: never more than the rows.
    strong = cli.find(cfg, photo, ("--json", "--strong"))
    assert len(strong) <= len(rows)


def test_find_refuses_references_it_cannot_embed(corpus, index):
    """The reference must be a picture: a document row in the index refuses with the
    engine's sentence, and a missing file refuses before the model ever loads."""
    cfg = _cfg(index, (corpus,))
    with pytest.raises(cli.CLIError, match="no such file"):
        cli.find(cfg, f"{corpus}/photos/definitely-not-here.jpg", ("--json",))
    with pytest.raises(cli.CLIError, match="not a picture"):
        cli.find(cfg, f"{corpus}/videos/wakeboarding-vienna.webm", ("--json",))


@pytest.mark.slow
def test_speed_model_json_reports_the_device(corpus, index):
    reply = cli.speed_model(_cfg(index, (corpus,)), when_quiet_s=2.0, tries=2, timeout=600.0)
    assert reply["event"] == "speed"
    assert reply["measured"] is True
    assert {"kinds", "seconds", "load", "total", "tries", "checks"} <= set(reply)
    assert reply["kinds"]                                   # per-kind multipliers, the engine's
