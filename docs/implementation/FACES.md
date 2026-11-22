# DigUp Native Face Recognition Engine
**Architectural Specification v1.1 (Final & Amended)**

Status: planning — Phase 0 gates open (license audit in flight, `ort`/`tract` spike not run).
Rides branch `feature/native-faces`. Binding gates below are the build order; a phase is not
done until its gate numbers are measured and recorded here.

This specification establishes the blueprint for implementing a native, privacy-first facial recognition and active-learning clustering system inside **DigUp**. All components strictly adhere to house architecture laws, pure-Rust execution principles, and isolated data boundaries.

---

## Phase 0: Research & Licensing Gates (Pre-Code)
Before code execution begins, the following gates must be cleared:

1. **Licensing Audit & Dataset Lineage:** — **GATE RUN 2026-11-21** (independent audit; exact quotes below are from dataset pages/model cards).
   * Dataset license quotes: WIDER FACE (HF card): "Creative Commons Attribution-NonCommercial-NoDerivatives 4.0 International (CC BY-NC-ND 4.0)"; CASIA-WebFace agreement: "Researcher shall use the Database only for non-commercial research and educational purposes"; Glint360K: "The Glint360K dataset (and the models trained with this dataset) are available for non-commercial research purposes only"; WebFace260M: signed-agreement, non-commercial academic; MS-Celeb-1M: research-only, **withdrawn 2019** (MS1MV2/V3 derivatives have no obtainable license text).
   * **Detectors:** YuNet — weights stated MIT per zoo model dir (badge links Apache-2.0 zoo LICENSE; mismatch recorded), lineage WIDER FACE NC-ND → **ship, disclosed**. MediaPipe BlazeFace — Apache-2.0 weights, Google-internal self-collected training → **fully clean**. UltraFace — MIT weights in-repo, lineage WIDER FACE NC-ND → ship, disclosed. SCRFD/det_10g — "ALL models are available for non-commercial research purposes only" → **REJECT for payload; user-supplied only** (dev-side reference baseline allowed). Paddle detectors — Apache-2.0 repo, no per-weight terms → ship only with disclosure.
   * **Embedders:** **No 512-d embedder was found whose published license names a permissive training dataset.** MediaPipe face_embedding (192-d, MobileFaceNet, Apache-2.0, internal data) = the only fully clean pair, weaker accuracy, TFLite (tract tflite frontend is experimental — reference role, not ship target). AuraFace-v1 `glintr100` (512-d, HF apache-2.0 + Apache-2.0 LICENSE.md, card claims "trained on commercially and publicly available data sources", datasets **not enumerated**) = best 512-d candidate **pending written provenance from the publisher**. AdaFace (MIT repo incl. weights, WebFace600K/260M research lineage), ONNX-Zoo arcfaceresnet100 (Apache-2.0, MS1MV2 withdrawn/unverifiable), SFace + Paddle face exports (Apache repo badges; CASIA-WebFace NC / MS1Mv3 research; PaddleClas repo-level statement puts models/datasets under CC BY-NC 4.0 — recorded as a license conflict, conservative reading = CC BY-NC) → disclosed tier at best. EdgeFace — GitHub BSD-3 vs HF CC BY-NC-SA 4.0 **conflict** → REJECT pending author clarification. w600k_r50 / buffalo_l / antelopev2 / Keras facenet512 → **REJECT payload; user-supplied only**.
   * *User-Supplied Fallback (CONFIRMED AS THE PLAN, not Plan B):* embedders are **user-supplied** via `download-model` behind an explicit license-acceptance gate. No embedder ships in repository payloads. Default tier = YuNet detector (shipped, WIDER FACE lineage disclosed in the acceptance-gate text) + user-supplied embedder; the MediaPipe pair is the fully-clean low-accuracy fallback tier; AuraFace 512-d joins the tier list only if provenance arrives in writing.
   * Open items recorded: AuraFace provenance unenumerated; EdgeFace license conflict; Paddle per-model weight terms; MS1MV2-withdrawal provenance gap for all zoo ArcFace exports; MediaPipe internal training collection unverifiable; no public legal position on whether WIDER FACE NC-ND reaches trained weights (every major detector inherits this).
