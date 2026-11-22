"""The faces contract (FACES.md Phase 3): the consent wall, the scan, the People verbs.

Runs in the `server-test` container over a copy of the **corpus photos** (the 12 Wikimedia
files conftest mounts at /corpus/photos): the server's own roots, the engine's own answers,
no mocks. The numbers below are measured rows, not expectations — 12 photos → 3 faces
(`wakeboard-logan.jpg`, `fishing-from-dock-oregon.jpg`, `man-on-dock-with-fish-1900.jpg`),
each its own person at the default merge threshold 0.5 (measured 2026-11-22 on
ai-ubuntu-computer, w600k_mbf, the same gate numerics FACES.md pins). A face model is a
**user-supplied, consent-gated** weight (FaceModelFiles pins): the suite needs the pinned
packs in the models folder — a missing pin fails the scan loudly (503, the daemon's words),
it never skips quietly.

The load-bearing laws this module pins, in file order:

* the consent wall: EVERY faces verb — scan and reads alike — answers 400 with the engine's
  sentence before a consent record exists, and the record is written only by the affirmative
  act (`accept: true`, admin) as a 0600 file the deployment keeps;
* the scan is the engine's: counts relayed verbatim, roots are the server's own roots
  (no path is accepted from a client), piggyback freshness skips recorded files, and ONE
  scan at a time — a second request answers 409 (the index writer's flock law);
* face rows mint the existing `/preview` media handle (the frontend crops with the row's
  box/kps — the server has no crop verbs and invents no scores);
* name/merge/delete/confirm relay the engine, the pending queue carries the engine's own
  candidate cosines, `similar` ranks by the engine's brute-force cosine excluding the face
  itself;
* withdrawing consent closes the verbs again (the face store stays — mass deletion is the
  user's act, never a side effect).

The scan runs seconds-per-photo (detect is ~4 s/photo CPU): the module keeps its own
client with an engine-sized timeout, and every cost is paid ONCE — one server, one scan,
freshness for the rest.
"""

from __future__ import annotations

import os
import shutil
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import httpx
import pytest

from conftest import _cfg
from support import LiveApp

# The fixture-measured face rows (measured 2026-11-22, ai-ubuntu-computer, w600k_mbf, merge
# 0.5 — the numbers the engine's gate numerics make reproducible on any platform).
FACE_PHOTOS = {"wakeboard-logan.jpg", "fishing-from-dock-oregon.jpg", "man-on-dock-with-fish-1900.jpg"}
PHOTO_COUNT = 12
FACE_COUNT = 3
CONSENT_SENTENCE = "face recognition is not consented for target 'media'"

IMAGE_SUFFIXES = {".jpg", ".jpeg", ".png"}


class Faces:
    """The live server over the faces corpus, with an engine-sized client timeout: the
    scan is synchronous on the engine's clock (seconds per photo), far past the plain
    module fixtures' 10 s."""

    def __init__(self, app: LiveApp):
        self.app = app
        self.config_dir = Path(app.config_dir)
        self.index = Path(app.cfg.index)
        self.client = httpx.Client(
            base_url=f"http://127.0.0.1:{app.port}",
            timeout=httpx.Timeout(900.0, connect=10.0),
        )

    def close(self) -> None:
        self.client.close()
        self.app.stop()

    def consent_path(self) -> Path:
        return self.config_dir / "faces-consent.json"

    def scan(self, body: dict | None = None):
        return self.client.post("/api/v1/faces/scan", json=body if body is not None else {})

    def persons(self) -> list[dict]:
        reply = self.client.get("/api/v1/faces/persons")
        assert reply.status_code == 200, reply.text
        return reply.json()["persons"]

    def counts(self) -> dict:
        reply = self.client.get("/api/v1/faces/status")
        assert reply.status_code == 200, reply.text
        return reply.json()["counts"]


