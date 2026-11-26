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

from conftest import _cfg, _cli_index
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
    cfg = _cfg(index, (faces_corpus,))
    # Fill the fixture's own index BEFORE serving (measured 2026-11-25): conftest's
    # `_cli_index` fills the shared /index — the plain modules' story — and this fixture
    # owns a fresh index folder, where plain search hits answer []: the People-filter
    # pins below relay membership over the engine's OWN hits, and an unfilled index
    # gives them nothing to relay. Indexing is not a faces verb — the consent wall,
    # the scan's fresh-file counts and the store's face.sqlite are untouched by it.
    _cli_index(cfg, 1800.0)
    app = LiveApp(cfg,
                  config_dir=str(tmp_path_factory.mktemp("faces-config")),
                  # The core law is the engine's default now (FACES.md 2026-11-23: a person
                  # exists from 3 faces up — the daemon gates pin it). This suite pins the
                  # SERVER's relay on the law-off shape (the corpus yields singleton
                  # clusters), and by setting the knob it live-tests the argv chain:
                  # env → faces.rs → faces-service `--faces-min-faces 1`.
                  env_extra={"DIGUP_SERVER_FACE_MIN_FACES": "1"}).start()
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


def test_persons_limit_answers_the_paintable_window(faces):
    # The paintable window (2026-11-23): covers:true pays one cover query per person —
    # 34 s cold for 8,868 people on the live store, over the thin clients' read clock —
    # and the People tab paints at most GRID_CAP cards. `?limit=N` answers the top N by
    # photo count (ties by id), covers minted for the returned rows only; the plain
    # list keeps its exact shape. Validation rides the `similar?limit=` law.
    persons = faces.persons()
    ordered = sorted(persons, key=lambda p: (-p["count"], p["id"]))
    for n in (1, 2, len(persons), len(persons) + 5):
        reply = faces.client.get("/api/v1/faces/persons", params={"limit": n})
        assert reply.status_code == 200, reply.text
        rows = reply.json()["persons"]
        assert len(rows) == min(n, len(persons)), rows
        assert [r["id"] for r in rows] == [r["id"] for r in ordered[:n]], rows
        assert all(r["cover_face"] for r in rows), rows
    # No params = the full list, id order — the shape every persons() pin above reads.
    assert [p["id"] for p in faces.persons()] == [p["id"] for p in sorted(persons, key=lambda p: p["id"])]
    for params in ({"limit": 0}, {"limit": -1}, {"limit": 1_000_001}, {"limit": "abc"}):
        assert faces.client.get("/api/v1/faces/persons", params=params).status_code == 422, params


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


def test_persons_named_answers_the_merge_targets(faces):
    # `?named=1` (the autocomplete list, 2026-11-23): the named people only, photo
    # count first — the names the user has given are the merge targets.
    named = faces.client.get("/api/v1/faces/persons", params={"named": 1}).json()["persons"]
    assert len(named) == 1, named
    assert named[0]["name"] == "Angela", named
    assert all(p["name"] for p in named), named
    assert len(named) == faces.counts()["named"], faces.counts()
    # named=0 answers the full list (the flag filters, it never invents), and a
    # present non-bool is a 422 the faces query law.
    full = faces.client.get("/api/v1/faces/persons", params={"named": 0}).json()["persons"]
    assert len(full) == len(faces.persons()), full
    for params in ({"named": "abc"}, {"named": "2"}):
        assert faces.client.get("/api/v1/faces/persons", params=params).status_code == 422, params


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


