<p align="center"><img src="docs/logo.png" width="96" alt=""></p>

<h1 align="center">DigUp</h1>

<p align="center"><b>Describe it. Dig it up.</b><br>
Search the files on your computer by what's in them — photos, PDFs, documents, recordings,
video, code — with Google DeepMind's
<a href="https://huggingface.co/google/embeddinggemma-2">EmbeddingGemma 2</a>.<br>
The engine, the server and the desktop app are <b>Rust</b>. Everything runs on your machine.</p>

<p align="center"><a href="https://github.com/Ocean572/DigUp-RS/releases/latest"><b>Download</b></a> ·
<a href="#the-docker-compose-app"><b>Docker Compose app</b></a> ·
<a href="#command-line"><b>Command line</b></a><br>
<sub>Free and open source, MIT. No account, no cloud.</sub></p>

<p align="center">
<a href="https://github.com/Ocean572/DigUp-RS/stargazers"><img src="https://img.shields.io/github/stars/Ocean572/DigUp-RS?style=flat-square" alt="GitHub stars"></a>
<a href="https://github.com/Ocean572/DigUp-RS/releases"><img src="https://img.shields.io/github/downloads/Ocean572/DigUp-RS/total?style=flat-square" alt="Downloads"></a>
<a href="https://github.com/Ocean572/DigUp-RS/releases/latest"><img src="https://img.shields.io/github/v/release/Ocean572/DigUp-RS?style=flat-square" alt="Latest release"></a>
<a href="LICENSE"><img src="https://img.shields.io/github/license/Ocean572/DigUp-RS?style=flat-square" alt="MIT license"></a>
</p>

<p align="center"><img src="docs/demo.gif" width="800" alt="DigUp's search panel: a zebra in a video, a podcast minute, a clause in a lease, a dog on the beach in English, Bengali and Arabic, and code that opens in Zed."></p>

EmbeddingGemma 2 is the multimodal embedding model Google DeepMind released in October 2026.
It puts text, images, audio and video into one shared space: a photo of a dog on a beach lands
next to the words "a dog on the beach", and a recording of someone talking about sleep lands next
to "where they talk about sleep". It understands more than 100 languages, it's open (Apache 2.0),
and at 740M parameters it's small enough to run on a laptop.

DigUp brings that to your files. Pick the folders to search, and it reads the pictures, PDFs,
documents, recordings and videos in them. Then describe what you're looking for, in your own
words, and it finds the match, whatever kind of file it is.

## What you can find

- "zebra in a video" opens the clip at the moment the zebra shows up
- "where they talk about sleep" jumps to that minute of a podcast
- "the clause about pets in the lease" shows the PDF page
- "a dog on the beach" finds the photo, and "payment declined error" the screenshot
- a search in English can find a note written in Bengali, Arabic or any of the model's 100+
  languages, and the other way around
- "code: retry a failed call with backoff" finds the function in your projects, if you turn on
  code search

Exact words count too. Invoice numbers, error codes and names are matched as written, in file
names and document text, so a search gets both the meaning and the exact string. Start a search
with `name:` and only file and folder names are searched — the exact words, with the model
staying out.

## Private

Nothing you index leaves your computer. DigUp goes online only to download the model, once.
No account, no telemetry, no cloud. The index is a plain SQLite file on your disk.

## Three ways to run it

