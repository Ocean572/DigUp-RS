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
| Accuracy tier (user-supplied / disclosed-ship decision) | **AdaFace IR-101 512-d (MIT weights incl. repo license; WebFace600K/12M lineage)** | 99.78–99.83 / **99.2** / 98.2–98.4 / **95.0**; IJB-C 97.66 TAR@FAR 0.1 %; TinyFace/IJB-S SOTA | **> Immich default** (`w600k_r50`: 99.80 / 97.82 / 97.82 / 92.23) on cross-pose, cross-pose-lighting, low quality; ≈ on age/ frontal. > everything in PhotoPrism's registry. Quality-adaptive margin = the aging/lighting/small-face axis the People tab needs |
| Detector | default YuNet (shipped, disclosed); user tier SCRFD-10G (NC) | SCRFD > YuNet on tiny faces | Immich's detection = SCRFD-10G; parity needs the user-supplied tier |

**The gate is a measurement, not a claim.** Dev-only harness scores competitor-class models (`w600k_r50`, `scrfd_10g`, `sface`, `facenet`, `auraface`, AdaFace IR-50/IR-101, MediaPipe pair) on the SAME pinned fixtures — per-person recall, cluster purity + purification, multi-year age-gap pairs, low-quality crops, tiny faces in group photos — at matched budgets (ms/photo, ms/face, RSS). **No "outperforms X" line ships in our docs from vendor numbers; only from fixture rows recorded in this file.**

Design edges to measure (they are where "outperform" is most defensible — their documented failures are clustering, not embedding):
* Online quality-scored centroid clustering + collision-distance guard + match margin + the **active-learning review queue** vs their pure auto-clustering (their issue threads are the target: merged-grandpas, split-personas).
* Detection at full decoded pixels vs PhotoPrism's 720-px thumbnail → small-face recall, measured.
* CPU pure-Rust (tract) vs Python-onnxruntime (Immich) / Go+TF/ONNX (PhotoPrism) — per-face ms at parity accuracy, measured.
* License posture: **we ship zero NC-licensed weights; both competitors ship NC-lineage weights in official images.** This advantage is real only if the shipped tier stays clean (MediaPipe/AuraFace) and NC-lineage models (AdaFace-with-disclosure, SCRFD, w600k, buffalo) ride the user-supplied acceptance gate.

Open item: Immich's 2025+ "new face backend / more models" direction (minivision/miniface family) — license + lineage not yet audited; re-survey before Phase 2 ships claims.