def test_person_show_pages_the_gallery(faces):
    # The painted gallery (2026-11-23): `?order=photo` sorts by the photos' own dates
    # newest first, `?limit=&offset=` scrolls the window (each page mints its media
    # handles), no params = every row. A big person's full read is what the app could
    # never render at once (7,294 photos) — the pages are the UI's law.
    target = [p for p in faces.persons() if p["count"] == 2][0]["id"]
    full = faces.client.get(f"/api/v1/faces/person/{target}").json()["faces"]
    assert len(full) == 2, full
    assert all(f["media_id"] for f in full), "every page mints its handles"
    page0 = faces.client.get(
        f"/api/v1/faces/person/{target}", params={"limit": 1, "offset": 0, "order": "photo"}
    ).json()["faces"]
    assert len(page0) == 1, page0
    assert page0[0]["mtime"] == max(f["mtime"] for f in full), "the newest photo leads"
    page1 = faces.client.get(
        f"/api/v1/faces/person/{target}", params={"limit": 1, "offset": 1, "order": "photo"}
    ).json()["faces"]
    assert len(page1) == 1 and page1[0]["id"] != page0[0]["id"], "the pages are disjoint"
    # The window over the whole store answers every row, scan order intact — the
    # MINTED handle is the only difference: each call mints its own media ids
    # (measured 2026-11-25, the suite's own mint law — an `==` over the rows as they
    # come compares per-call ids, which no machine can make equal across two calls).
    # Compare everything the store carries; the handles are pinned present above.
    def strip_handles(rows):
        return [{k: v for k, v in f.items() if k != "media_id"} for f in rows]
    assert strip_handles(faces.client.get(
        f"/api/v1/faces/person/{target}", params={"limit": 9}).json()["faces"]) == strip_handles(full)
    for params in ({"limit": 0}, {"limit": 10001}, {"offset": -1}, {"offset": "x"}, {"order": "date"}):
        assert faces.client.get(f"/api/v1/faces/person/{target}", params=params).status_code == 422, params


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


# ── the search window's People filter (round six, 2026-11-23, the user's design) ─

def test_search_person_filter_relays_the_faces_membership(faces):
    # The search stays PURE: the engine answers its ranking untouched, and the face
    # store answers WHICH of those paths carry the person's face. The server drops
    # whole rows the store does not name — z, kind, score, excerpt ride verbatim,
    # nothing is re-scored (the rows compare field-for-field with the unfiltered
    # answer; the per-call minted media_id is the only difference).
    target = faces.persons()[0]["id"]
    gallery = faces.client.get(f"/api/v1/faces/person/{target}").json()["faces"]
    owned = {f["path"] for f in gallery}
    assert len(owned) == FACE_COUNT, gallery  # one person owns the three face photos
    # The folder token hits EVERY corpus photo's name segment (FTS splits the path):
    # 12 hits, three of them the face photos.
    q = "name: faces"
    plain = faces.client.post("/api/v1/search", json={"q": q}).json()
    plain_paths = {h["path"] for h in plain["hits"]}
    assert len(plain_paths) == PHOTO_COUNT, plain["hits"]
    assert plain_paths > owned, "the face photos are a proper subset of the hits"

    filtered = faces.client.post("/api/v1/search", json={"q": q, "person": target}).json()
    kept = {h["path"] for h in filtered["hits"]}
    assert kept == plain_paths & owned, filtered["hits"]  # exactly the face-photo rows
    plain_rows = {h["path"]: {k: v for k, v in h.items() if k != "media_id"}
                  for h in plain["hits"]}
    for h in filtered["hits"]:
        row = {k: v for k, v in h.items() if k != "media_id"}
        assert row == plain_rows[h["path"]], (row, plain_rows[h["path"]])
    # `person: null` is the pure search — the same rows, the same numbers.
    nulled = faces.client.post("/api/v1/search", json={"q": q, "person": None}).json()
    assert {h["path"] for h in nulled["hits"]} == plain_paths
    # The pydantic law of the field (SearchRequest's declarations put person last):
    # the 422 items carry loc ["body", "person"] and the pydantic kinds.
    for bad in (0, -1, "abc", 1.5, []):
        reply = faces.client.post("/api/v1/search", json={"q": q, "person": bad})
        assert reply.status_code == 422, (bad, reply.text)
        item = reply.json()["detail"][0]
        assert item["loc"] == ["body", "person"], item


# ── one writer, answered busy (the index writer's flock law) ──────────────────

