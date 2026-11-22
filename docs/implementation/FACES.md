# DigUp Native Face Recognition Engine
**Architectural Specification v1.1 (Final & Amended)**

Status: planning — Phase 0 gates open (license audit in flight, `ort`/`tract` spike not run).
Rides branch `feature/native-faces`. Binding gates below are the build order; a phase is not
done until its gate numbers are measured and recorded here.

This specification establishes the blueprint for implementing a native, privacy-first facial recognition and active-learning clustering system inside **DigUp**. All components strictly adhere to house architecture laws, pure-Rust execution principles, and isolated data boundaries.

---

## Phase 0: Research & Licensing Gates (Pre-Code)
Before code execution begins, the following gates must be cleared:

1. **Licensing Audit & Dataset Lineage:** 
   * Audit embedder training sets (e.g., YuNet, MobileFaceNet/PaddleFace variants) for dataset provenance (WIDER FACE, CASIA, etc.).
   * *User-Supplied Fallback:* If research licenses prevent commercial/redistribution packaging, embedders are **user-supplied** via `download-model` behind an explicit license-acceptance gate. Models never ship in repository payloads.
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