2. **Inference Engine Spike (`ort` vs `tract`):**
   * Spike pure-Rust `tract` vs `ort`. 
   * *Acceptance Metrics:* Cosine similarity agreement $\ge 0.999$ (with 2-ulp float tolerance) against a dev-only Python reference harness using public Wikimedia fixtures (~30 identities / ~50 images; fixture set must include multi-year same-person pairs and side profiles — the purity curve pins on them).
   * *Cost Metrics:* Measured budget on target hardware: ms/photo, ms/face, RSS peak, and payload delta ($\sim 0$ for `tract`).
3. **Preprocessing Parity:**
   * Prove input-tensor byte-parity (letterbox padding, color space conversion BGR/RGB, resize interpolation) before measuring output agreement.
4. **Quality Gates & Aligned Crops:**
   * Define minimum face size, confidence thresholds, and blur filters.
   * Implement 5-landmark affine warp as a pure-Rust helper in `crates/native` rather than graph ops.

---

## Phase 1: Engine & Storage Architecture

Face processing is strictly decoupled from the UI shell.

### 1. Crate Boundaries
* **`crates/runtime`:** Executes pure-Rust ONNX inference via `tract` (CPU-only). Shares the existing `crates/native` image decoding pipeline for EXIF orientation and file format boundaries.
* **`crates/core`:** Owns the isolated `face.sqlite` store, handles clustering logic, and exposes database interactions.
* **`crates/services` & `crates/server`:** Expose verbs as JSON-lines relays and handle server endpoints/MCP tool calls.