@pytest.fixture(scope="module")
def faces_corpus(corpus, tmp_path_factory) -> Path:
    """A writable copy of the corpus photos: the server's roots, and the folder the
    busy-window test adds its 3 new files to (the scan is a reader, the corpus must be
    the deployment's own tree — a copy is how every other module builds its roots)."""
    root = tmp_path_factory.mktemp("faces-corpus")
    photos = [p for p in sorted((corpus / "photos").iterdir()) if p.suffix.lower() in IMAGE_SUFFIXES]
    assert len(photos) == PHOTO_COUNT, f"the faces rows pin {PHOTO_COUNT} corpus photos, found {len(photos)}"
    for photo in photos:
        shutil.copy(photo, root / photo.name)
    return root


@pytest.fixture(scope="module")
def faces(faces_corpus, tmp_path_factory):
    """One server, one index, one scan stretch: face.sqlite lives beside a fresh index
    folder of its own (the store is the scan's, the module's mutations are its story)."""
    index = tmp_path_factory.mktemp("faces-index")
    app = LiveApp(_cfg(index, (faces_corpus,)),
                  config_dir=str(tmp_path_factory.mktemp("faces-config"))).start()
    handle = Faces(app)
    try:
        yield handle
    finally:
        handle.close()


# ── the consent wall ──────────────────────────────────────────────────────────

def test_status_reports_the_faces_state_before_any_consent(faces):
    reply = faces.client.get("/api/v1/faces/status")
    assert reply.status_code == 200, reply.text
    body = reply.json()
    assert body["ok"] is True
    assert body["consented"] is False
    assert body["record"] is None
    assert body["counts"] == {"faces": 0, "assigned": 0, "persons": 0, "named": 0, "pending": 0}
    # The pinned user-supplied packs (FaceModelFiles), presence read off the models folder:
    # the People tab offers the consent-gated download only against this list.
    names = [m["name"] for m in body["models"]]
    assert names == ["scrfd_10g_bnkps.onnx", "w600k_mbf.onnx", "w600k_r50.onnx",
                     "adaface_ir_18.onnx", "adaface_ir_50.onnx", "adaface_ir_101.onnx"], names
    missing = [m["name"] for m in body["models"] if not m["present"]]
    assert not missing, f"the suite needs the pinned face packs in the models folder, absent: {missing}"


@pytest.mark.parametrize("method,path,body", [
    ("post", "/api/v1/faces/scan", {}),
    ("get", "/api/v1/faces/persons", None),
    ("get", "/api/v1/faces/person/1", None),
    ("get", "/api/v1/faces/pending", None),
    ("get", "/api/v1/faces/similar?face=1", None),
    ("post", "/api/v1/faces/name", {"person": 1, "name": "x"}),
    ("post", "/api/v1/faces/merge", {"into": 1, "from": 2}),
    ("post", "/api/v1/faces/delete", {"person": 1}),
    ("post", "/api/v1/faces/confirm", {"face": 1, "person": 1}),
])
def test_every_faces_verb_refuses_before_consent(faces, method, path, body):
    # The wall is in front of EVERY faces surface — reads included — and in front of any
    # daemon spawn: 400 with the engine's sentence, naming the target and the way to open it.
    reply = getattr(faces.client, method)(path, json=body) if body is not None \
        else getattr(faces.client, method)(path)
    assert reply.status_code == 400, reply.text
    detail = reply.json()["detail"]
    assert CONSENT_SENTENCE in detail, detail
    assert "/api/v1/faces/consent" in detail, detail
    # Nothing was built behind the wall: no store, no counts, and consent itself reads closed.
    assert (faces.index / "face.sqlite").exists() is False
    assert faces.client.get("/api/v1/faces/consent").json()["consented"] is False


def test_consent_post_validates_the_affirmative_act(faces):
    # No pre-ticked box, no implicit yes: `accept` must be true.
    refusals = [
        ({}, "accept"),
        ({"accept": False}, "accept"),
        ({"accept": True, "emb_model": "evil.onnx"}, "emb_model"),
        ({"accept": True, "det_model": "w600k_mbf.onnx"}, "det_model"),
    ]
    for body, field in refusals:
        reply = faces.client.post("/api/v1/faces/consent", json=body)
        assert reply.status_code == 422, f"{body} → {reply.text}"
        errors = reply.json()["detail"]
        assert any(e["loc"] == ["body", field] for e in errors), (field, errors)
    assert faces.client.get("/api/v1/faces/consent").json()["consented"] is False


