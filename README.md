<p align="center"><img src="docs/logo.png" width="96" alt="DigUp Logo"></p>

<h1 align="center">DigUp</h1>

<p align="center">
  <b>Describe it. Dig it up.</b><br>
  Local-first, multimodal semantic search for your files — photos, PDFs, documents, audio, video, and code — powered by Google DeepMind's <a href="https://huggingface.co/google/embeddinggemma-2">EmbeddingGemma 2</a>.<br>
  Engine, HTTP server, and desktop runtime built in <b>pure Rust</b>. Everything executes locally on your machine.
</p>

<p align="center">
  <a href="https://github.com/Ocean572/DigUp-RS/releases/latest"><b>Download</b></a> ·
  <a href="#docker-compose-deployment"><b>Docker Compose</b></a> ·
  <a href="#command-line-usage"><b>CLI</b></a> ·
  <a href="#architecture--workspace"><b>Architecture</b></a><br>
  <sub>Free & open source (MIT). Zero accounts, zero telemetry, zero cloud.</sub>
</p>

<p align="center">
  <a href="https://github.com/Ocean572/DigUp-RS/stargazers"><img src="https://img.shields.io/github/stars/Ocean572/DigUp-RS?style=flat-square" alt="GitHub stars"></a>
  <a href="https://github.com/Ocean572/DigUp-RS/releases"><img src="https://img.shields.io/github/downloads/Ocean572/DigUp-RS/total?style=flat-square" alt="Downloads"></a>
  <a href="https://github.com/Ocean572/DigUp-RS/releases/latest"><img src="https://img.shields.io/github/v/release/Ocean572/DigUp-RS?style=flat-square" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Ocean572/DigUp-RS?style=flat-square" alt="MIT license"></a>
</p>

<p align="center">
  <img src="docs/demo.gif" width="800" alt="DigUp search interface finding video frames, podcast timestamps, PDF clauses, multilingual notes, and source code.">
</p>

---

**EmbeddingGemma 2** is Google DeepMind's multimodal embedding model (released October 2026). It projects text, images, audio, and video into a unified vector space: a photo of a dog on a beach maps directly alongside the query *"a dog on the beach"*, and a podcast discussion on sleep maps directly to *"where they talk about sleep"*. At **740M parameters** under an Apache 2.0 license, it supports over **100 languages** while remaining lightweight enough to run locally on consumer hardware.

**DigUp** brings this capability directly to your local filesystem. Point it at the directories you want to index, and it extracts and embeds images, PDFs, office documents, audio recordings, video frames, and source code—letting you search everything using natural language.

---

## Key Capabilities

- **Timestamped Video & Audio Search:** Querying *"zebra in a video"* opens the clip at the exact frame the zebra appears; querying *"where they talk about sleep"* jumps directly to that timestamp in a podcast.
- **Document & Page-Level Retrieval:** Querying *"the clause about pets in the lease"* navigates straight to the matching PDF page.
- **Visual & Screenshot Matching:** Querying *"a dog on the beach"* retrieves matching photos, while *"payment declined error"* locates relevant UI screenshots.
- **Cross-Language Retrieval:** Search in English to retrieve notes written in Bengali, Arabic, or any of the 100+ supported languages—and vice versa.
- **Hybrid Exact-String & Keyword Matching:** Invoice numbers, error codes, and proper nouns are indexed verbatim alongside semantic vectors. Prefix any query with `name:` to bypass semantic embeddings and perform strict filename/directory matching.
- **Dedicated Code Search (Opt-In):** Enable code indexing to search functions by intent (e.g., `code: retry a failed call with backoff` or `code: refreshAccessToken`) and open results directly in your editor at the defining line.

## People: Face Recognition (Opt-In)

DigUp can recognize faces in your indexed photos, group them into people you can name, and filter search results by person — in the desktop app (the **People** tab) and in the web UI.