def test_a_second_scan_is_answered_busy(faces, faces_corpus):
    # Nine new files arrive (3 copies of each face photo); the first scan is in the
    # model for seconds-per-photo on every runner. The second request lands inside
    # that window: 409 with the lock's sentence, the first scan finishing as the
    # engine reports. Window sizing is a MEASURED law (2026-11-25): a 3-photo scan
    # on a quiet warm box finished inside the 1.5 s sleep — the second request
    # landed after the lock released and answered an honest 200 with
    # {"files":0,"fresh":15} (main-tip module-scope repro). The window is the
    # FILES' scan cost, so it is sized in files: nine copies run ≥ 4 s warm
    # (~0.5 s/photo measured on ai-ubuntu-computer) and tens of seconds on CI
    # (~4-5 s/photo) — the sleep fits on every machine, with 3× margin.
    for name in sorted(FACE_PHOTOS):
        for n in (1, 2, 3):
            shutil.copy(faces_corpus / name, faces_corpus / f"copy{n}-{name}")
    with ThreadPoolExecutor(max_workers=1) as pool:
        first = pool.submit(faces.scan)
        time.sleep(1.5)
        busy = faces.scan()
        reply = first.result(timeout=900)
    assert busy.status_code == 409, busy.text
    assert "a faces scan is already running" in busy.json()["detail"], busy.json()
    assert reply.status_code == 200, reply.text
    scan = reply.json()["scan"]
    assert scan["files"] == 9, scan               # only the nine new files were decoded
    assert scan["fresh"] == PHOTO_COUNT, scan     # the 12 recorded files skipped as fresh
    # the copies embed identically to their originals: one face each, joining their person
    assert scan["faces"] == 3 * FACE_COUNT, scan
    assert scan["skipped"] == 0, scan
    # The window closed: the next scan finds the tree quiet, 0 files, 21 fresh.
    after = faces.scan()
    assert after.status_code == 200, after.text
    assert after.json()["scan"]["files"] == 0, after.json()
    assert after.json()["scan"]["fresh"] == PHOTO_COUNT + 9, after.json()
    # The law the suite pins: every face lands in a person, nothing is left queued,
    # and no person vanished. The exact person count is the engine's centroid
    # geometry, not a fixture constant — a byte-copy joins its person only when its
    # cosine to the person's CENTROID reaches the merge threshold, and the centroid
    # of a cluster holding three different people sits near that line for a member's
    # own photo (measured on this box: 2 persons in the 4-photo probe, 3 in this
    # 12-photo run — the merged pair's geometry moves with the crawl order, the
    # rowid artifact the repo notes pin). Byte-identical copies merge with each
    # other (cos 1.0), so each marginal original can add AT MOST one new person:
    # with nine copies the bound is the three originals' geometry, and the suite
    # pins it wide (1..2×FACE_COUNT) because it is geometry, not a constant. The
    # merge/confirm verbs exist for exactly that marginal case.
    counts = faces.counts()
    assert counts["faces"] == 4 * FACE_COUNT, counts
    assert counts["assigned"] == 4 * FACE_COUNT, counts
    assert counts["pending"] == 0, counts
    assert 1 <= counts["persons"] <= 2 * FACE_COUNT, counts


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
    # The search's People filter rides the SAME consent wall (round six): the search
    # answers 400 with the faces-wall body — never a silent unfiltered pass.
    refused = faces.client.post("/api/v1/search", json={"q": "name: faces", "person": 1})
    assert refused.status_code == 400, refused.text
    assert CONSENT_SENTENCE in refused.json()["detail"], refused.json()
    # Opting out is not deleting the faces: the store stays on disk, the user's data.
    assert (faces.index / "face.sqlite").is_file()
    # Re-consenting is the same affirmative act, and the store is the same store.
    again = faces.client.post("/api/v1/faces/consent", json={"accept": True})
    assert again.status_code == 200, again.text
    assert faces.counts()["faces"] == 4 * FACE_COUNT, faces.counts()