def test_consent_is_a_durable_0600_record(faces):
    reply = faces.client.post("/api/v1/faces/consent", json={"accept": True})
    assert reply.status_code == 200, reply.text
    body = reply.json()
    assert body["consented"] is True
    record = body["record"]
    assert record["consented"] is True
    assert isinstance(record["accepted_at"], (int, float))
    # The tier the acceptance names — the daemon serves exactly this pair (FACES.md's
    # store-scoped model law: one embedder pin per face.sqlite).
    assert record["det_model"] == "scrfd_10g_bnkps.onnx"
    assert record["emb_model"] == "w600k_mbf.onnx"
    path = faces.consent_path()
    assert path.is_file()
    assert os.stat(path).st_mode & 0o777 == 0o600, oct(os.stat(path).st_mode & 0o777)
    assert faces.client.get("/api/v1/faces/consent").json()["record"]["consented"] is True
    # `accepted_at` is an f64, and the file round trip is not value-exact: serde_json's
    # float PARSER is mantissa×10^exp, not correctly rounded (measured 2026-1795337956.9
    # — the POST echo carries the exact f64, the re-read lands 1 ulp off, and its shortest
    # repr loses a digit: `…1621` → `…162`). The repo's cross-process float law (root
    # AGENTS.md, the Swift z-noise row) tolerates 2 ulps on floats and demands exactness
    # on strings/integers — the record's strings pin exact below, its timestamp with a
    # tolerance. A suite demanding bit-exact f64 across a JSON round trip passes by luck.
    read = faces.client.get("/api/v1/faces/consent").json()["record"]
    assert abs(read["accepted_at"] - record["accepted_at"]) < 1e-6, (read, record)
    assert {k: v for k, v in read.items() if k != "accepted_at"} == \
           {k: v for k, v in record.items() if k != "accepted_at"}, (read, record)
    assert faces.client.get("/api/v1/faces/status").json()["consented"] is True


# ── the scan: the engine's counts, relayed ────────────────────────────────────

def test_scan_relays_the_engines_counts(faces, faces_corpus):
    reply = faces.scan()
    assert reply.status_code == 200, reply.text
    scan = reply.json()["scan"]
    assert scan["files"] == PHOTO_COUNT, scan
    assert scan["faces"] == FACE_COUNT, scan
    assert scan["skipped"] == 0, scan
    assert scan["fresh"] == 0, scan
    assert scan["assigned"] == FACE_COUNT, scan
    # The face store is the scan's own file beside the index — separate from index.sqlite
    # (FACES.md §2: faces never ride the main store's schema).
    assert (faces.index / "face.sqlite").is_file()
    assert (faces.index / "face.sqlite") != (faces.index / "index.sqlite")
    # The scan crawled the SERVER's roots (never a client-supplied path): the rows it
    # recorded live under the deployment's own tree.
    names = {p.name for p in faces_corpus.iterdir()}
    for person in faces.persons():
        assert Path(person["cover_face"]["path"]).name in names, person


def test_rescan_skips_recorded_files(faces):
    # Piggyback freshness (FACES.md §3, the scans table): a file recorded at its exact
    # (path, size, mtime) is not re-decoded — the rescan decodes 0 files, skips 12 as
    # fresh, finds no new face, and assigns nothing. The faceless photos are the point:
    # they cost the full decode exactly once (schema 2's fix, measured 2026-11-22).
    reply = faces.scan()
    assert reply.status_code == 200, reply.text
    scan = reply.json()["scan"]
    assert scan["files"] == 0, scan
    assert scan["fresh"] == PHOTO_COUNT, scan
    assert scan["faces"] == 0, scan
    assert scan["skipped"] == 0, scan
    assert scan["assigned"] == 0, scan


