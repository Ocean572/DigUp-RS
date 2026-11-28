# DigUp server API (`/api/v1`)

The REST API is the product. The web UI in [`../ui/`](../ui/) is one client of it, served
same-origin at `/`; nothing here depends on the UI. OpenAPI is generated from the app itself at
[`/api/v1/openapi.json`](http://127.0.0.1:8787/api/v1/openapi.json) and a browsable docs page at
[`/api/v1/docs`](http://127.0.0.1:8787/api/v1/docs) — this file is the human reference and the
compatibility promise. `server/tests/test_api_docs.py` fails CI if an endpoint is added without
appearing here.

* Base URL: `http://127.0.0.1:8787` (container: `127.0.0.1:8787:8787` published, host side
  movable with `DIGUP_PUBLISH_PORT`). Requests and responses are JSON UTF-8
  (`content-type: application/json` on POST bodies).
* **One API process.** The media handles ([`media_id`](#media-identifiers)) live in that
  process's LRU cache and the index job runner is a thread inside it. Running more than one
  server process (`--workers >1`, replicas, a load balancer in front of two containers
  sharing a volume) 404s media ids from another process's searches and lets two job runners
  race. Scale by giving the machine more cores, not by adding processes.
* Every answer is the engine's. `hits`, `strong`, `meaning`, `ms`, `warning` pass through
  verbatim; ranking, z-scores and timings are DigUp's, computed by the engine (`crates/` on `main`;
  the Swift twin on branch `swift-reference`), and the
  server adds no cap, no re-ranking and no explanation of its own. Engine failures come back
  as 5xx bodies quoting the engine (with its stderr tail), never as a server-invented message.

## Versioning

`/api/v1` is the version. Additive changes (new optional fields, new endpoints) land in v1;
a field is never renamed, retyped, removed, or given a new meaning in v1 — a breaking change
means `/api/v2` and a deprecation window. Responses are typed with Pydantic; the search reply
model is `extra="allow"`, so an engine field this API has never seen is **published**, not
dropped at the boundary. Clients should ignore unknown fields.

## `GET /api/v1/health`

Engine identity plus live counts. Cheap enough to be the container's `HEALTHCHECK` (30 s).

```sh
curl -s http://127.0.0.1:8787/api/v1/health
```

```json
{"ok": true,
 "engine": {"fingerprints": "google/embeddinggemma-2@bfcd298… | vectors1",
            "build_tag": "b11461", "vectors_version": 1,
            "index": "/data/index", "index_size": "201 KB"},
 "live": {"encoder": "ready", "vectors": 37, "files": 15, "searchable": 15, "pending": 0}}
```

`503` with the engine's own sentence means *the searcher is mid-respawn*: the container is
healthy (the healthcheck treats 503 as alive on purpose). Treat 503 as "try again in a moment",
not as a failure.

## `GET /api/v1/status`

The same live counts plus the engine's per-kind table, its fingerprints, and the server's own
configured roots (deployment config, published for display — the API never accepts paths from a
client). Polled by the UI every 15 s; answers come from a short TTL cache
(`DIGUP_SERVER_STATUS_TTL_S`, default 15) so a poll never costs a `digup status` subprocess.

```sh
curl -s http://127.0.0.1:8787/api/v1/status | jq '.kinds, .roots'
```

## `POST /api/v1/search`

One query, the engine's answer.

| field | type | default | notes |
|---|---|---|---|
| `q` | string | required | max 512 characters (422 above it) |
| `kinds` | array of `screenshot` `image` `pdf` `doc` `audio` `video` `code` | all kinds | anything else → 422 (the engine would silently drop it) |
| `limit` | int 1–100 | **40** | the engine's own default (`SearchService.swift:130`); see [Limits and pagination](#limits-and-pagination) |
| `code` | bool | `false` | the code index (`code:` searches); it is a separate index in the engine |
| `meaning` | bool | `true` | `false` = keyword-only (words, no embedding) — the desktop panel's first keystroke |
| `prefix_last` | bool | `false` | last word as a prefix; the panel's "typing a word that isn't indexed yet" path |
| `name_only` | bool | `false` | **#66 "Names only"**: the daemon seam composes the exact typed `name:` spelling above (same names-only path, no model); a false/absent field is the byte-parity verb, and the reply shape is the ordinary search answer |
| `person` | int ≥ 1 | absent | **the People search (round twelve, 2026-11-27)**: the search is POOLED into that person's photos — the face store's rows for the person are the candidate set, and the engine's z-gate is computed **inside that pool** (modality means, pool mean, deviation all see the pool's rows), so her own standouts stand out instead of being crowded out by the global field. The store's read is consent-gated engine-side and consent-gated here: no accepted consent record for the target → the **400** faces wall, before any daemon call. A person with no photos answers empty (the honest pool, like an empty scope). Never with `code` — faces live on photos, never on code rows → 502-shaped engine refusal relayed. The field is absent in the verb when unset: every pre-round pin stays byte-identical |
| `under` | array of folder paths | absent | **#206/#132/#188 folder scope**: the engine answers only for files under these paths — z's stay the WHOLE field's, rows outside the scope are dropped after ranking (the pinned #178/#189 shape: a keep-scope, not a pool — which is exactly why `person` above is its own field). An empty scope answers no hits |
| `ext` | array of extensions | absent | file-type keep (lowercase, no dot): post-ranking, the engine's ranked rows ride verbatim, the filter KEEPS, it never re-scores |

**Prefixes inside `q`** (the engine parses them, so every client gets them through one path):

| prefix | what it does | where |
|---|---|---|
| `code: …` | the code index, the same as `"code": true` — the engine strips the prefix and answers from the code searcher | `CodeQuery`, `Searcher.search` |
| `name: …` | **file and folder names only**: keyword search over the name segments, no query vector — the engine's encoder never starts for it, and the app's unloaded vectors stay unloaded. A 2-letter word there matches as a name prefix (`name: in` finds "Invoices"); one-letter words are no search | `NameQuery`, `Searcher.search`, `SearchService.swift:138` |

The two are one prefix per query: the engine reads `code:` first, so `"code: name: x"` is a code
search of `name: x`. The server relays `q` verbatim — it adds no rewriting of its own. In the
search bars the typed prefixes are the surface: the **2026-11-27 bar law** retires the `names`
toggle and the folder-scope control — the only extra control right of the kind chips is the
**People** dropdown (`person`, above); `code` stays a box, and `name: …` / `code: …` typed in
the field do what the flags and prefixes do. The verb fields the retired controls rode
(`under`, `ext`) stay published — CLI and API clients keep them.

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/search \
  -H 'content-type: application/json' \
  -d '{"q": "wakeboarding", "limit": 5}' | jq '{strong, meaning, ms, n: (.hits|length)}'
```

```json
{"hits": [{"path": "/media/photos/clips/wakeboarding-vienna.webm",
           "kind": "video", "segment": 3, "segment_id": 41,
           "score": 0.412, "z": 4.15, "cosine": 0.61, "keyword_rank": 2,
           "loc": 9.5, "loc_end": 12.0,
           "modified": 1730000000.0, "size": 1048576, "info": "…",
           "media_id": "9f2c1a7b3e04"}],
 "strong": 3, "meaning": true,
 "ms": {"embed": 1554, "search": 8}, "warning": null}
```

Field notes (engine truth, published as the engine made it):

* `hits[].path` is the server's resolved path for the file — **for display only**. It is not an
  input to any endpoint; media access goes through `media_id` (§ [Media
  identifiers](#media-identifiers)).
* `z` is the engine's significance over the indexed set; a published hit clears `zGate = 2.0`.
  `strong` is how many hits cleared it. **A 1-hit answer is the corpus's size, not a filter, a
  cap, or a bug** — a small gallery yields few ≥2σ matches, a personal library yields many.
* `loc` / `loc_end` are the engine's seconds into an audio/video file; `segment` /
  `segment_id` identify the page, passage, frame, or 30-second window the hit is about.
* `ms` are the engine's own milliseconds. A cold first query pays the model load
  (`embed` of seconds, not milliseconds); after prewarm the same query embeds in hundreds.
* `meaning: false` requests answer without the embedding path; `warning` carries the engine's
  own warning when there is one.
* An empty answer is honest, not an error: `hits: []` with the engine's `strong`/`meaning` as
  it answered. `200` with no hits beats any invented explanation.

## `POST /api/v1/embed_ahead`

The typist's warm-up: the engine embeds and caches the text so far, so the *next* search pays
milliseconds instead of the model load. Send it alongside a `meaning: false` keyword search
while the user is typing, then send the `meaning: true` search when they pause. Same op the
desktop panel uses.

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/embed_ahead \
  -H 'content-type: application/json' -d '{"q": "wakeboa"}'
```

## `POST /api/v1/find-like`

**#162/#121 "Find like this photo"** — the reference is a picture, not words. The client
sends the clicked hit's minted `media_id` (the law that `hits[].path` is never an input — the
server resolves the handle through its HitCache, exactly like `/preview` and `/text` do); the
engine embeds the reference with the **full media encoder** at the budget its stored twin used
(photo 140 / screenshot 280 tokens) and ranks the indexed vector pool by cosine. This is the
search pipeline minus the words: the same **2.0σ z-gate**, the same filter chain, the same
verbatim rows, z-scores and timings.

| field | type | default | notes |
|---|---|---|---|
| `media_id` | string 1–64 chars | required | the clicked hit's handle (a search answer minted it); an **unknown handle is `404`** like the media routes, not a 422 |
| `kinds` | array (as `/search`) | all kinds | kind filter on the answer rows |
| `limit` | int 1–100 | **40** | the engine's own default |
| `under` | array of paths | unscoped | the `/search` scope law, same field |
| `ext` | array of extensions | all | the `/search` extension keep, same field |

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/find-like \
  -H 'content-type: application/json' \
  -d '{"media_id": "9f2c1a7b3e04", "limit": 5}' | jq '{strong, found_from, n: (.hits|length)}'
```

```json
{"hits": [{"path": "/media/photos/IMG_2043.jpg", "kind": "image", "segment": 1,
           "segment_id": 87, "score": 0.512, "z": 3.02, "cosine": 0.71,
           "modified": 1730000000.0, "size": 2097152, "info": {},
           "media_id": "3d91c0a7e512"}],
 "strong": 1, "meaning": true,
 "ms": {"embed": 3850, "search": 12},
 "found_from": {"media_id": "9f2c1a7b3e04", "kind": "image"}, "warning": null}
```

* The **reference never answers its own search**: its own row drops out of the hits (the
  People-tab `similar` law — you don't want the photo you clicked).
* `found_from` echoes the request's handle and the engine's kind for the reference — the
  reference's **local path is never relayed** (the daemon answers it internally; the server
  strips it, the media handle is the public name for it).
* The **first call loads the picture encoder** (the 555 MB media model on top of the text
  model; `ms.embed` of seconds on a cold daemon), after that it stays warm and rides the
  daemon's idle stop with the text model.
* A non-picture handle (video/document/audio) and a dead file answer `hits: []` with the
  engine's own `warning` words relayed — `200` with an honest warning beats an invented answer.
* Few hits on a thin pool is the 2.0σ gate, exactly like text search (§ above).

## `POST /api/v1/estimate`

The engine's own forecast for the configured roots — per folder: readable, kind counts, PDF
pages, audio/video durations, `calibrated` (whether the engine has measured this machine's
speed enough to trust the number), top subfolders. It walks the whole tree, so **one estimate at
a time**: a second concurrent call answers `409`.

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/estimate -H 'content-type: application/json' -d '{}' \
  | jq '.folders[0] | {folder, calibrated, files, seconds}'
```

## `POST /api/v1/index` — start a job (admin operation)

Indexing never starts by itself. An empty body is a plain sync of the server's configured roots;
`{"prune": true}` also retires rows for files no longer under those roots (the engine's
`--prune`) — run it once after remounting media at a new path, so the old scope's ghost rows stop
counting in `files`/`vectors` and diluting z.

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/index -H 'content-type: application/json' -d '{}'
# → 202 {"ok": true, "job": {"id": "1a2b3c4d5e6f", "state": "queued", "started": 1730000000.123}}
curl -s -X POST http://127.0.0.1:8787/api/v1/index -H 'content-type: application/json' -d '{"prune": true}'
# → 409 {"detail": "job 1a2b3c4d5e6f is running"} while a job is in flight
```

`202` means queued and accepted, `409` names the job that is in flight (one writer, enforced by
`flock` beside the index). A job runs in this API process, so it is not durable across a
restart: a `running` job with no ending recovers as **interrupted** on the next boot (never as
success), and the next job resumes the walk.

## `GET /api/v1/jobs` and `GET /api/v1/jobs/{id}`

Job history, replayed from the append-only job log (rotated at size; history outlives any
rotation). States: `queued`, `running`, `done`, `interrupted`, `busy`.

```sh
curl -s http://127.0.0.1:8787/api/v1/jobs | jq '.jobs[-1]'
curl -s http://127.0.0.1:8787/api/v1/jobs/1a2b3c4d5e6f
# unknown id → 404 {"detail": "no job 1a2b3c4d5e6f"}
```

A finished record carries the engine's own counts:

```json
{"job": {"id": "1a2b3c4d5e6f", "state": "done", "started": 1730000000.123,
         "ended": 1730000012.123, "seconds": 12.0, "prune": true,
         "files": 61, "searchable": 61, "pending": 0, "vectors": 65,
         "files_done": 61, "files_failed": 0, "errors": 0}}
```

`files`/`searchable`/`pending`/`vectors` ride the engine's `idle` totals (the post-index truth),
`files_done`/`files_failed` count its `file` events. After a `done` job the searcher is brought
current (`reload` → `prewarm`); if that refresh is refused mid-transition the job stays `done`
with a `why` note ("searcher refresh deferred") — a refused refresh never rewrites a finished
job.

## Media identifiers

`media_id` is the **server's** handle for one hit in one search reply, not a DigUp identifier
and not a path. It is 12 hex characters, issued when a search reply is published, and it lives
in a bounded LRU cache in this API process. Consequences a client must design for:

* Only ids from a published search reply work. An unissued, evicted, or stale id →
  `404 {"detail": "no media handle …"}`. Re-run the search and use the fresh ids.
* Ids are **not durable**: they die with the process (restart, upgrade, reindex-triggered
  reload). Store nothing long-term keyed by `media_id`; re-acquire by searching.
* Ids are **not global**: two searches of the same file issue different ids, and each is
  pinned to that hit's segment and moment. The stable identity a client should key on is
  `path` + `kind` + `segment_id` (+ `loc` for a moment), which the engine publishes.
* No endpoint accepts a path, folder, or URL from a client (§ [Security](#security)). The
  server resolves a handle inside its own configured roots and refuses symlink traversal.

### `GET /api/v1/media/{media_id}/preview?pixels=256`

The JPEG the engine's `render` wrote under `<index>/previews` (photo, PDF page, video frame at
the hit's second). `pixels` is 32–2048, default 256. Answers `image/jpeg`; `404` for a dead
handle; `500` if the engine ever answered outside the previews folder.

```sh
curl -s -o p.jpg "http://127.0.0.1:8787/api/v1/media/9f2c1a7b3e04/preview?pixels=512"
```

### `GET /api/v1/media/{media_id}/text`

The matched segment's text (PDF page, passage, transcript window, code lines) — the engine's
`text` op for that hit's `segment_id`.

```sh
curl -s http://127.0.0.1:8787/api/v1/media/9f2c1a7b3e04/text | jq -r '.text[:200]'
```

### `GET /api/v1/media/{media_id}/file`

The original bytes: the desktop app's "Open", over HTTP.

* Type comes from the file's **magic bytes**, never its name; a source file named `index.html`
  never arrives as markup. Unknown types are `application/octet-stream`; code/doc go
  `text/plain; charset=utf-8`.
* Media kinds (`image`, `screenshot`, `video`, `audio`, `pdf`) are `inline`; everything else is
  an `attachment` with an RFC 5987 filename. Every answer carries
  `x-content-type-options: nosniff`.
* `Accept-Ranges: bytes` always; `Range: bytes=a-b` answers `206` + `Content-Range`, an
  unsatisfiable range answers `416` with `bytes */size`. This is what makes
  `<video src="/api/v1/media/<id>/file#t=9.5">` seek. Single ranges only; `If-Range` is not
  honored. Open-ended ranges are **capped at 8 MiB** and streamed in 1 MiB chunks — a 20 GB
  clip streams, it is never buffered.
* The type, the size and the span all come from **one open** of the file (`fstat` on the live
  handle), so a replacement mid-request cannot make the headers lie about the bytes; a file that
  vanished is `404 {"detail": "the file is gone (it left the folders)"}`.

```sh
# a video frame at the hit's moment, seekable
curl -s -o /dev/null -w '%{http_code} %{content_type}\n' \
  "http://127.0.0.1:8787/api/v1/media/9f2c1a7b3e04/file"
# a range request, exactly as a media element sends it
curl -s -D - -o /dev/null -H 'Range: bytes=0-1023' \
  "http://127.0.0.1:8787/api/v1/media/9f2c1a7b3e04/file"
# → 206, content-range: bytes 0-1023/124177, accept-ranges: bytes
```

## Faces — the People tab (consent-gated)

Face recognition is **off by default**: no face model ships in any payload, and no endpoint
answers until the deployment records an affirmative acceptance. The record lives in the
server's config folder — `faces-consent.json` for the main media index,
`faces-consent-<name>.json` for a named target, mode `0600` — and every faces endpoint
(including the reads) answers `400 {"detail": "face recognition is not consented for target
'media' …"}` until it exists. The engine enforces the same file daemon-side
(`faces-service --faces-consent`): the bearer admin check fires **before** the consent
guard on the mutating routes (measured 2026-11-22, pinned by `test_mcp.py`): a
read-scope key on `/faces/scan` answers the bearer `403` SCOPE body even on an
unconsented deployment; the consent `400` is what admin/inert paths see. The MCP
front-end enforces the same principal rule at the tool edge (a tool call skips the
middleware — the front-end quotes the gate's sentence for the same principal).
All face facts come
from `digup faces-service` (`scan/cluster/persons/name/merge/delete/confirm/pending/similar`
over JSON-lines, FACES.md); the server relays them and invents nothing. Face rows carry the
engine's `box`/`kps` and a minted `media_id` for their photo — the frontend crops the face out
of the existing [`/preview`](#get-apiv1mediamedia_idpreviewpixels256) with those numbers;
there are no crop verbs and no crop files.

### `GET /api/v1/faces/status`

```sh
curl -s http://127.0.0.1:8787/api/v1/faces/status
# → 200 {"ok": true, "consented": false, "record": null,
#        "models": [{"name": "scrfd_10g_bnkps.onnx", "present": true}, …],
#        "counts": {"faces": 0, "assigned": 0, "persons": 0, "named": 0, "pending": 0}}
```

`counts` are the engine's (`0` before any scan); `models` names the pinned user-supplied packs
and whether each is in the models folder — the People tab offers the model download only for
a consented target and pins it by sha256.

### `GET/POST/DELETE /api/v1/faces/consent`

```sh
curl -s http://127.0.0.1:8787/api/v1/faces/consent
# → 200 {"ok": true, "consented": false, "record": null}
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/consent \
  -H 'content-type: application/json' -d '{"accept": true}'
# → 200 {"ok": true, "consented": true,
#        "record": {"consented": true, "accepted_at": 1730000000.5,
#                   "det_model": "scrfd_10g_bnkps.onnx", "emb_model": "w600k_mbf.onnx"}}
curl -s -X DELETE http://127.0.0.1:8787/api/v1/faces/consent
# → 200 {"ok": true, "consented": false, "removed": true}
```

`POST` is the affirmative act (admin operation): `accept` must be `true`, and the tier names
must be pinned packs (`emb_model` one of `w600k_mbf.onnx`, `w600k_r50.onnx`,
`adaface_ir_18.onnx`, `adaface_ir_50.onnx`, `adaface_ir_101.onnx`; `det_model`
`scrfd_10g_bnkps.onnx`) — anything else answers `422`. The record is written `0600`, atomically.
`DELETE` withdraws consent (endpoints go back to `400`); the face store is **not** deleted —
mass deletion is the user's act, never a side effect.

### `POST /api/v1/faces/scan` (admin operation)

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/scan -H 'content-type: application/json' -d '{}'
# → 200 {"ok": true, "scan": {"files": 5, "faces": 5, "skipped": 0, "fresh": 0, "assigned": 5}}
# → 409 {"detail": "…: a faces scan is already running"} while one is in flight
```

The scan crawls the **target's own roots** (the deployment's config — no path is accepted from
a client, § [Security](#security)), decodes each image not yet recorded at its exact
`(path, size, mtime)` (piggyback freshness), runs detection + embedding, and relays the
engine's counts. Synchronous on the engine's clock: detection is ~4.4 s/photo on CPU, so a big
tree is hours — the ceiling is `DIGUP_SERVER_FACE_SCAN_TIMEOUT_S` (3600). One scan at a time
(`flock` on `.digup-server.faces.writer.lock` beside the index, the index writer's law).
Clustering runs at the scan's tail; the knobs are `DIGUP_SERVER_FACE_MERGE` (0.5) and
`DIGUP_SERVER_FACE_COLLISION` (0.10), and the core law `DIGUP_SERVER_FACE_MIN_FACES` (3 — a
person exists from this many faces up, lower counts stay in the review queue; Immich's
`minFaces` law) with the record-side detection floor `DIGUP_SERVER_FACE_MIN_SCORE` (0.5;
Immich runs 0.7).

The scan reply's fields are the engine's, in its own words: `files` = files decoded by **this**
scan; `faces` = faces this scan recorded; `skipped` = files whose decode failed (a failed file
is not recorded, so a later scan retries it); `fresh` = files the store had already recorded at
their exact `(path, size, mtime)` — the rescan of a quiet tree answers
`{"files": 0, "faces": 0, "skipped": 0, "fresh": N, "assigned": 0}`: the decode is paid once
per file, faceless photos included (`face.sqlite` schema 2's `scans` table); `assigned` = faces
the tail clustering joined into persons.

### `GET /api/v1/faces/persons`

```sh
curl -s http://127.0.0.1:8787/api/v1/faces/persons
# → 200 {"ok": true, "persons": [{"id": 3, "name": "Angela", "count": 2,
#         "cover": 0.9999, "cover_face": {"id": 5, "path": "/…/photo.jpg",
#         "box": [736.5, 250.9, 1256.3, 904.6], "kps": [[…], …],
#         "width": 1600, "height": 1067, "score": 0.884, "media_id": "07cbe86e5775"},
#         "cover_render": "/…/faces/<home>/covers/3.jpg", "cover_media": "1a2c…"}]}
```

Person cards: `name` is `null` for an unnamed cluster (the UI shows "Person NN");
`cover_face` is the person's best-scoring face. `box`/`kps` are **source pixels** and
`width`/`height` are the photo's own pixels — the crop map: scale the box by
`pixels / max(width, height)` and crop it out of that face's `preview` handle. A row
recorded before face.sqlite schema 3 carries `width: 0` — the honest fallback (the card
shows the uncropped photo; a re-scan fills the map).

**The pre-rendered cover card (round eleven, 2026-11-27).** `cover_render` is the engine's
card file, stored inside the faces home beside `face.sqlite` (`<config>/faces/<home>/covers/
<id>.jpg`): the scan fills the cards as a byproduct at the cluster tail (the photos it
decodes are the decodes it pays for detection), and unload / delete / merge / confirm sweep
the card files with the rows they dissolve — no store reset is ever needed, the first Scan
press fills cards for a store that has none. When the server can read the card it mints a
`cover_media` handle like every media row, and `GET /api/v1/media/{id}/file` serves it with
**zero render** — the People grid reads plain files. The card is faces data: it rides the
consent wall (only the persons relay mints it, behind the consent guard), not the roots
wall; the `/file` exception is the cards folder and nothing wider.

`?limit=N` answers the paintable window: the top N people by photo count (ties by id),
covers included, and the cover is paid for the returned rows only. With the cards filled
the read is a file read (the 34 s cold cover-query cost measured pre-round-eleven is what
the cards remove); a card the store has not filled yet pays the engine's one cover query
for that row, and the People tab paints at most its grid cap — the window is exactly what
the client paints. Absent `limit` answers every person in the store's id order (the full
list, the shape this route has always answered). A present limit outside 1..=1000000 is
a 422, the `similar?limit=` law restated.

### `GET /api/v1/faces/person/{id}`

The person's face rows (same row shape as `cover_face`, each with its minted `media_id`).

`?order=photo` sorts by the photos' own dates newest first (`mtime`, id on ties);
`?limit=&offset=` walks the painted window the UI scrolls. The gallery's law (2026-11-23):
a mega-cluster's full read (7,294 rows measured 4.2 s + every row rendered) is what no
panel paints at once — a 50-row page answers in 0.07 s on the live store. Absent params
answer every row (the shape this route has always answered). Validation: limit
1..=10000, offset ≥ 0, order ∈ `photo`/`scan` — the faces query law.

`?named=1` on `GET /api/v1/faces/persons` answers the **named people only**, photo count
first — the autocomplete list of the People tab's name box (picking one of these names on
another card is the human's merge act). `named=0`/absent answers the full list.

### `POST /api/v1/faces/name` · `merge` · `delete` · `unload` · `confirm` (admin operations)

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/name   -d '{"person": 3, "name": "Angela"}' -H 'content-type: application/json'
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/merge  -d '{"into": 3, "from": 4}' -H 'content-type: application/json'
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/delete -d '{"person": 1}' -H 'content-type: application/json'
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/unload -d '{}' -H 'content-type: application/json'
curl -s -X POST http://127.0.0.1:8787/api/v1/faces/confirm -d '{"face": 5, "person": 3}' -H 'content-type: application/json'
# → 200 {"ok": true} on each; `unload` answers {"ok": true, "persons": N, "faces": M}
```

Naming is a label. `merge` joins two clusters. `delete` dissolves a person — its faces go to
the pending queue (deleting a person is **not** deleting photos; the faces re-cluster).
`unload` is the bulk `delete` over the whole unnamed group: every person without a name is
dissolved, their faces requeue, the named people stay — the next scan re-clusters onto the
names (the engine relays its own counts, `{"ok": true, "persons": N, "faces": M}`).
`confirm` is the human answer to a collision: a queued face joins the named person and is
pinned there. `name` is 1–120 characters; ids are integers > 0; bad bodies answer `422`.
These mutating acts also sweep the pre-rendered cover-card files with the rows they dissolve
(round eleven) — the cache follows the store, and a scan fills fresh cards at the cluster tail.

### `GET /api/v1/faces/pending` and `GET /api/v1/faces/similar?face=N&limit=M`

`pending` is the collision guard's review queue: each queued face with the engine's top
candidate persons and their cosines (the evidence, not an invented score). `similar` ranks
the store's faces against one face's vector (brute-force cosine, the engine's own numbers),
`limit` 1–200 default 20 — the People tab's "more like this".

Faces ride the bearer gate like every other endpoint: the reads answer a `read` key, the
consent writes, `scan`, `name`, `merge`, `delete` and `confirm` demand the admin key
(`ADMIN_ROUTES`); an `app:<name>` token works only on its own target.

## Limits and pagination

`limit` is the whole pagination story: 1–100 per search, default 40 (the engine's own default,
so a client that names no limit gets the engine's answer — `tests/test_api.py` pins it). There
is **no offset and no cursor**: the engine ranks the entire index and returns the top `limit`
hits that clear its z-gate. To see more of a large library, narrow the query and re-search
(`kinds`, `code`, `meaning: false` for the keyword pass, a more specific phrase) — paging into
weak matches is not what the ranking is for. The web UI's row-count select sends `limit`
explicitly; direct API clients get 40.

## Errors

All errors are JSON with a single human-readable `detail` string, safe to show to a user:

| status | when | example body |
|---|---|---|
| `422` | validation (bad kind, over-512 `q`, `limit` out of 1–100, missing `q`) | the validation layer's list of error objects — render each item, do not show the raw array |
| `404` | unknown job id, dead media handle, file outside the configured roots, file gone | `{"detail": "no job 1a2b3c4d5e6f"}` |
| `409` | a job is already running; an estimate is already running | `{"detail": "job 1a2b3c4d5e6f is running"}` |
| `416` | unsatisfiable `Range` | empty body, `content-range: bytes */size` |
| `502` | the CLI (`status`/`estimate`/`speed`) failed | the engine's own message + its stderr tail |
| `503` | the engine refused (`ok:false`), the searcher exited, `/health` mid-respawn | the engine's own sentence, quoted |
| `504` | an adapter call timed out | the timeout the adapter hit |

`503` is not a server fault code here: it is the engine saying "not right now, in its own words".
Retry it. A `500` means a server bug — report it, and it ships with a test.

## Security

* Binds `127.0.0.1:8787` by default (`DIGUP_SERVER_HOST`/`DIGUP_SERVER_PORT` for bare metal; in
  a container the server binds `0.0.0.0` inside and the `127.0.0.1:8787:8787` publish line is what
  enforces localhost). LAN exposure needs authentication first — do not publish the port on a
  routable interface unauthenticated.
* **Host and Origin validation is opt-in**: `DIGUP_SERVER_ALLOWED_HOSTS` names the URL hosts
  this deployment serves. Unset (the default) validates nothing, so loopback behavior never
  changes. When set: a request whose `Host` is not listed gets `400`, and a browser's
  state-changing request (`POST`/`PUT`/`PATCH`/`DELETE`) whose `Origin` is not that same host
  gets `403`. Non-browser clients send no `Origin` and pass; read-only `GET`s are not checked;
  ports are not compared; there is no wildcard. Set it the moment the server is reachable from a
  Docker network, a reverse proxy, or a LAN name — that is the setting that stops DNS-rebinding
  against a localhost service. `tests/test_security.py` pins all of it.
* No CORS allowance at all: the UI is served by the API on the same origin.
* No settings endpoints: the server's configuration is container configuration
  ([`.env.example`](../.env.example)), not a remote-writeable surface. No uploads, no
  client-chosen paths, no shell interpolation of input.
* Search is a read-only operation. Indexing, pruning and the full-tree estimate are
  administrative — see [Authentication](#authentication): the deployment splits them onto the
  admin token, so a search-only client cannot start a job.
* HTTPS is the reverse proxy's job (`nginx`/`caddy` terminating TLS in front of
  `127.0.0.1:8787`), not the API's: keep the API on loopback and proxy from there.

## Authentication

Off by default: a loopback-only server has no auth state to leak, and nothing here changes its
behavior. The deployment turns it on with environment variables
([`.env.example`](../.env.example)) — tokens are never in the repo, never in the image:

| variable | scope | what it guards |
|---|---|---|
| `DIGUP_SERVER_TOKEN` | read | every `/api/v1` endpoint except `health`, `docs`, `openapi.json` |
| `DIGUP_SERVER_ADMIN_TOKEN` | admin | `POST /api/v1/index` and `POST /api/v1/estimate`, on top of the read scope |
| `DIGUP_SERVER_READ_TOKENS` | read (named) | `name:token` pairs, comma-separated: **each entry guards the read scope**, exactly like `DIGUP_SERVER_TOKEN`, and the access log names the entry that asked (`principal=nc-a`). The separator is the LAST colon, so names may carry colons — the target-bound form `app:<target>` guards that target's index only (see "Named indexes") |

* No credential on a guarded endpoint → `401` with `WWW-Authenticate: Bearer`. A token that is
  not this server's → `401`. The **read** token on an admin endpoint → `403` saying so.
* Send it as `Authorization: Bearer <token>`. Clients that must not mutate the index (search
  apps, agents, dashboards) get the read token and nothing else; keep the admin token with the
  operator. An **admin-only** deployment (no `DIGUP_SERVER_TOKEN`) guards the mutating endpoints
  and leaves the read side open for every client on the machine.
* The **named map** is for a server answering several deployments (several Nextcloud instances,
  dashboard tiles, Immich): one secret per client, revocation per client, attribution per client.
  Names are lowercased, the first value wins per name, and a malformed entry is skipped with a
  named warning in the log — a silently-dropped token is an outage nobody asked for. The admin
  token covers the read scope too (an admin client searches like a read client); its log line
  says `admin (read scope)` when it presents on the read side.
* `GET /api/v1/health`, `/api/v1/docs` and `/api/v1/openapi.json` are deliberately unguarded:
  the container's `HEALTHCHECK` calls `/health` with no credential (a guarded health endpoint
  would mark a live container unhealthy), and the API's own documentation page is documentation,
  not media data.
* The web UI keeps the key this browser sends in `localStorage` (`digupBearer`) and sends it
  on every request. There is no paste field: in the **tokenless** shape the main page
  searches with no key at all; in the **minted-key** shape (applications registered, see
  "Named indexes") the gear's **API key** section shows the server-generated keys with
  **Copy** (for pasting into the applications) and **Regenerate** — Regenerate also adopts
  the new admin key for this browser. In the **environment-keys** shape the keys are the
  deployment's config, shown by nothing, and the `401` message says exactly that. A `401`
  with a stored key reports the server's words, so a wrong key cannot loop the page.
  (The embed widget, which has no settings panel of its own, keeps its prompt.)
* Tokens are compared with `hmac.compare_digest`. `tests/test_security.py` pins the whole table
  above, including the open health/docs/UI paths.

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/search \
  -H 'content-type: application/json' -H "Authorization: Bearer $DIGUP_READ_TOKEN" \
  -d '{"q": "wakeboarding"}'
curl -s -X POST http://127.0.0.1:8787/api/v1/index \
  -H 'content-type: application/json' -H "Authorization: Bearer $DIGUP_ADMIN_TOKEN" \
  -d '{"prune": true}'
```

## CORS for browser widgets (`DIGUP_SERVER_ALLOWED_ORIGINS`)

Off by default and needed by neither the server's own UI (same origin, `/`) nor server-to-server
clients (PHP, curl, agents: browsers do not attach `Origin` to their fetches, so nothing here
constrains them). It exists for **in-browser widgets hosted by other applications** —
`GET /embed.html`, the compact search page this server serves (docs/implementation/
stage-12-integrations.md §11.4), embedded by a host app (a Nextcloud app page, an Immich
workspace panel):

* `DIGUP_SERVER_ALLOWED_ORIGINS` is a comma-separated list of **exact origins**
  (`http://nextcloud.lan:8081`, scheme+host+port): no wildcards, no path, no trailing slash.
* A preflight (`OPTIONS` with an allowlisted `Origin`) answers 204:
  `access-control-allow-origin: <origin>`, `allow-methods: POST, GET, OPTIONS`,
  `allow-headers: authorization, content-type`, `max-age: 600`, `vary: Origin`.
* A request from an allowlisted origin gets its response decorated with
  `access-control-allow-origin` + `vary: Origin`; any other origin gets nothing — the
  cross-origin read is refused by the browser exactly where this list says.
* The Host/Origin guard's cross-origin-POST rule keeps its teeth: the widget's POST passes
  only because CORS (installed outside the guard) blessed that exact origin first, and the
  Host allowlist still outranks everything. Non-allowlisted browsers keep the §7 rule.
* The embed widget carries the read token in `?token=`, which moves to `sessionStorage` and
  leaves the URL line (history/bookmarks/host logs never see it); previews are authorized
  blob fetches, never `<img src?token=…>`.

## Multiple clients on one index

One deployment answers many applications (Immich, Home Assistant, agents, desktop tools) from one
index and one model deployment: point them all at this API, index once, search concurrently. The
engine holds one hot `search-service` per API process (its query encoder stays warm for
`DIGUP_SERVER_ENCODER_IDLE` seconds and the adapter restarts it transparently), one writer under
the flock, and one job queue. Search is safe to hammer; index jobs are serialized and `409` while
one runs. Keep every client on the same root layout (an index folder belongs to one set of root
paths — remounting media under a new path needs one prune job), and never point two root layouts
at the same index folder.

## Named indexes — per-application indexes (`DIGUP_SERVER_APP_TARGETS`)

One server, many indexes. The deployment's own media index (`media`, the roots+index the
environment names) is what the UI and `/api/v1/*` answer by default — the standalone server,
exactly as before. An **application target** is a named pair of (roots, index folder) the same
server supervises *independently*: its own `search-service` child, its own writer (the flock
lives beside each index folder, so crawls on different indexes never queue behind each other),
its own jobs log (`jobs-<name>.jsonl`), its own previews folder, its own token. A Nextcloud
bridge pointing at this API with its target's token builds and searches **its own index** —
the media index is untouched by its crawl, and a second application gets its own target the
same way.

```
DIGUP_SERVER_APP_TARGETS=nextcloud=/media/nextcloud:/data/index-nextcloud;dc=/media/dc:/data/index-dc
```

Entries are `;`-separated, each is `name=roots:index` (the roots colon-separated, the index
the LAST folder — the same colon shape as `DIGUP_SERVER_ROOTS`); names are
lowercase slugs (`media` is reserved for the main index), paths absolute, and a target's
index may not be the main index — separate indexes are the point. A persisted
`<config>/targets.json` joins the registry the same way; env wins per name. Malformed
entries refuse startup with the entry quoted.

**Minted keys — the server's "API key" rows.** When targets exist and their tokens are not
configured, the server mints them (`secrets.token_urlsafe(24)`), stores them in
`<config>/tokens.json` (0600, config folder = recreation-safe: a recreation never silently
revokes a connected application), and shows them in the UI (gear → Settings → **API key** —
the server's key is one row: **API key | value | Copy | Regenerate**) and via
`GET /api/v1/tokens`. **In a minted deployment the keys surface is key-free** (2026-11-08
UX brief, round 6 — the ticket model): the gear reads the row with no key in the browser,
exactly as the main page searches with no key. An `app:` key and an env-keyed deployment are
refused there (`403`, admin scope only — the env-shape's gate keeps 401-ing every call).
**The gear takes the key it is shown** (`app.js`): in a minted deployment the operator's own
panel runs the admin-scope controls (`index`, `estimate`, `tokens` rotate) with the displayed
value, stored in `localStorage` exactly as rotation already stores its new key; a stored key the
server no longer recognizes is replaced by the value shown now (the panel self-heals after a
rotate or re-mint). Nothing is typed, pasted, or prompted, and the main page's key-free reads
are unaffected.
Minting fires only when targets exist; a standalone, token-less deployment keeps running
unguarded and unchanged. **The minted shape keeps the main index's READS open** (2026-11-08
user brief — "the standalone app just uses the directory set in the environment"): a key-less
browser on the operator's box searches, previews, and reads status on the main index; the
minted **admin** key guards the index operations (`index`, `estimate`), the keys surface, and
`Regenerate`; each minted **app** key guards its own index. A deployment that typed its keys
in the environment is the guarded shape — there, every call needs a key, as before. The admin
key is read from the file on the operator's own config mount (`cat <config>/tokens.json`) when
this browser must administer the server (index/estimate/regenerate); ordinary browsing needs
no key. Environment-configured secrets are never copied into the file and never echoed by
the API — `/api/v1/tokens` shows `configured: true` for them and values only for minted ones.

`POST /api/v1/tokens` (admin scope) **regenerates** the minted keys — the UI's Regenerate
button, behind a confirm ("the applications that have these keys pasted lose access"):
every minted value (admin and app keys) is freshly minted, persisted 0600, and live
immediately — the old values answer `401` from this call on. Environment-configured keys
are never rotated (a rotation must not silently override the deployment's own config); a
deployment with nothing minted answers `400`.

**The standalone box mints its key at boot (`DIGUP_SERVER_MINT_KEYS=1`).** With the knob the
server mints its admin key with no application registered — the gear shows it with **Copy**,
and the box stays searchable with no key (the minted shape's open main reads). This is the
ticket the applications bring: `POST /api/v1/targets` (admin key) with
`{"name": "<slug>", "roots": ["<abs>", …], "index": "<abs, optional>"}` registers the
application at the **running** server — the startup validator runs at runtime (slug rules,
absolute paths, "not the main index", name reserved `media`, duplicates `409`), the server
mints the application's **private key** (returned once here, persisted 0600), gives it its own
index folder (default `<config>/index-<name>`), and persists the registration
(`<config>/targets.json` — a restart re-reads both files). From that call on the application's
key answers only its target: its own status, search, previews, and its own crawl
(`POST /api/v1/index` — background, one writer, never touching the main index, invisible to
the main page). Key-less and application-keyed requests cannot register.

**Per-target skip lists (#196, 2026-11-25).** `POST /api/v1/targets` accepts two more
fields: `exclude` and `exclude_ext` (arrays of folder paths / extensions, optional, default
none) — the target's OWN skip lists. The engine's law makes them per-root by construction:
the server spawns the target's crawl and search with the GLOBAL env lists
(`DIGUP_SERVER_EXCLUDE[_EXT]`) plus the target's own, and the engine applies each entry
only inside the roots that contain it — a skip for one target never reaches another.
`GET /api/v1/targets` publishes both lists per row (display, like `roots`); a runtime
registration survives a restart (the lists persist in `targets.json`), and a
`DIGUP_SERVER_APP_TARGETS` override of the same name keeps the registered lists (env
carries roots/index only). The main index keeps the env lists alone — `targets.json`
describes application targets.

**Endpoint scoping.** Every guarded endpoint addresses a target: `?target=name` names it
explicitly; the default is `media` — except for an `app:<name>` token, whose default is its own
target (the binding is what the token carries, so an application calls plain paths). The bearer
gate binds: `admin` answers every target; `DIGUP_SERVER_TOKEN` and plain
named tokens answer the main index (their contract, unchanged); an `app:<name>` token answers
its target's index and nothing else — any other target answers `403`. Mutating routes
(`index`, `estimate`) accept an app token **for its own target**: the bridge queues its own
crawl, and never anyone else's. One estimate runs at a time **server-wide** (a full-tree crawl
is the expensive walk, wherever its roots are); one index job at a time is **per target** —
the writer lock lives beside each index folder, so crawls of different indexes run in
parallel. `GET /api/v1/targets` lists the registry: counts come from a running target's
live searcher (the authority on a store it has open), from the read-only CLI status for a
target whose searcher is not spawned (no engine spawn for a listing), and are `null` for an
index folder with no store yet; an app token sees only its own. Media handles resolve their
target from the path's owning roots, so `/preview`, `/text`, and `/file` work identically
across indexes. `GET /api/v1/health` reports the main target (container health is the
deployment's own index). The access log carries the resolution: every audited line names the
principal **and the target it addressed** (`principal=app:nc target=nc`).

Deploy shape: `deploy/docker-compose.app-targets.yml` (opt-in `-f`) mounts the application's
data folder read-only and registers one target; bring-up needs the full chain and env
(see the file's header). `tests/test_targets.py` pins the registry, the minting rules, the
binding table, and the end-to-end flow over the real engine.

## Connect an AI agent (`POST /api/v1/mcp`, and `digup mcp` over stdio)

The server speaks the **Model Context Protocol** (protocol version `2025-06-18`) in two
shapes, and both are the same thin relay: a tool reply carries **the engine's answer
verbatim** in `structuredContent` — the same rows, scores, counts and timings the web UI
reads, with the transport id stripped exactly like REST strips it — beside a short
skimming text block. Nothing in the MCP layer re-ranks, reformats a score, or invents a
number, and the gate is the REST gate: bearer 401s, the read/admin split, the 403
refusal in the gate's own words, target scoping, and the access log's `principal= …`
line cover MCP calls like any other request.

**Stdio** — for Claude Desktop and local agents. The `digup` binary on every desktop
package runs `digup mcp [FOLDER]... [--index DIR] [--allow-admin]`: one JSON-RPC message
per line on stdin, one per line on stdout, on the same folders the search verb reads.
The `index` tool does not exist in this shape (a stdio run has no job runner to reach);
`estimate` is advertised only with `--allow-admin`. Claude Desktop's config block:

```json
{ "mcpServers": { "digup": {
    "command": "/usr/lib/DigUp/helper/digup",
    "args": ["mcp", "--index", "/home/me/digup-index", "/data/photos"]
} } }
```

**HTTP** — for remote and LAN agents: `POST /api/v1/mcp`, the stateless streamable shape
(one JSON-RPC 2.0 message per POST, one JSON answer per POST, an `mcp-session-id` header
echoed on every answer). A notification answers `202` with an empty body; a malformed
body answers `400` with the JSON-RPC parse error. Send the bearer key like every other
guarded endpoint — the **read** key answers `search`, `status`, `show` and `people`; the
**admin** key adds `estimate` and `index` (a read key calling one lands on the gate's own
words, quoted). An inert (tokenless) deployment answers the full tool set key-free, matching
what its REST admin routes answer key-free.

| tool | scope | what it relays |
|---|---|---|
| `search` | read | the `POST /api/v1/search` answer: `hits` (path, kind, segment, score, z, loc, excerpt…), `strong`, `meaning`. `limit` defaults to **10** (max 100 — the engine's own cap) to keep agent context small; `kinds`, `code`, `name_only`, `under`, `ext` ride the REST model; the API surface adds `person` (the People filter) |
| `status` | read | the `GET /api/v1/status` counts, verbatim |
| `show` | read | one segment's stored full text by its `segment_id` (from a hit row) — the search daemon's `text` verb |
| `find_like` | read | **#162/#121**: the `POST /api/v1/find-like` answer relayed. The API surface names the reference by the clicked hit's `media_id` (REST: paths never cross the API); the stdio surface is local and names it by `path`. Same gate, same rows, `found_from` relayed (stdio: with its path, local only) |
| `people` | read (HTTP only) | the faces surface (FACES.md): `action` = `status` (the default) / `persons` / `pending` / `person` (needs `person`) / `scan`. The reads relay the `GET /api/v1/faces/…` answers verbatim; `scan` rides the REST admin wall (a read key lands on 403 words) and runs synchronously on the engine's clock, busy answers the `409` text; every action answers the consent wall's `400` words until the deployment records the acceptance. Stdio has no faces seam (the consent record lives in the server's config), so the tool is invisible there |
| `estimate` | admin | the engine's crawl forecast, `{"folders": […]}` |
| `index` | admin, HTTP only | queue one crawl: the job record (`202` shape); a running writer answers the `409` text, relayed as `isError` |

```sh
curl -s -X POST http://127.0.0.1:8787/api/v1/mcp \
  -H 'content-type: application/json' -H "Authorization: Bearer $DIGUP_READ_TOKEN" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
       "params":{"name":"search","arguments":{"q":"wakeboarding","limit":10}}}'
```

`tests/test_mcp.py` pins the gate shapes, the tool visibility per scope, the
field-for-field equality of a tool reply with the same REST answer, and the 202/400
transport shapes; the CLI suite pins the stdio loop (initialize, tools, a relayed
search, daemon stop at EOF).

## Deploy shape (what a client can assume)

`deploy/docker-compose.yml` (CPU-only, the default) → `http://127.0.0.1:8787/`: UI at `/`,
`/api/v1`, `tini` as PID 1, non-root `uid 990`, `stop_grace_period: 90s`, healthcheck on this
endpoint, index/models/config/job-log on bind-mounted volumes (container recreation never
destroys them). The GPU (NVIDIA) override arrives as a separate opt-in Compose file; CPU-only is
the baseline everything is measured against.