- **Consent first:** face recognition stays fully off until you enable the People tab. The enable screen explains in plain words what the feature does and which model packs it downloads, and your acceptance is recorded as a local file. Withdraw anytime: face processing stops, and your photos are never touched.
- **How it works:** a detector finds faces in photos, an embedder turns each face into a vector, and the vectors cluster into people. Name the clusters you recognize, merge duplicates, dissolve the rest. Scans are incremental, so new photos are picked up on the next scan.
- **Face data stays local:** vectors and clusters live in a dedicated `face.sqlite` beside the index on your machine. Photos are never uploaded, and recognition runs fully offline ([why the packs are downloaded](#why-face-model-packs-are-downloaded-not-packaged)).
- **People filter on search:** pick a named person in the search bar and results narrow to the photos carrying that person's face. The engine's ranking is untouched — the filter relays the face store's own membership facts.

## Privacy by Design

Your files never leave your hardware. DigUp connects to the internet **only once** to download the model weights. There are no accounts, no analytics, and no external API calls. Your entire index resides in a single, portable SQLite database on your local disk, and indexed media directories are mounted strictly **read-only**.

---

## Deployment Options

| Option | Best For | Description |
| :--- | :--- | :--- |
| **[Docker Compose](#docker-compose-deployment)** | Self-hosting & LAN | Single-container deployment combining the Rust engine, HTTP API (`/api/v1`), and web UI. |
| **[Command Line (`digup`)](#command-line-usage)** | Terminal & Scripting | Standalone cross-platform CLI (`digup`) and headless API daemon (`digup-server`). |
| **Desktop App (Beta)** | Native Desktop | Lightweight tray/panel app (Tauri) for Windows and Linux backed by the `digup` binary. |

---

## Docker Compose Deployment

Run the engine, REST API (`/api/v1`), and web frontend inside a single container. Your index, model weights, and configuration persist as standard host directories, while your media directories are mounted **read-only**.

### 1. Start the Container (x86_64 Prebuilt Image)

```bash
git clone https://github.com/Ocean572/DigUp-RS && cd DigUp-RS/deploy
mkdir -p data/index data/models data/config data/media

# Required: the container executes as unprivileged UID 990
sudo chown -R 990:990 data

docker compose -f docker-compose.yml -f docker-compose.ghcr.yml up -d
```

### 2. Download the Model Weights (865 MB, One-Time)

Download the pinned EmbeddingGemma 2 weights into the mounted `data/models` directory using the native downloader:

```bash
scripts/fetch-vendor.sh && scripts/build-llama.sh && cargo build --release
DIGUP_MODELS=$PWD/data/models ./target/release/digup download-model
```

### 3. Open the Web Interface

Navigate to **http://127.0.0.1:8787**. Place the files you want to search inside `data/media/photos` (mounted read-only at `/media/photos`), click **Index now**, and begin searching once indexing completes. See [`server/docs/API.md`](server/docs/API.md) for the complete HTTP API specification.

### Environment Variables (`deploy/docker-compose.yml`)

| Variable | Default | Description |
| :--- | :--- | :--- |
| `DIGUP_MEDIA_DIR` | `./data/media/photos` | Host directory mounted read-only at `/media/photos` |
| `DIGUP_INDEX_DIR` | `./data/index` | Host directory storing `index.sqlite` |
| `DIGUP_MODELS_DIR` | `./data/models` | Host directory storing EmbeddingGemma 2 weights |
| `DIGUP_CONFIG_DIR` | `./data/config` | Host directory storing configuration and job logs |
| `DIGUP_PUBLISH_PORT` | `8787` | Host port exposed by the web server |
| `DIGUP_LAN_BIND` | *(loopback only)* | Bind IP for LAN access (opt-in; server has no built-in auth) |

### Compose Override Profiles

Combine modular Compose files to tailor your runtime environment:

| Override File | Purpose |
| :--- | :--- |
| `docker-compose.yml` | Base configuration (builds the server image from source) |
| `docker-compose.ghcr.yml` | Pulls the prebuilt GHCR image (`ghcr.io/ocean572/digup-server`) without local compilation |
| `docker-compose.rust.yml` | Pure-Rust runtime (`digup-server` binary serving engine + API + UI with zero Python) |
| `docker-compose.nvidia.yml` | Enables CUDA build and GPU runtime (`--profile nvidia`) |
| `docker-compose.lan.yml` | Exposes the server on your local network via `DIGUP_LAN_BIND` |

**Example Deployments:**

```bash
# Pure-Rust server exposed on a local LAN IP:
DIGUP_LAN_BIND=192.168.1.20 docker compose \
  -f docker-compose.yml -f docker-compose.rust.yml -f docker-compose.lan.yml up -d --build

# NVIDIA CUDA-accelerated build and runtime:
docker compose -f docker-compose.yml -f docker-compose.nvidia.yml --profile nvidia up -d --build
```

> **Hardware Acceleration Note:** GPU acceleration is verified empirically rather than assumed. Run `digup speed --model --json` to inspect the active compute backend and verify cosine similarity agreement against CPU execution ($\ge 0.999$ required). See [`docs/implementation/gpu-handoff.md`](docs/implementation/gpu-handoff.md) for details.

---

## Command Line Usage

Prebuilt release tarballs for **Linux** (`x86_64`, `aarch64`), **macOS** (`arm64`, `x86_64`), and **Windows** (`x86_64`) are available on the [Releases page](https://github.com/Ocean572/DigUp-RS/releases/latest). Each archive bundles the `digup` binaries alongside their required native shared libraries—extract anywhere and run.

**macOS / Linux via Homebrew:**
```bash
brew install ocean572/tap/digup
```

**Core CLI Workflow:**
```bash
digup download-model                  # Fetch pinned model weights (one-time)
digup estimate ~/photos               # Preview file counts and estimated indexing time
digup index ~/photos                  # Build the initial semantic + FTS5 index
digup search "a dog on the beach"     # Query the index (ranked by engine z-score)
digup speed --model                   # Benchmark GPU vs. CPU speed and cosine agreement
digup --help                          # List all available subcommands
```

All frontends (the web UI and Tauri desktop shell) communicate with `digup` strictly over documented JSON-lines protocols. All ranking, scoring, and filtering happen exclusively inside the Rust engine.

UI work is evaluated on the **desktop app** (the Tauri shell), not on the container image — the contract container is a CI checkpoint, never the test target. Every UI change lands on every front in the same round: the shell (`shell/`, which Windows and macOS ride at packaging time) and the web UI (`server/ui/`).

---

## MCP: Search From an AI Agent

DigUp is a Model Context Protocol (MCP) server: any MCP-capable agent — Claude Desktop, Cursor, Claude Code, opencode, and others — can connect and search your indexed folders. Every answer is the engine's own rows, scores, and counts, relayed verbatim; your files stay on your computer.

- **stdio (local agents):** `digup mcp [FOLDER]… [--index DIR]` — one JSON-RPC message per line on stdin, one on stdout. Tools: `search`, `status`, `show`. The desktop app ships a one-click toggle that registers it with Claude Desktop.
- **HTTP (remote and LAN agents):** a server deployment also answers `POST /api/v1/mcp` behind the same bearer gate as the REST API: a read key answers `search`, `status`, `show`, and `people`; an admin key adds `estimate` and `index`.

The full contract, tool by tool, is in [`server/docs/API.md`](server/docs/API.md).

---

## Supported File Formats & Indexing Scope

DigUp scans only the directories you explicitly configure, never modifies or moves source files, and automatically skips sensitive paths (`.ssh`, keys, certificates, password stores, application bundles, caches, and ML dataset directories):

- **Images & Screenshots:** JPEG, PNG, HEIC, WebP, and other standard raster formats.
- **PDFs:** Page-by-page extraction including scanned documents (prioritizes the first 30 pages immediately; supports up to 5,000 pages per file).
- **Documents:** `.txt`, `.md`, `.typ`, `.rtf`, `.docx`, `.odt`, `.html`.
- **Spreadsheets & Presentations:** `.xlsx`, `.ods`, `.pptx`, `.odp` (indexes cell rows, slide text, and speaker notes).
- **Audio:** Any duration, segmented into 30-second sliding windows with timestamped match pointers.
- **Video:** Any duration, sampling keyframes per shot alongside full soundtrack audio indexing.
- **Source Code (Opt-In):** Isolated in a separate index (`code:` prefix). Indexes source files, Jupyter notebooks (excluding outputs), and documentation while respecting `.gitignore` and skipping vendored dependencies or secrets.

---

## Architecture & Workspace

```text
Indexed Folders ─► Native Readers: PDFium (PDF), FFmpeg (Audio/Video), stb (Images), miniz (OOXML)
                      │
                      ▼
                   EmbeddingGemma 2 via llama.cpp (isolated worker subprocess)
                      │
                      ▼
                   SQLite Database (index.sqlite)
                   ├── 768-dim float16 vector embeddings (per image, page, passage, frame, 30s audio)
                   └── FTS5 full-text keyword index (separate DB for code search)

Search Query ───► Text Encoder (~250 MB, loaded on demand)
                      │
                      ▼
                   Cosine Nearest-Neighbor + Exact-Keyword Boost ─► Ranked Results (z-scores)
```

### Cargo Workspace Layout

| Crate / Directory | Responsibility |
| :--- | :--- |
| `crates/core` | Document/mail extraction, vector math, scoring, and archive handling |
| `crates/native` | FFI bindings and safe shims for `llama.cpp`, `FFmpeg`, `PDFium`, and `stb` |
| `crates/runtime` | Embedding pipelines, indexing event loops, and the search engine |
| `crates/services` | JSON-lines IPC daemons (`search-service`, `index-service`, `SharedEmbedder`) |
| `crates/cli` | The `digup` command-line interface |
| `crates/server` | The `digup-server` Axum HTTP API, background job runner, and media streamer |
| `shell/` | Cross-platform Tauri desktop wrapper (Rust backend + lightweight JS frontend) |

### Schema & Twin Parity

Each index tracks the exact model version that generated its vectors, and automated reference-vector tests validate every `llama.cpp` update prior to release. The SQLite schema (`v6`: tables, triggers, counters, and FTS5 virtual tables) is **100% byte-compatible** with the original Swift implementation—allowing indexes created by either implementation to be queried interchangeably without re-indexing.

---

## Build from Source

### Prerequisites
- **Rust:** `1.88+` (CI pins `1.99.1`)
- **Build Tools:** C/C++ compiler toolchain (`gcc`/`clang`/`MSVC`), `CMake`, and `Python 3`

```bash
git clone https://github.com/Ocean572/DigUp-RS && cd DigUp-RS

# 1. Fetch pinned, SHA-256 verified C/C++ dependencies (llama.cpp, SQLite/FTS5, PDFium, stb, miniz)
scripts/fetch-vendor.sh

# 2. Compile llama.cpp at the pinned revision
scripts/build-llama.sh

# 3. Build release binaries (target/release/digup and target/release/digup-server)
cargo build --release

# 4. Run the workspace test suite
cargo test --workspace
```

To build or test the web frontend (`server/ui`, Lit + Vite):
```bash
cd server/ui && npm ci && npm test && npm run build
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for development workflows and PR guidelines.

---

## CI/CD & Releases

Releases are automated and gated via GitHub Actions:
- **Tagged Releases ([`.github/workflows/rust-release.yml`](.github/workflows/rust-release.yml)):** Pushing a version tag executes the full Rust workspace suite and the 139-test language-neutral contract suite against the compiled binary with zero skips. Once green, native builds compile tarballs for Linux, macOS, and Windows and update the Homebrew tap.
- **Container Images ([`.github/workflows/server.yml`](.github/workflows/server.yml)):** Every merge to `main` builds and publishes updated container images to `ghcr.io/ocean572/digup-server`.
- **Port Verification Ledger:** Full architectural parity benchmarks and audit logs are documented in [`docs/RUST_PORT_AUDIT.md`](docs/RUST_PORT_AUDIT.md).

---

## FAQ

**How does DigUp differ from macOS Spotlight or Windows Search?**  
Built-in OS search tools index filenames, metadata, and embedded text. DigUp understands semantic visual and audio content—allowing you to search inside images, silent video frames, and spoken audio while jumping directly to the matching timestamp or page. It runs cleanly alongside native OS search tools.

**Why rewrite the engine in Rust?**  
Porting the engine to Rust eliminates the macOS/Swift toolchain dependency, enabling native compilation and deployment across Linux servers, Docker containers, and Windows desktops. The Rust port reproduces the original JSON-lines protocols, SQLite schema, and vector layouts byte-for-byte, verified continuously against the Swift reference suite.

**Can I use an existing index created by the Swift version of DigUp?**  
Yes. Because the SQLite schema is byte-compatible, the Rust engine opens and searches Swift-built indexes directly with zero re-indexing (verified against a 131,973-file Swift index running on the Rust Docker stack). Ensure your mounted folder paths match the original root paths.

**Why is the model download 865 MB?**  
The download contains the complete multimodal EmbeddingGemma 2 weights: the text encoder (~310 MB) and the vision/audio encoders (~555 MB). The compiled `digup` binaries themselves are only a few tens of megabytes.

**Will indexing slow down my system?**  
Only the initial indexing pass is compute-intensive; subsequent incremental updates process new files in seconds. The heavy multimodal encoders run inside an isolated worker subprocess that exits immediately after indexing finishes, while search queries load only the lightweight ~250 MB text encoder on demand.

**What languages are supported?**  
EmbeddingGemma 2 supports semantic and cross-language search across 100+ languages. Note two platform-specific limits on exact-keyword boosting: on-device OCR for text inside screenshots is currently macOS-only (via Apple Vision), and unsegmented scripts without spaces (e.g., Chinese, Japanese, Thai) rely on semantic vector matching rather than whitespace-delimited FTS5 token boosting.

**How do I uninstall DigUp?**  
Remove the `digup` binaries and delete your local data directories (`index.sqlite`, model weights, and config). For Docker deployments, run `docker compose down` and delete the `deploy/data/` folder. Your indexed source media folders are never altered.

---

## Acknowledgments & Credits

DigUp was originally created by Abdurrahim ([`ARahim3/DigUp`](https://github.com/ARahim3/DigUp) · [Support the original creator](https://arahim3.gumroad.com/l/support)). This repository (`Ocean572/DigUp-RS`) is the **pure-Rust port**: `main` houses the Rust engine, CLI, server, and Tauri shell, while the original Swift codebase is preserved on the [`swift-reference`](../../tree/swift-reference) branch as the byte-parity reference baseline (alongside the native SwiftUI macOS app in `Sources/DigUpApp`).

Powered by open-source technology:
- [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2) by Google DeepMind (Apache 2.0)
- [llama.cpp](https://github.com/ggml-org/llama.cpp) (MIT)
- [Tauri](https://tauri.app) (MIT / Apache 2.0)
- [PDFium](https://pdfium.googlesource.com/pdfium/) (BSD), [FFmpeg](https://ffmpeg.org) (LGPL), [stb](https://github.com/nothings/stb) (Public Domain), and [miniz](https://github.com/richgel999/miniz) (MIT)

*DigUp is an independent open-source project and is not affiliated with Google, Apple or Microsoft. Gemma is a trademark of Google LLC; Mac and Spotlight are trademarks of Apple Inc.; Windows is a trademark of Microsoft Corporation.*

## License

[MIT](LICENSE)

---

## Why Face Model Packs Are Downloaded, Not Packaged

Face recognition rides on two model packs — a face detector and a face embedder — totaling roughly 30 MB to 280 MB depending on the accuracy tier you choose. They are too large to ship inside every app download, and their licenses call for a download you authorize yourself, so they are not packaged. Enabling the People tab downloads the packs you chose from Hugging Face once — the feature's only network step — and face recognition is fully offline afterward: photos are never uploaded, and face data never leaves your machine. Consent for all of this is explicit, shown in plain words before the download, and revocable at any time; withdrawing deletes face data and never deletes a photo.