def test_status_after_a_scan_names_the_engines_counts(faces):
    body = faces.client.get("/api/v1/faces/status").json()
    assert body["consented"] is True
    counts = body["counts"]
    assert counts["faces"] == FACE_COUNT, counts
    assert counts["persons"] == FACE_COUNT, counts   # the measured singletons
    assert counts["pending"] == 0, counts
    assert counts["named"] == 0, counts
    assert counts["models"]["emb"] == "w600k_mbf.onnx", counts


# ── the People tab's cards: rows, handles, previews ───────────────────────────

def test_person_cards_carry_cover_faces_with_media_handles(faces):
    persons = faces.persons()
    assert len(persons) == FACE_COUNT, persons
    for person in persons:
        assert person["id"] > 0
        assert person["name"] is None            # unnamed cluster: the UI shows "Person NN"
        assert person["count"] == 1              # the measured fixture: 3 singletons
        cover = person["cover_face"]
        assert cover is not None, person
        assert Path(cover["path"]).name in FACE_PHOTOS, cover
        assert len(cover["box"]) == 4 and cover["box"][2] > cover["box"][0], cover
        assert len(cover["kps"]) == 5, cover     # the engine's landmarks, for the crop
        assert cover["width"] > 0 and cover["height"] > 0, cover   # the crop map (schema 3)
        assert 0.0 < cover["score"] <= 1.0, cover
        assert cover["media_id"], cover
    ids = {p["cover_face"]["media_id"] for p in persons}
    assert len(ids) == FACE_COUNT, "each card carries its own minted handle"


def test_cover_faces_preview_through_the_existing_media_handle(faces):
    for person in faces.persons():
        media_id = person["cover_face"]["media_id"]
        reply = faces.client.get(f"/api/v1/media/{media_id}/preview", params={"pixels": 256})
        assert reply.status_code == 200, reply.text
        assert reply.headers["content-type"] == "image/jpeg", reply.headers
        assert len(reply.content) > 1000, len(reply.content)
    # The handle is the PHOTO — the frontend crops with the row's box/kps (zero crop verbs).
    # A handle the store never minted is 404, the media surface's own law.
    assert faces.client.get("/api/v1/media/deadbeefcafe/preview", params={"pixels": 256}).status_code == 404


def test_person_show_lists_its_faces_with_handles(faces):
    for person in faces.persons():
        pid = person["id"]
        reply = faces.client.get(f"/api/v1/faces/person/{pid}")
        assert reply.status_code == 200, reply.text
        faces_rows = reply.json()["faces"]
        assert len(faces_rows) == person["count"], faces_rows
        for row in faces_rows:
            assert Path(row["path"]).name in FACE_PHOTOS, row
            assert row["person"] == pid, row
            assert row["media_id"], row
            assert len(row["box"]) == 4 and len(row["kps"]) == 5, row
            # The crop map (face.sqlite schema 3): box/kps are source pixels and the
            # preview is a scaled render — the client crops with the row's own dims.
            assert row["width"] > 0 and row["height"] > 0, row


def test_similar_ranks_the_stores_faces_by_the_engines_cosine(faces):
    face_id = faces.persons()[0]["cover_face"]["id"]
    reply = faces.client.get("/api/v1/faces/similar", params={"face": face_id, "limit": 3})
    assert reply.status_code == 200, reply.text
    rows = reply.json()["faces"]
    # The engine's brute-force cosine, the store's other faces only (the face itself is
    # excluded), ranked by its own numbers — the server caps `limit`, never re-sorts.
    assert all(row["id"] != face_id for row in rows), rows
    assert len(rows) == FACE_COUNT - 1, rows
    cosines = [row["cos"] for row in rows]
    assert cosines == sorted(cosines, reverse=True), cosines
    assert all(-1.0 <= cos <= 1.0 for cos in cosines), cosines
    assert all(row["media_id"] for row in rows), rows