### 2. Storage & Cross-Store Joins (`face.sqlite`)
* **Vector Storage:** No `sqlite-vec`. Face embeddings (N-dim L2-normalized `f32`; N declared by the model pin's meta — 512-d is an ArcFace-family fact, not a spec fact, see Model Tiers) are stored as raw `BLOB` types with brute-force cosine similarity scans.
* **Join Rule (CRITICAL):** Cross-joins between `face.sqlite` and `index.sqlite` key strictly on **path + size + mtime** (never unstable crawl rowids).
* **Prune Cascade:** Pruning or changing a root directory must cascade to drop corresponding face rows and recompute affected person centroids.

### 3. Incremental File Crawler Integration
* **Piggyback Sync:** No secondary crawlers. Face processing rides directly on top of the existing `IndexStore::sync` loop, inheriting standard job-log interrupted/partial-counter semantics.

---

## Phase 2: Feature Surface, Verbs, and UI (People Tab)

The user goal is a functional **People tab** built on thin-client relays.

### 1. UI & Rendering (Server UI - Lit)
* **Lit Page:** Implemented in the server UI using Lit components (no React).
* **Face Cropping:** Face crops are rendered via frontend CSS cropping over existing `/render` thumbnails using relayed bounding box coordinates (zero engine crop verbs required).

### 2. Command Verbs & Scoping
* **Exposed Verbs:** `scan-faces`, `list-persons`, `person-photos`, `name-person`, `merge-persons`, `delete-person`, `find-similar-faces`, `pending-clusters`, and `confirm-person`.
* **Privacy Contract:** `delete-person` instantly purges face vectors and person clusters while leaving primary file indexes untouched.
* **Semantic Scoping:** Engine scope remains strictly path-based (`under:`). Person scoping is composed server/CLI-side by resolving `person-photos` to paths before feeding them into the existing Stage-3 `under:` machinery.

### 3. Model Tiers (v1.1 amendment — the rules that make tiers real)
* **Model-agnostic engine:** default tier = clean shipped detector (YuNet, stated Apache-2,
  WIDER FACE NC lineage documented in the license matrix, no silent "fully permissive" claim)
  + user-supplied embedder behind the `download-model` license-acceptance gate; higher-accuracy
  tiers (buffalo-class, SCRFD detectors) are user-supplied weight packs on the user's authority.
* **Store-scoped model law:** one `face.sqlite` is embedded by exactly ONE embedder pin; vectors
  from different models are incomparable. Tier = per-store choice, never per-photo.
* **Dimension from the pin:** `face.sqlite` meta carries `embed_model` + `dim` (128-d and 512-d
  candidates are both real); scans use the meta's dim. The 512-d figure is a model fact, not a spec fact.
* **Tier switch = full re-embed + re-cluster.** Detector swaps cascade (faces re-derived). Person
  names migrate by majority face-id overlap between old and new clusters (best-effort; the
  migrated fraction is relayed). Phase-1 decision, not a Phase-2 surprise.
* **Cost relayed per tier:** the `estimate` family gains a face-pass forecast per tier; the tier
  table (ms/photo, ms/face, RSS) is recorded here from measurement. Default tier carries no
  accuracy promise — default-tier recall is an eval number.
* **Eval fixture requirement:** multi-year same-person pairs (aging) + side profiles/occlusion in
  the Wikimedia fixture set; purity thresholds pinned on them.

### 4. Scope Boundaries & Watchdogs
* **Still Images Only:** v1.0 indexes still images only. Video-frame face extraction is out of scope.
* **Opt-In Default:** Face indexing defaults to OFF per target. Local-only data; never included in logs or audit pushes.
* **Resource Management:** Rides the existing watchdog (`crates/services/src/watchdog.rs`). Inference models load once for a batch job and auto-unload after 60s of idle time (incorporating batch stickiness).

---

## Competitive Baseline Gate (v1.1 amendment — the accuracy bar, 2026-11-21)

The bar is explicit: **outperform Immich and PhotoPrism on faces, with a stricter license posture than either.** Competitor facts (verified from their docs/configs):

* **Immich:** default recognition model `buffalo_l` = SCRFD-10G detector + `w600k_r50` ArcFace-R50 embedder — NC-research weights shipped in official images; clustering = distance-based with operator knobs (`maxDistance 0.5`, `minFaces 3`, `minScore 0.7`); documented complaint profile: over-splitting / person-spam, quality variance.
* **PhotoPrism (current develop):** bundled default `sface` (SFace, 128-d, CASIA lineage); legacy `facenet` (512-d TF); `auraface` is THEIR optional download (their docs: same quality band as sface at ~8× size); `arcface_r50` registered. Their own measured operating points (TAR @ FAR): `facenet` 0.8318 @ 1.43 %; `auraface` 0.9308 @ 0.14 %; `arcface_r50` 0.9943 @ 0.14 %; detection runs on the **720-px thumbnail**; documented failure profile: over-merging (family relatives merged into one cluster), DBSCAN core tuning.

**Tier claims (published benchmark numbers — the measured-claim bar below is what ships):**

| Tier | Embedder | LFW / CFP-FP / AgeDB / CPLFW | Verdict vs competitors |
|---|---|---|---|
| Clean-shipped (fully permissive) | MediaPipe face_embedding 192-d (Apache-2.0, internal training) | MobileFaceNet-class | ≥ PhotoPrism default band; **< Immich default** |
| Clean-shipped candidate (provenance pending) | AuraFace `glintr100` 512-d (Apache-2.0) | 99.65 / 95.19 / 96.10 / 90.93 | > PhotoPrism `facenet`; ≈ PhotoPrism `sface` band; **< Immich default** (their own table: 0.9308 vs 0.9943 TAR) |
| Accuracy tier | **AdaFace IR-101 512-d (MIT weights incl. repo license; WebFace600K/12M lineage)** — **DECIDED 2026-11-21: user-supplied only, never payload; rides the UI Consent & Download Gate below** | 99.78–99.83 / **99.2** / 98.2–98.4 / **95.0**; IJB-C 97.66 TAR@FAR 0.1 %; TinyFace/IJB-S SOTA | **> Immich default** (`w600k_r50`: 99.80 / 97.82 / 97.82 / 92.23) on cross-pose, cross-pose-lighting, low quality; ≈ on age/ frontal. > everything in PhotoPrism's registry. Quality-adaptive margin = the aging/lighting/small-face axis the People tab needs |
| Detector | default YuNet (shipped, disclosed); user tier SCRFD-10G (NC) via the consent gate | SCRFD > YuNet on tiny faces | Immich's detection = SCRFD-10G; parity needs the user-supplied tier |

**The gate is a measurement, not a claim.** Dev-only harness scores competitor-class models (`w600k_r50`, `scrfd_10g`, `sface`, `facenet`, `auraface`, AdaFace IR-50/IR-101, MediaPipe pair) on the SAME pinned fixtures — per-person recall, cluster purity + purification, multi-year age-gap pairs, low-quality crops, tiny faces in group photos — at matched budgets (ms/photo, ms/face, RSS). **No "outperforms X" line ships in our docs from vendor numbers; only from fixture rows recorded in this file.**

Design edges to measure (they are where "outperform" is most defensible — their documented failures are clustering, not embedding):
* Online quality-scored centroid clustering + collision-distance guard + match margin + the **active-learning review queue** vs their pure auto-clustering (their issue threads are the target: merged-grandpas, split-personas).
* Detection at full decoded pixels vs PhotoPrism's 720-px thumbnail → small-face recall, measured.
* CPU pure-Rust (tract) vs Python-onnxruntime (Immich) / Go+TF/ONNX (PhotoPrism) — per-face ms at parity accuracy, measured.
* License posture: **we ship zero NC-licensed weights; both competitors ship NC-lineage weights in official images.** This advantage is real only if the shipped tier stays clean (MediaPipe/AuraFace) and NC-lineage models (AdaFace-with-disclosure, SCRFD, w600k, buffalo) ride the user-supplied acceptance gate.

Open item: Immich's 2025+ "new face backend / more models" direction (minivision/miniface family) — license + lineage not yet audited; re-survey before Phase 2 ships claims.

---

## UI Consent & Download Gate (v1.1 amendment — 2026-11-21)

**Posture decision recorded: option (b) — user-supplied only.** NC/research-lineage weights (AdaFace IR-101, SCRFD-10G, buffalo-class packs) never ship in payload tiles; the payload tier stays 100 % clean (YuNet detector + fully permissive embedder candidates only). Restricted weights enter a machine only through the gate below.

1. **People tab UX & enable action (default OFF).** First navigation to the People tab is an explicit enable-state empty screen: "Enable Face Recognition" is the only action. No face scan starts, `face.sqlite` is not populated, and no face model loads until consent is recorded. Consent is **per-target** (a `photos` target's consent is not a global face license).
2. **License acceptance popup.** "Enable" opens a modal naming every model in the chosen tier: model name, pinned SHA-256, weight license (AdaFace: MIT), training-dataset lineage **with the dataset's research/non-commercial terms quoted verbatim** (WebFace600K/12M application terms; the Glint360K quote from Phase 0), and a plain-language sentence: *"These weights are downloaded on your authority. The DigUp distribution does not ship them and does not grant you rights over the training data."* The fully-clean tier is listed as the default option in the same modal; choosing the high-accuracy tier is the affirmative act. No pre-ticked checkbox, no scroll-wrap, accept button is named ("I understand and accept"), and every license quote is a real link to its source text.
3. **User-initiated download.** Only after affirmative consent does the server fire the existing `download-model` verb: pinned URL → pinned SHA-256 → progress relayed verbatim via JSON-lines events (download / checksum / load — no invented progress). Hash mismatch is a hard, honest failure with the engine's own error line; **no silent fallback to the clean tier.**
4. **Consent is server state, not UI state.** The record lives in `<config>/faces-consent.json` (0600 — the `tokens.json` precedent; timestamp + model pin + license-text snapshot). The engine and server enforce it: `scan-faces` and every face verb refuse without a consent record (additive, honest 400/409 lines). CLI, MCP agents, and any thin client hit the same wall — **what the UI gates, the engine gates**; no surface bypasses the acceptance act.
5. **Revocation.** "Disable face recognition" purges `face.sqlite` entirely (vectors, clusters, person names) plus the consent record; gate-downloaded model files are removed by a local op (a `delete-model` verb line — Phase-1 decision). `delete-person` semantics unchanged (one person's vectors/clusters, indexes untouched).
6. **Legal separation (the gate's purpose).** The AGPL distribution ships zero NC-lineage weights; restricted weights exist on a machine only by the user's affirmative, logged act under the dataset publishers' own terms; the Phase-0 license matrix ships with the payload. Consent record, model files, and face stores are local-only — the privacy audit covers them like index files.


### Phase 0 #3 — cv-pixel parity (measured 2026-11-22, this box)

Fixtures: `crates/native/tests/fixtures/face_cvops` (self-made smooth-RGB images,
seeds pinned; cv2 4.12 + scikit-image under `OPENCV_IPP=disabled` — the wheel's IPP
warp path diverges from OpenCV's built-in, and the port transcribes the built-in).
Suite: `cargo test -p digup-native --test cvops_parity` — 3 tests, green.

- `resize` (INTER_LINEAR 8u): byte-exact vs the pinned model; cv2 divergence
  (counted outside the recorded `tail_rows` — cv2's upscale last row reads
  `src.ptr(h)` past the Mat buffer, an allocator-dependent value):
  diffs 0/0/614/0/0/53/0, max Δ 1 (fixtures 0-6).
- `warp` (INTER_LINEAR, BORDER_CONSTANT 0): byte-exact vs the pinned model;
  cv2 divergence 6533/6295/0/362 of 37632 bytes, max Δ ≤ 4 — all at the
  AB_BITS=10 map quantization boundary (the fixtures record cv2's measured maps
  via 16U ramp warps; the mask = model map ≠ cv2 map). The 8u cast is
  round-half-to-even of Σ/32768 (measured: 202.5→202, 199.5→200).
- `fit`: Umeyama closed form vs scikit-image `SimilarityTransform`, rel < 1e-5.

Law: the engine ships the pinned model's numbers (byte-deterministic across
platforms); the divergence from the Python cv2 wheel is recorded as measured
facts, not hidden. The pipeline-level gate is embedding agreement vs the
insightface reference (§Phase 0 #4).

### Phase 0 #2/#4 — pipeline agreement + cost (measured 2026-11-22, this box,
ai-ubuntu-computer: Ryzen 9 7950X, `CARGO_BUILD_JOBS=2` build, CPU tract, `faces` feature)

Fixtures: 34 public Wikimedia photos (`/tmp/opencode/face-fixtures`, manifest with
source URLs + licenses), 172 reference faces (insightface `scrfd_10g_bnkps` +
`w600k_mbf` on onnxruntime CPU, `OPENCV_IPP=disabled`). Harnesses:
`crates/runtime/examples/face_spike.rs` (pipeline), `face_embed_crops.rs`
(isolation probe), reference `/tmp/opencode/ref_harness.py`.

Gate line (min cosine on matched faces, bar ≥ 0.999): **model numerics
`w600k_mbf` = 1.00000000 min/median over all 171 matched faces** — tract f32
reproduces onnxruntime f32 **bit-exactly** on byte-identical 112-crops.

End-to-end facts (Rust pipeline vs reference on the same photos) — the residual
is crop geometry through the letterbox→SCRFD→warp cascade, not the model:

- detection: 171/172 faces matched (1 reference face under threshold in Rust),
  **min IoU 0.994267**, max kps L1 Δ **0.6526 px** (over the 5 points);
- embeddings: min cos **0.99472308**, median **0.99970748**,
  frac ≥ 0.999 = 0.912, frac ≥ 0.9999 = 0.164;
- crop pixels (Rust vs reference, matched faces): 21.7 % of pixels differ on
  average, max Δ 79, worst-face frac 0.66 — sub-pixel warp shift from the
  0.16-px/coordinate kps delta, amplified by the 112-px template scale;
- the swapRB fact: the model input is channel-0 = R
  (`blobFromImage(BGR, swapRB=True)`); reading it backwards (B-first) costs
  min IoU 0.843, max kps Δ 13.6 px, min cos 0.43 — recorded because the
  pipeline must reproduce it deliberately, not inherit cv2.

Costs (CPU, this box, tract `default-features=false`): decode 14 516 ms /
34 photos (427 ms/photo), detect 149 588 ms (**4.4 s/photo** at 640×640),
embed 133 936 ms / 171 faces (**783 ms/face**, w600k_mbf),
per-photo total 8.77 s; **VmHWM 304 MB**. GPU row: see below (bounded attempt).

Licenses of the probe models (dev-side reference only, user-supplied tier in
shipping): `scrfd_10g_bnkps` (non-commercial research terms), `w600k_mbf`
(AntelopeFace, research-only) — never payload (Phase 0 #1).

### Phase 2 #2 — faces verbs daemon gate (measured 2026-11-22, this box)

Gate: `crates/cli/tests/faces_service.rs` — a live `digup faces-service` over
JSON-lines on five repo-committed public Wikimedia fixtures
(`crates/cli/tests/fixtures/faces/`, manifest with source URLs + licenses,
resized ≤1600 px q88). 2 tests green (31 s with the pinned models, self-skip
without them): full verb round trip (scan/counts/persons/name/person/person-faces/
merge/delete/cluster/pending/similar/prune/quit) + the tier-switch refusal
(second embedder pin on the same store → daemon exits non-zero before `ready`).

Fixture cosine rows (`w600k_mbf`, Rust pipeline, the facts the gate pins):

| pair | cos | fact |
|---|---|---|
| Merkel 2005 ↔ 2021 | 1.0000 | SAME source photograph (identical Wikimedia file) — merges exactly |
| Merkel 2005/2021 ↔ 2013 | 0.4761 | the real cross-age pair: joins at 0.45, SPLITS at default 0.5 |
| cross-person (Merkel↔Obama↔Trump) | ≤ 0.068 | cleanly separated at any sane threshold |

Gate threshold **0.45** — at the default 0.5 the 2013 photo stands alone
(4 persons, the purity curve's point); at 0.45 the three Merkel files form one
person (cover 0.860), Obama and Trump stand alone: **5 faces, 3 persons, 0
pending**. The 0.5-vs-0.45 fact is the fixture row behind the merge-threshold
knob; the server does not invent it.

Daemon facts (measured, same run): scan events = one `face-scan` per file in
readdir order (order is a crawl artifact — the gate asserts counts, not order);
freshness `(path, size, mtime)` skips re-decode; `--emb-model` is REQUIRED
(no default embedder ships — the user-supplied posture); the daemon requires
`--faces-consent` (the store-scoped consent file: what the UI gates, the
engine gates); person ids are `New(i) → i+1` insertion order.

### Schema 2 — the per-file scan fact (measured 2026-11-22, this box, ai-ubuntu-computer)

The server contract suite (`server/tests/test_faces.py`) caught schema 1's freshness gap:
`is_fresh` read the **face rows**, so a file that produced no faces was never recorded and
**every scan re-decoded every faceless photo** — the majority of a photo library, at
~4 s/photo. Measured on the 12 corpus photos: the rescan decoded 9 files (the faceless
ones) with `skipped 0`; the same shape with 3 copies added: `files 12, skipped 0`. Schema
2 adds `scans(path PRIMARY KEY, size, mtime, faces, scanned_at)`: a completed decode
records the FILE's fact (`faces: 0` included), `is_fresh` demands the exact
`(path, size, mtime)` from that table, `prune` takes the scan facts out with their files,
and a schema-1 store backfills its scan rows from its faces at migration.

Gate rows (the daemon gate's step 1b + the suite's rescan test pin these):

| scan over the 12 corpus photos | files | faces | skipped | fresh | assigned |
|---|---|---|---|---|---|
| first (fresh store) | 12 | 3 | 0 | 0 | 3 |
| rescan (nothing changed) | 0 | 0 | 0 | 12 | 0 |
| 3 byte-copies added | 3 | 3 | 0 | 12 | 3 |
| after that | 0 | 0 | 0 | 15 | 0 |

The scan reply gained `fresh` (files skipped as recorded — the honest count of what
piggyback freshness saved; `skipped` keeps its meaning: decode failures, which the next
scan retries). A face whose clustering is still queued no longer makes its file stale:
re-decoding a queued face deletes the very row the human review queue is about, so the
queue's evidence survives a scan (schema 1's queued-condition in `is_fresh` is gone).

Byte-copy row (measured, same box, merge 0.5): 3 face photos copied under new names →
`files 3, faces 3, fresh 4, skipped 0, assigned 3`. Every face lands in a person
(`assigned 6, pending 0`), and the person count is the online centroid's geometry, not a
constant: a copy joins its person only on its cosine to the **centroid**, and a cluster of
three different people sits near the merge line for a member's own photo — measured 2
persons on the 4-photo probe, 3 on the 12-photo suite run (the merged pair's geometry
moves with the crawl order, the rowid artifact the repo notes pin). The merge/confirm
verbs are the human's answer to exactly this marginal case; the suite pins the law
(every face lands, nothing stays queued), never the marginal count.

### Schema 3 — the crop map (2026-11-22)

`faces.width`/`faces.height`: the decoded photo's own pixels, recorded with each face.
The reason is the thin-client crop law: `box`/`kps` are **source** pixels, `/preview` is a
scaled render (`pixels` = longest side), and the client crops the face out of the handle
it is given — for that it needs the map (`scale = pixels / max(width, height)`). Rows
recorded before schema 3 read `0` — the uncropped fallback, filled by the next scan of a
changed file. The migration chain runs 1 → scans backfill → dims (the main index's chain
shape, index_store::migrate); the store's faces, persons and queue are the user's data
and the chain only adds. Gates: `crates/core/tests/face_store.rs` (freshness rows, the
dims row, a schema-1 store landing at 3 with its face kept) and the suite's person-rows
assertions (`width > 0`).

### MCP `people` — the tool IS the REST routes (shipped 2026-11-22)

The peer core advertises `people` on the **API surface only** (the faces verbs ride the
server's own config — the consent record lives in the server's config folder, the scan
serializes on the server's faces writer lock; stdio has no consent seam, so the tool is
invisible there, manifest-pinned). `action` = status/persons/pending/person/scan; the
front-end calls the same handlers the browser calls, so the consent 400, busy 409 and
admin 403 all land on the tool reply relayed verbatim — the peer core authors no new
refusals. Two measured relay laws from the container run: the faces consent wall's 400
body carries `detail` **only** (the `status` field belongs to the bearer gate's 403 —
the estimate/index pin proves that shape); a persons relay equality must strip the
**per-call minted `media_id`** row-by-row on both sides (the search relay test's law:
a fresh handle per answer).

### Consent record — the f64 round-trip row (measured 2026-11-22, this box)

The record's `accepted_at` is an f64, and its file round trip is **not value-exact**:
serde_json's float *parser* is `mantissa × 10^exp`, not correctly rounded. Measured on
the suite's first run — the POST echo carries the exact f64 (`1795337956.9011621`), the
re-read of the same file lands 1 ulp off and its shortest repr loses a digit
(`1795337956.901162`). The suite pins the record per the repo's cross-process float law
(root AGENTS.md, the Swift z-noise row): floats within 2 ulps, strings and integers
exact. A suite demanding bit-exact f64 across a JSON round trip passes only by luck.

### GPU cost row (2026-11-22, 1070 card) — **GPU NOT MEASURED**

Measured reason, not an assumption: tract 0.23.10's `tract-onnx`/`tract-core`
register **no CUDA/Vulkan/wgpu feature** (grep of their Cargo.toml: zero hits);
GPU compute in tract lives in the separate `tract-gpu` crate, which needs a
manual linalg-lowering pipeline plus wgpu's CUDA backend (untested on
Pascal/sm_61, driver 580). Wiring it into SCRFD+ArcFace is an engineering
task, not a measurement — the CPU rows above are the shipping numbers
(CPU is the default device for faces on every platform). Reporting
"GPU NOT MEASURED" per the house rule; invented success is not.
