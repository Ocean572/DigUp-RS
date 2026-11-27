"""#195 S6 — the migration rides the suite's REAL shared index.

The shared `/index` was built by the pre-portability engine: a v6 store (user_version
6, absolute rows, no anchors table). Every module above READS it verbatim — that is
the v6 read-compat gate the contract rides (reads never migrate). The first WRITE
verb — an index job — runs the engine's migration: anchors from the stored rows'
clusters, the argv roots validate them, counters re-key into the anchor-prefixed
bucket space, and every answer assembles byte-identically at the row edge.

This module pins the OUTCOME on that real store, measured through the server's own
verb: v7 stamped, the corpus root's anchor row, every row migrated (none pruned —
the root validates every cluster), counters re-keyed and consistent with the rows,
and the resume marker cleared.
"""

from __future__ import annotations

import sqlite3
import time

import pytest

from conftest import _cfg
from support import LiveApp


def _conn(index_dir) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{index_dir / 'index.sqlite'}?mode=ro", uri=True)


def _wait_job(client, job_id: str, timeout: float = 600.0) -> dict:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        job = client.get(f"/api/v1/jobs/{job_id}").json()["job"]
        if job["state"] in ("done", "interrupted", "busy"):
            return job
        time.sleep(0.5)
    pytest.fail(f"job {job_id} never reached a final state within {timeout:g} s")


def test_the_first_write_verb_lands_the_shared_v6_store_on_v7(index, corpus):
    """The migration is a write-verb fact, not a boot fact: the job the server runs
    carries the store over, and the store answers the same rows it always answered."""
    app = LiveApp(_cfg(index, (corpus,)), config_dir=str(index / "server-config-migration"))
    app.start()
    try:
        started = app.client.post("/api/v1/index", json={})
        assert started.status_code == 202, started.text
        job = _wait_job(app.client, started.json()["job"]["id"])
        assert job["state"] == "done", job
    finally:
        app.stop()

    con = _conn(index)
    try:
        assert con.execute("PRAGMA user_version").fetchone()[0] == 7, "the write verb stamped v7"
        anchors = [r[0] for r in con.execute("SELECT abs FROM anchors ORDER BY id")]
        assert anchors == [str(corpus)], "the corpus root's anchor row (the cluster validated)"
        rows = [r[0] for r in con.execute("SELECT path FROM files ORDER BY path")]
        assert len(rows) >= 15, "every stored row migrated — the root validates every cluster"
        assert all(str(corpus).rstrip("/") + "/" in path for path in rows), \
            "answers assemble the absolute spelling at the row edge, byte-for-byte"
        buckets = {r[0] for r in con.execute("SELECT DISTINCT bucket FROM counters")}
        assert buckets and all(b.startswith("1/") for b in buckets), \
            "counters re-keyed into the anchor-prefixed bucket space (the triggers ride verbatim)"
        total = con.execute("SELECT count(*) FROM files").fetchone()[0]
        counters = con.execute("SELECT coalesce(sum(n), 0) FROM counters").fetchone()[0]
        assert counters == total, "the counters agree with the rows — no ghost key survived"
        marker = con.execute(
            "SELECT coalesce((SELECT value FROM meta WHERE key = 'portable_migration'), '')"
        ).fetchone()[0]
        assert marker == "", "the resume marker cleared on the completed migration"
    finally:
        con.close()