def test_faces_validation_refuses_bad_ids_bodies_and_limits(faces):
    post = faces.client.post
    checks = [
        # ids: integers > 0, in body and in path alike; the FastAPI 422 list shape.
        (post, "/api/v1/faces/name", {"person": 0, "name": "x"}, ["body", "person"]),
        (post, "/api/v1/faces/name", {"person": 1}, ["body", "name"]),
        (post, "/api/v1/faces/name", {"person": 1, "name": "   "}, ["body", "name"]),
        (post, "/api/v1/faces/name", {"person": 1, "name": "n" * 121}, ["body", "name"]),
        (post, "/api/v1/faces/merge", {"into": 1}, ["body", "from"]),
        (post, "/api/v1/faces/merge", {"into": 0, "from": 1}, ["body", "into"]),
        (post, "/api/v1/faces/delete", {}, ["body", "person"]),
        (post, "/api/v1/faces/confirm", {"face": 1}, ["body", "person"]),
        (post, "/api/v1/faces/scan", {"limit": 0}, ["body", "limit"]),
    ]
    for method, path, body, loc in checks:
        reply = method(path, json=body)
        assert reply.status_code == 422, f"{path} {body} → {reply.text}"
        errors = reply.json()["detail"]
        assert any(e["loc"] == loc for e in errors), (loc, errors)
    assert faces.client.get("/api/v1/faces/person/0").status_code == 422
    assert faces.client.get("/api/v1/faces/person/not-a-number").status_code == 422
    for params in ({"face": 0}, {"limit": 0}, {"face": 1, "limit": 999}, {}):
        assert faces.client.get("/api/v1/faces/similar", params=params).status_code == 422, params


# ── the people verbs: name, merge, delete, confirm ────────────────────────────

def test_name_labels_a_person(faces):
    pid = faces.persons()[0]["id"]
    reply = faces.client.post("/api/v1/faces/name", json={"person": pid, "name": "Angela"})
    assert reply.status_code == 200, reply.text
    assert reply.json()["ok"] is True
    named = {p["id"]: p["name"] for p in faces.persons()}
    assert named[pid] == "Angela", named
    assert faces.counts()["named"] == 1, faces.counts()


def test_pending_queue_is_empty_on_a_clean_cluster(faces):
    reply = faces.client.get("/api/v1/faces/pending")
    assert reply.status_code == 200, reply.text
    body = reply.json()
    assert body["pending"] == []
    assert body["count"] == 0


def test_merge_joins_two_clusters(faces):
    before = faces.persons()
    into, from_ = before[0]["id"], before[1]["id"]
    reply = faces.client.post("/api/v1/faces/merge", json={"into": into, "from": from_})
    assert reply.status_code == 200, reply.text
    after = {p["id"]: p for p in faces.persons()}
    assert from_ not in after, after
    assert after[into]["count"] == 2, after
    # The merged person keeps its name (the label rides the row that survives).
    assert after[into]["name"] == "Angela", after


def test_merge_refuses_an_identity_and_missing_fields(faces):
    pid = faces.persons()[0]["id"]
    reply = faces.client.post("/api/v1/faces/merge", json={"into": pid, "from": pid})
    assert reply.status_code == 422, reply.text
    assert any(e["loc"] == ["body", "into"] for e in reply.json()["detail"]), reply.json()


def test_delete_dissolves_a_person_into_the_pending_queue(faces):
    victim = [p for p in faces.persons() if p["count"] == 1][0]
    face_id = victim["cover_face"]["id"]
    reply = faces.client.post("/api/v1/faces/delete", json={"person": victim["id"]})
    assert reply.status_code == 200, reply.text
    persons = {p["id"]: p for p in faces.persons()}
    assert victim["id"] not in persons, persons
    # Deleting a person is NOT deleting photos: its face row goes to the review queue,
    # with the engine's own candidate cosines as the evidence.
    queue = faces.client.get("/api/v1/faces/pending").json()
    assert queue["count"] == 1, queue
    item = queue["pending"][0]
    assert item["face"]["id"] == face_id, item
    assert item["face"]["media_id"], item
    assert item["candidates"], item
    top = item["candidates"][0]
    assert top["person"] in persons, top
    assert -1.0 <= top["cos"] <= 1.0, top