| Way | What you get |
|---|---|
| [Docker Compose app](#the-docker-compose-app) | a self-hosted search server with a web UI, on your machine or your LAN |
| [Command line](#command-line) | `digup` — indexes and searches from a terminal; `digup-server` — the same HTTP API |
| Desktop app (beta) | the tray/panel app for Windows and Linux (Tauri), which runs the same `digup` binary as its helper |

## The Docker Compose app

One container: the engine, the HTTP API (`/api/v1`), and a web app in the browser. Your index,
the model and the settings live as plain folders on your host disk; the files you search are
mounted **read-only** — DigUp reads them and never touches them. Search answers are the engine's
own numbers: it never invents scores.

**Run it** (x86_64; the published image):

```sh
git clone https://github.com/Ocean572/DigUp-RS && cd DigUp-RS/deploy
mkdir -p data/index data/models data/config data/media
sudo chown -R 990:990 data            # the container runs as uid 990 — this is required
docker compose -f docker-compose.yml -f docker-compose.ghcr.yml up -d
```

**Get the model** (once, 865 MB) — the engine's own downloader, into the same folder the
container mounts:

```sh
scripts/fetch-vendor.sh && scripts/build-llama.sh && cargo build --release
DIGUP_MODELS=$PWD/data/models ./target/release/digup download-model
```

**Open it:** **http://127.0.0.1:8787** — the web app shows index status, an *Index now* button
and search. Put the files you want to search under `data/media/photos` (the default root, mounted
read-only at `/media/photos`), press **Index now**, and search when the job is done. The full HTTP
contract is [`server/docs/API.md`](server/docs/API.md); the UI is a thin client — the engine
decides everything.

**Knobs** (all plain environment variables, all read by
[`deploy/docker-compose.yml`](deploy/docker-compose.yml)):

| variable | default | what it is |
|---|---|---|
| `DIGUP_MEDIA_DIR` | `./data/media/photos` | the tree mounted read-only at `/media/photos` |
| `DIGUP_INDEX_DIR` | `./data/index` | the index folder (`index.sqlite`, plain SQLite) |
| `DIGUP_MODELS_DIR` | `./data/models` | the model folder |
| `DIGUP_CONFIG_DIR` | `./data/config` | settings and the job log |
| `DIGUP_PUBLISH_PORT` | `8787` | the host port |
| `DIGUP_LAN_BIND` | — (loopback only) | publish on your LAN address; the server has no auth, so this is opt-in |

**Compose shapes** — small override files that merge, each opt-in:

| file | what it does |
|---|---|
| `docker-compose.yml` | the base: builds the server image from source |
| `docker-compose.ghcr.yml` | pull the published image (`ghcr.io/ocean572/digup-server`) — no build, no toolchain |
| `docker-compose.rust.yml` | the pure-Rust server: engine + API + UI in one binary, no Python |
| `docker-compose.nvidia.yml` | the CUDA build + runtime for NVIDIA cards (`--profile nvidia`) |
| `docker-compose.lan.yml` | publish on your LAN address: `DIGUP_LAN_BIND=192.168.1.20 …` |

Examples:

```sh
# published image, pure Rust, on your LAN:
DIGUP_LAN_BIND=192.168.1.20 docker compose \
  -f docker-compose.yml -f docker-compose.rust.yml -f docker-compose.lan.yml up -d --build

# NVIDIA GPU (the image is built for your driver's CUDA; see the file's header):
docker compose -f docker-compose.yml -f docker-compose.nvidia.yml --profile nvidia up -d --build
```

GPU is a measured decision, never an assumption: `digup speed --model --json` reports the device
the engine picked and the agreement between GPU and CPU (0.999 agreement required), and the full
procedure is [`docs/implementation/gpu-handoff.md`](docs/implementation/gpu-handoff.md).

## Command line

The [Releases page](https://github.com/Ocean572/DigUp-RS/releases/latest) carries a payload
tarball per platform — Linux x86_64 / aarch64, macOS arm64 / x86_64, Windows x86_64 — with the
native libraries beside the binary: untar anywhere and run. Homebrew:
`brew install ocean572/tap/digup`.

```sh
digup download-model                    # fetch the pinned model, once
digup estimate ~/photos                 # file count + a cost forecast, measured on your box
digup index ~/photos                    # the first pass
digup search "a dog on the beach"       # results ranked by z: the engine's own score
digup speed --model                     # GPU-vs-CPU measurement (device + agreement)
digup --help                            # all verbs
```

Every UI — the web app, the desktop shell — talks to `digup` over its published JSON-lines
interfaces. Nothing above it re-ranks, re-stores, or invents scores.

## What it reads

Only the folders you choose:

- pictures and screenshots (JPEG, PNG, HEIC, WebP and more), by what they show
- PDFs, page by page, scans included (a long one gets its first 30 pages right away, up to 5,000
  pages a file)
- documents: txt, md, typ, rtf, docx, odt, html
- spreadsheets and presentations: xlsx, ods, pptx, odp — each sheet's rows, each slide's text and
  speaker notes
- audio of any length: the model listens in 30-second windows, a match points to its moment
- video of any length: a frame every few seconds, one per shot, plus the soundtrack like audio

It never reads keys, certificates, password files, hidden folders like `.ssh`, apps or caches,
and it skips folders that look like datasets. It only reads; it never changes, moves, or uploads
files. After the first pass (one-time; a big photo library can take hours, so `estimate` shows
the time first), a new file is searchable seconds after it lands.

## Code search

Off until you turn it on. Then `code:` searches find functions by what they do —
"code: refreshAccessToken" goes to the line that defines it, not the places that call it — and
results open in your editor at the line. Code keeps its own index and never shows up in media
searches. It reads source, notebooks (without outputs) and repo docs; it skips what git ignores,
vendored code, and anything that looks like a key.

## How it works

```
your folders ─► readers: PDFium (PDF), ffmpeg (audio/video), stb (images), miniz (zip/OOXML)
                  ─► EmbeddingGemma 2 on llama.cpp, in a helper that quits when it's done
                  ─► one SQLite file: a 768-dim float16 vector per picture, page, passage,
                     video frame and 30 s of sound, plus an FTS5 keyword index
                     (code search keeps a second index)
your words   ─► the model's text part (~250 MB, only while you search)
                  ─► nearest vectors + exact-word boost ─► ranked results with z-scores
```

The Rust workspace:

| crate | what it is |
|---|---|
| `crates/core` | extraction: documents, mail, scoring, vectors, zip |
| `crates/native` | the llama.cpp / ffmpeg / PDFium / stb seams |
| `crates/runtime` | the embedders, the index loop, the search engine |
| `crates/services` | the JSON-lines services (`search-service`, `index-service`, the shared embedder) |
| `crates/cli` | the `digup` binary |
| `crates/server` | the `digup-server` binary: the HTTP API, jobs, media, targets |
| `shell/` | the Tauri desktop app (its Rust + a plain JS UI) |

The index remembers which model made its vectors; a reference-vector test checks every llama.cpp
update before it ships. The SQLite schema is byte-compatible with the Swift original (schema v6:
FTS5, counters, triggers), so an index built by one engine is read and searched unchanged by the
other — measured, not assumed.

## Releases

A release is gated, then built, by CI ([`.github/workflows/rust-release.yml`](.github/workflows/rust-release.yml)):
the workspace suite and the contract suite run **on the tagged commit** — the contract suite with
zero skips, against the real payload — and only then the payload tarballs for Linux, macOS and
Windows are compiled natively per platform and attached to the release, with the Homebrew
formula. The server image publishes to `ghcr.io/ocean572/digup-server` on every merge to `main`
([`.github/workflows/server.yml`](.github/workflows/server.yml)), which is what the
`docker-compose.ghcr.yml` shape pulls. The audit ledger for the port is
[`docs/RUST_PORT_AUDIT.md`](docs/RUST_PORT_AUDIT.md).

## Build from source

You need Rust 1.88+ (CI pins 1.99.1), a C/C++ toolchain, CMake, and Python 3; `cargo` does the
rest.

```sh
git clone https://github.com/Ocean572/DigUp-RS && cd DigUp-RS
scripts/fetch-vendor.sh          # llama.cpp, SQLite/FTS5, PDFium, stb, miniz — pinned, sha256-checked
scripts/build-llama.sh           # llama.cpp at the pinned tag
cargo build --release            # → target/release/digup, target/release/digup-server
cargo test --workspace           # the engine's suites: schema, vectors, protocol, CLI
```

The web app is the [Compose app](#the-docker-compose-app) above (it builds the same payload).
The web UI is [`server/ui`](server/ui) (Lit + Vite): `npm ci && npm test && npm run build`.
Contributions: [`CONTRIBUTING.md`](CONTRIBUTING.md).

## FAQ

**How is this different from Spotlight or Windows Search?** They find files by names, text and
metadata. DigUp also finds them by what they show or say, and takes you to the page or moment.
They work fine side by side.

**Why Rust?** The engine runs on Linux, Windows and in Docker, with no Swift toolchain anywhere
in the build. The port reproduces the Swift engine's published interfaces byte-for-byte — the
JSON-lines verbs, the SQLite schema, the vector layout — and the twin gate compares the two
engines' answers file by file, query by query.

**I have an index from the Swift app. Does it work?** Yes. The schema is byte-compatible; the
Rust engine opens, migrates and searches Swift-built indexes unchanged (measured: a 131,973-file
Swift-built index serving searches from a Rust compose app, zero re-indexing). Keep the same
folder paths — index rows are root-scoped.

**Why is the download 865 MB?** That's the model: the text part (310 MB) and the image and
audio encoders (555 MB). The app is a few tens of MB; the payload tarballs carry the binary plus
the PDF/video/image readers it uses.

**Will it slow my computer down?** Only the first pass is heavy; after that it reads new files.
The model runs in a separate process that quits when it's done, and the search side only loads
while you're searching.

**Which languages?** The model knows more than 100, and searches cross-language. Two limits hit
exact words, not meaning: the text inside screenshots is read on a Mac only (Apple's text
recognition), and languages written without spaces — Chinese, Japanese, Thai — aren't split into
words for the keyword boost.

**How do I remove it?** Delete what DigUp owns: the app, and its data folder — the index and the
model. On the server stack, that's the `data/` folders you created: `docker compose down`, then
delete them. Your files are never touched.

## The Swift reference, and thanks

DigUp was originally created by Abdurrahim ([`ARahim3/DigUp`](https://github.com/ARahim3/DigUp),
support: [arahim3.gumroad.com/l/support](https://arahim3.gumroad.com/l/support)) — the Swift
engine and Mac app. This repository is the **Rust remake**: `main` is the Rust core; the Swift
original lives on the branch [`swift-reference`](../../tree/swift-reference) as the parity
reference the twin gate measures against, and the macOS app (`Sources/DigUpApp`, SwiftUI) stays
Swift.

Built on [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2) by Google DeepMind
(Apache 2.0) and [llama.cpp](https://github.com/ggml-org/llama.cpp) (MIT). The Windows/Linux
app is built with [Tauri](https://tauri.app) (MIT or Apache 2.0) and reads files with
[PDFium](https://pdfium.googlesource.com/pdfium/) (BSD), [FFmpeg](https://ffmpeg.org) (LGPL),
[stb](https://github.com/nothings/stb) (public domain) and
[miniz](https://github.com/richgel999/miniz) (MIT). Their license notices ship with the payload.

DigUp isn't affiliated with Google, Apple or Microsoft. Gemma is a trademark of Google LLC; Mac
and Spotlight are trademarks of Apple Inc.; Windows is a trademark of Microsoft Corporation.

## License

MIT