def test_confirm_answers_a_queued_face(faces):
    queue = faces.client.get("/api/v1/faces/pending").json()["pending"]
    assert len(queue) == 1, queue
    face_id = queue[0]["face"]["id"]
    target = faces.persons()[0]["id"]
    reply = faces.client.post("/api/v1/faces/confirm", json={"face": face_id, "person": target})
    assert reply.status_code == 200, reply.text
    persons = {p["id"]: p for p in faces.persons()}
    assert persons[target]["count"] == FACE_COUNT, persons
    assert faces.client.get("/api/v1/faces/pending").json()["pending"] == []
    counts = faces.counts()
    assert counts["faces"] == FACE_COUNT and counts["persons"] == 1, counts


# ── one writer, answered busy (the index writer's flock law) ──────────────────

def test_a_second_scan_is_answered_busy(faces, faces_corpus):
    # Three new files arrive; the first scan is in the model for ~15 s (3 photos at
    # ~4-5 s/photo). The second request lands inside that window: 409 with the lock's
    # sentence, the first scan finishing as the engine reports.
    for name in sorted(FACE_PHOTOS):
        shutil.copy(faces_corpus / name, faces_corpus / f"copy-{name}")
    with ThreadPoolExecutor(max_workers=1) as pool:
        first = pool.submit(faces.scan)
        time.sleep(1.5)
        busy = faces.scan()
        reply = first.result(timeout=900)
    assert busy.status_code == 409, busy.text
    assert "a faces scan is already running" in busy.json()["detail"], busy.json()
    assert reply.status_code == 200, reply.text
    scan = reply.json()["scan"]
    assert scan["files"] == 3, scan               # only the three new files were decoded
    assert scan["fresh"] == PHOTO_COUNT, scan     # the 12 recorded files skipped as fresh
    assert scan["faces"] == FACE_COUNT, scan      # the copies embed identically to their originals
    assert scan["skipped"] == 0, scan
    # The window closed: the next scan finds the tree quiet, 0 files, 15 fresh.
    after = faces.scan()
    assert after.status_code == 200, after.text
    assert after.json()["scan"]["files"] == 0, after.json()
    assert after.json()["scan"]["fresh"] == PHOTO_COUNT + 3, after.json()
    # The law the suite pins: every face lands in a person, nothing is left queued,
    # and no person vanished. The exact person count is the engine's centroid
    # geometry, not a fixture constant — a byte-copy joins its person only when its
    # cosine to the person's CENTROID reaches the merge threshold, and the centroid
    # of a cluster holding three different people sits near that line for a member's
    # own photo (measured on this box: 2 persons in the 4-photo probe, 3 in this
    # 12-photo run — the merged pair's geometry moves with the crawl order, the
    # rowid artifact the repo notes pin). The merge/confirm verbs exist for exactly
    # that marginal case; the suite does not pretend it is a constant.
    counts = faces.counts()
    assert counts["faces"] == 2 * FACE_COUNT, counts
    assert counts["assigned"] == 2 * FACE_COUNT, counts
    assert counts["pending"] == 0, counts
    assert 1 <= counts["persons"] <= FACE_COUNT, counts


# ── opting out ────────────────────────────────────────────────────────────────

def test_withdrawing_consent_closes_every_verb(faces):
    reply = faces.client.delete("/api/v1/faces/consent")
    assert reply.status_code == 200, reply.text
    body = reply.json()
    assert body["consented"] is False and body["removed"] is True, body
    assert faces.consent_path().exists() is False
    for method, path in (("get", "/api/v1/faces/persons"), ("get", "/api/v1/faces/pending")):
        refused = getattr(faces.client, method)(path)
        assert refused.status_code == 400, refused.text
        assert CONSENT_SENTENCE in refused.json()["detail"], refused.json()
    assert faces.scan().status_code == 400
    # Opting out is not deleting the faces: the store stays on disk, the user's data.
    assert (faces.index / "face.sqlite").is_file()
    # Re-consenting is the same affirmative act, and the store is the same store.
    again = faces.client.post("/api/v1/faces/consent", json={"accept": True})
    assert again.status_code == 200, again.text
    assert faces.counts()["faces"] == 2 * FACE_COUNT, faces.counts()
