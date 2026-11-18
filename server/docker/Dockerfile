# syntax=docker/dockerfile:1
# DigUp engine, built from source for the container's own architecture.
#
# Ubuntu 22.04 is deliberate: the binaries then ask for glibc 2.35 at
# most, which is what the runtime stages ship. Never build here what is meant to run
# there: a binary of the container's own architecture only (§11, native builds).
#
# The engine is Rust (crates/ → the `digup` + `digup-server` binaries), and llama.cpp /
# PDFium / ffmpeg are the native libraries it calls (the rust-engine-* stages). Rust is
# needed at build time only; the runtime stages carry no compiler.
#
#   docker build --target rust-engine-builder -t digup-rust-builder .
#
ARG BASE_IMAGE=ubuntu:22.04
# The CUDA *runtime* base whose libraries server-runtime-cuda ships (Stage 9). It must match
# the CUDA BASE_IMAGE the payload was built with. BuildKit prunes unreferenced stages: a
# CPU-only build (target server-runtime) never pulls this image.
ARG CUDA_RUNTIME_IMAGE=nvidia/cuda:11.8.0-runtime-ubuntu22.04

# ── Toolchain, vendor libraries, llama.cpp, ffmpeg, the CLI, the tests ───────────────────────
# ── The frontend build (user brief 2026-11-08, server/ui/AGENTS.md) ─────────────────────
# Lit + Vite: Node exists HERE, at build time, and never at runtime — the stage compiles
# ── The Rust engine: toolchain, vendor libraries, llama.cpp, ffmpeg, the CLI, the payload — the rust-* stages below ──
# unit test or a red build is a red image by construction. npm ci reads the committed
# package-lock.json; the npm cache rides a BuildKit cache mount (the registry is
# reachable from the builder, measured 2026-11-08).
FROM node:22-slim AS frontend-builder
WORKDIR /build
COPY server/ui/package.json server/ui/package-lock.json ./
RUN --mount=type=cache,target=/npm-cache npm ci --cache /npm-cache --no-audit --no-fund
COPY server/ui ./
RUN npm test && npm run build

# ── Rust engine payload (the engine, user-directed override) ─────────────────────────────────
# `digup` + `digup-server` and their native libraries beside
# themselves ($ORIGIN) — the file set and layout the retired Swift payload shipped, built from
# the Rust workspace (crates/).
# The contract suite (`server/tests/`) runs UNCHANGED against this payload: the engine's
# published interfaces (JSON-lines verbs, schema v6, the 768-f16 vector layout, one-shot
# --json CLI) are reproduced byte-for-byte, so the suite is the port's gate. The suite
# itself builds indexes with THIS binary (the /index mount starts empty), so unlike the
# cross-check gates it measures the payload end-to-end, not a cached artifact.
#
#   docker build --target server-test-rust -f server/docker/Dockerfile -t digup-rust-test .
#
# Ubuntu 22.04, the same glibc rule the retired Swift builder kept: the binaries then ask for 2.35 at
# most, and the runtime stages (Debian 13, glibc 2.41) run NEWER — forward, never back.
# The Vendor cache mount carries its own `id` (the rust-vendor note below) and the same stamped
# scripts: the C-library compiles are shared with everything else in the repo that builds them.
FROM ${BASE_IMAGE} AS rust-engine-builder

# Stage 9's CUDA knobs: scripts/build-llama.sh picks the
# CUDA flavor by finding nvcc on PATH (a CUDA *devel* base) and pins the card with CUDA_ARCHS.
# Without this ARG/ENV pair the build-arg has no landing place and CMake gets
# `-gencode=arch=compute_,code=sm_` — the empty arch number is what "Unsupported gpu
# architecture 'compute_'" means (measured 2026-11-13, this stage's first CUDA run).
ARG CUDA_ARCHS=native
ENV CUDA_ARCHS=${CUDA_ARCHS}
ENV DEBIAN_FRONTEND=noninteractive
# clang + libclang-dev: `digup-runtime`'s build script runs bindgen over the
# vendored llama.cpp/PDFium headers, and bindgen IS libclang (measured 2026-11-13:
# the build died "Unable to find libclang" without them).
RUN apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    ca-certificates curl git gnupg xz-utils unzip python3 binutils build-essential cmake ninja-build \
    meson nasm pkg-config patchelf file zlib1g-dev libcurl4-openssl-dev libxml2-dev libedit2 \
    libsqlite3-0 libz3-dev libpython3-dev tzdata libgomp1 clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*

# The pinned Rust toolchain, installed through rustup (the distro cargo lags it by
# years): 1.99.1, the compiler this port is measured with on the dev box. The
# workspace's floor is 1.88 (Cargo.toml — sha2 0.11 requires it; a 1.85 builder
# died on exactly that, measured 2026-11-13). Profile minimal: rustc + cargo,
# no clippy, no docs.
ARG RUST_VERSION=1.99.1
RUN set -eux; \
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup.sh; \
    sh /tmp/rustup.sh -y --default-toolchain "${RUST_VERSION}" --profile minimal; \
    rm -f /tmp/rustup.sh
ENV PATH=/root/.cargo/bin:$PATH
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

WORKDIR /src
COPY . .
COPY .ci-vendor /ci-vendor

# Vendor seeding through the build context: CI caches Vendor/ host-side and passes the built
# result as `.ci-vendor/`, the scripts land on their stamps (Vendor/VENDOR, ffmpeg/BUILD,
# llama/BUILD) and skip the rebuild.
# bash for pipefail (Ubuntu's /bin/sh is dash, which has no `set -o pipefail`).
# The Vendor cache mount carries `key=rust-vendor` for a measured reason: BuildKit runs
# stages in parallel, and the retired Swift builder shared `/src/Vendor` with this one — two
# `scripts/build-llama.sh` runs in one tree race (measured 2026-11-13 on the
# server-test-rust chain: `ninja: error: unknown target 'cmTC_…'` / `loading 'build.ninja':
# No such file or directory` from cmake's compiler-id test, the Swift stage failing on the
# Rust stage's half-written build tree). A distinct `id` = a distinct volume (BuildKit
# v0.34.2 rejects `key=` outright — "unexpected key 'key'"; the attribute is `id`, the name
# the cache records print: "cached mount /src/Vendor … with id //src/Vendor"). The id stays
# so the cache volume every measured CUDA run sits on is unchanged; the scripts stay untouched.
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
RUN --mount=type=cache,target=/src/Vendor,id=rust-vendor \
    --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.cargo/registry \
    set -euxo pipefail; \
    if [ -f /ci-vendor/llama/BUILD ]; then cp -a /ci-vendor/. /src/Vendor/; fi; \
    export CC="${CC:-cc}" CXX="${CXX:-c++}"; \
    scripts/fetch-vendor.sh; \
    # The flavor guard (measured 2026-11-04, in the retired Swift builder; this stage keeps it):
    # a CUDA-flavored Vendor
    # cache carries Vendor/llama-build/CMakeCache.txt with GGML_CUDA=ON, and a CPU run
    # reusing it dies at configure with "CUDA Toolkit not found" (measured 2026-11-05).
    rm -f Vendor/llama-build/CMakeCache.txt; \
    # CUDA_ARCHS normalization:
    # CUDA_ARCHS reaches CMake as ONE -D value and CMake lists are SEMICOLON-separated, so
    # "61,70,…" is one invalid arch token and configure dies. Plain numbers, native and
    # semicolon lists pass through unchanged. scripts/build-llama.sh stays untouched.
    export CUDA_ARCHS="$(printf %s "${CUDA_ARCHS:-native}" | tr ',' ';')"; \
    # The resolver-retry wrapper (the builder's standing rule, measured on resolver hiccups):
    # these scripts are stamped and idempotent, so one flaky DNS query must not kill a 20-minute build.
    tries=0; until scripts/build-ffmpeg.sh; do tries=$((tries+1)); [ "$tries" -ge 3 ] && exit 1; \
      echo "build-ffmpeg: attempt $tries failed; retrying in 30 s"; sleep 30; done; \
    tries=0; until scripts/build-llama.sh; do tries=$((tries+1)); [ "$tries" -ge 3 ] && exit 1; \
      echo "build-llama: attempt $tries failed; retrying in 30 s"; sleep 30; done; \
    cargo build --release -p digup-cli -p digup-server; \
    # The engine answers with the C libraries beside Vendor/ (the payload's own $ORIGIN
    # shape is gated in rust-engine-payload below). PIPEFAIL is the point: the usage is
    # CAPTURED and checked non-empty, because `./digup --help | sed -n '2p'` reports SED's
    # exit status — a binary that dies "error while loading shared libraries" prints nothing
    # and the RUN passes (measured 2026-11-13 on this stage's CPU run: a green image with an
    # empty probe). Line 1 is the CLI's own title, line 3 its first verb (line 2 is blank).
    usage="$(LD_LIBRARY_PATH="/src/Vendor/llama/lib:/src/Vendor/pdfium/lib" \
      PATH="/src/Vendor/ffmpeg/bin:$PATH" ./target/release/digup --help)"; \
    test -n "$(printf '%s\n' "$usage" | sed -n '1p')"; \
    printf '%s\n' "$usage" | sed -n '1p;3p'

# The payload: the CLI + the native libraries it asks for (PDFium, llama.cpp, ffmpeg),
# rpath'd $ORIGIN — the same file set and layout the retired Swift payload shipped. The Rust
# engine loads the llama/pdfium libraries at run time (dlopen), so the payload carries them
# exactly where the Swift payload put them; ffmpeg/ffprobe ride beside the binary,
# which is how `FFmpeg.find` reaches them (beside the executable, then PATH).
FROM rust-engine-builder AS rust-engine-payload
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
RUN --mount=type=cache,target=/src/Vendor,id=rust-vendor \
    --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.cargo/registry \
    set -euxo pipefail; \
    cargo build --release -p digup-cli -p digup-server; \
    mkdir -p /out; \
    cp /src/target/release/digup /src/target/release/digup-server \
       /src/Vendor/pdfium/lib/libpdfium.so \
       /src/Vendor/ffmpeg/bin/ffmpeg /src/Vendor/ffmpeg/bin/ffprobe /out/; \
    for file in /src/Vendor/llama/lib/*.so*; do \
      name=${file##*/}; \
      case "$name" in \
        *.so.[0-9]*) [ -L "$file" ] && cp -L "$file" "/out/$name" ;; \
        *.so) [ ! -L "$file" ] && cp "$file" "/out/" ;; \
      esac; \
    done; \
    for file in /out/digup /out/*.so*; do patchelf --set-rpath '$ORIGIN' "$file"; done; \
    # The gate is the payload answering from its own folder (the $ORIGIN rpath above): line 1
    # of --help, printed (line 2 is blank, so asking sed for '2p' proves nothing by itself),
    # and PIPEFAIL so a loader failure fails the RUN. digup-server rides with digup.
    test -x /out/digup-server; \
    PATH="/out:$PATH" /out/digup --help | sed -n '1p'

# ── Runtime: the server image's base. No Swift, no compiler, no desktop stack. ───────────────
FROM ${BASE_IMAGE} AS engine-runtime

RUN apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    ca-certificates libcurl4 libgomp1 libstdc++6 tzdata \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-engine-payload /out/ /opt/digup/
# ffmpeg and ffprobe are in /opt/digup and on PATH: that is how the engine finds them
# (`executable("ffmpeg")` looks on PATH, nothing else).
ENV PATH=/opt/digup:$PATH

# The engine runs headlessly as its own user; nothing here is a desktop, a display server, or a
# service manager, and no Swift toolchain is installed. No /data/locks: the writer lock is
# beside the index (`<index>/.digup-server.writer.lock`, engine/index.py), not a folder.
RUN useradd --system --create-home --uid 990 digup \
    && mkdir -p /data/index /data/models /data/config \
    && chown -R digup:digup /data \
    && digup --help > /dev/null \
    && digup --help | sed -n '2p'

USER digup
WORKDIR /data

# ── Test harness (dev): the engine payload + the Python adapter + the contract tests ─────────
# python:3.12-slim — measured in the built image: Debian 13 (trixie), glibc 2.41. The payload
# is built on Ubuntu 22.04 (glibc 2.35), so the image runs NEWER glibc than the payload was
# compiled against — forward, never back. The same payload, the same CLI: the contract tests
# prove the *shipped* binaries, so a passing suite is a statement about the runtime image,
# not about a dev box. Runs as root: mounts arrive from the host and the scratch indexes are
# the tests' own.
FROM python:3.12-slim AS server-test

RUN apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    libcurl4 libgomp1 tini \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir uv

COPY --from=rust-engine-payload /out/ /opt/digup/
COPY server /srv/server
# The built UI (the mount serves server/ui/dist; api.py): a dist that exists is what makes
# the suite's page fetches real bytes, not skips.
COPY --from=frontend-builder /build/dist /srv/server/ui/dist
WORKDIR /srv/server
ENV PATH=/opt/digup:$PATH
ENV DIGUP_BIN=digup \
    DIGUP_SERVER_CORPUS=/corpus \
    DIGUP_SERVER_CORPUS_ROOT=/corpus-root \
    DIGUP_SERVER_MODELS=/models \
    DIGUP_SERVER_INDEX=/index
RUN uv sync --frozen

# Mounts at run time: /corpus (the corpus media), /corpus-root (the corpus folder, the
# skipped files), /models (the verified model files), /index (a writable index folder; the
# first run fills it with `digup index`).
#
# tini here is not ceremony: the suite's app spawns `search-service` as a child of the test
# process, and ParentWatch ends a helper whose parent's parent is PID 1 ("the app is gone").
# Plain `docker run` makes pytest PID 1, so every helper died at once with `exit 0, no
# stderr` — measured. With tini as PID 1 the test process is a live parent, exactly the
# production shape, and the suite needs no `--init` flag to be correct.
ENTRYPOINT ["tini", "--"]
CMD ["uv", "run", "pytest"]

# ── Server runtime: the shipping image ──────────────────────────────────────────────────
# Engine payload + API + UI. No Swift, no compiler, no dev deps, no Tauri, no desktop
# stack. python:3.12-slim — measured: Debian 13 (trixie), glibc 2.41, newer than the
# payload's build glibc 2.35 (Ubuntu 22.04) — forward, never back. tini as PID 1, non-root
# user. The supervision chain (reconnaissance §15 item 8):
# tini (PID 1) → uvicorn → helpers; a helper's parent is uvicorn (never PID 1, so
# ParentWatch accepts it), and when uvicorn dies the helpers reparent to PID 1 and end —
# a stopped API takes its whole process tree with it, which is the point of the mechanism.
FROM python:3.12-slim AS server-runtime

RUN apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    ca-certificates libcurl4 libgomp1 tzdata tini \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir uv

COPY --from=rust-engine-payload /out/ /opt/digup/
ENV PATH=/opt/digup:$PATH
COPY server /srv/server
COPY server/docker/healthcheck.py /opt/digup/healthcheck.py
# The built UI: FastAPI serves /srv/server/ui/dist at `/` — static files only, no Node
# in this image (the frontend-builder stage is where the build happened).
COPY --from=frontend-builder /build/dist /srv/server/ui/dist
WORKDIR /srv/server
RUN uv sync --frozen --no-dev

# The same engine user as the engine-runtime stage (uid 990); /data is README §5's layout
# (index/, models/, config/) — bind-mounted volumes, owned by the server user. The writer
# lock lives beside the index (`<index>/.digup-server.writer.lock`), not in a data folder.
# `digup --help` proves the CLI runs headless in this image; §3's full acceptance — the
# CLI answering status and searches from the mounted folders — is proven by CI's boot step
# and the proof battery, which run against a live index, not from an empty image.
RUN useradd --system --create-home --uid 990 digup \
    && mkdir -p /data/index /data/models /data/config \
    && chown -R digup:digup /data /srv/server \
    && digup --help > /dev/null \
    && /srv/server/.venv/bin/python -c "import fastapi, uvicorn, digup_server.api" \
    && ! /srv/server/.venv/bin/python /opt/digup/healthcheck.py

ENV PATH=/srv/server/.venv/bin:$PATH \
    PYTHONPATH=/srv/server \
    DIGUP_SERVER_INDEX=/data/index \
    DIGUP_SERVER_ROOTS=/media/photos \
    DIGUP_SERVER_MODELS=/data/models \
    DIGUP_SERVER_CONFIG=/data/config
# Host and port are NOT env here: the CMD below binds them (DIGUP_SERVER_HOST/PORT feed
# `python -m digup_server.api`, the bare-metal entry — uvicorn's own args win in a container).

USER digup
WORKDIR /data
EXPOSE 8787

# SIGTERM (docker stop): tini forwards it to uvicorn, whose lifespan shutdown marks a
# running job *interrupted* (the truthful state) and quit()s every helper. The healthcheck
# asks the API's own /health — a healthy container is one whose engine answers. 503 counts
# as alive: /health answers 503 while the searcher is respawning (a refused search, the
# engine's own words) — that is the server honestly saying "not right now", not a sick
# container, and docker must not restart a live server over it. urlopen raises HTTPError
# on 503, so it is caught, not propagated.
HEALTHCHECK --interval=30s --timeout=10s --start-period=40s --retries=3 \
    CMD python /opt/digup/healthcheck.py

ENTRYPOINT ["tini", "--"]
CMD ["uvicorn", "--factory", "digup_server.api:app_factory", "--host", "0.0.0.0", "--port", "8787"]

# ── Server runtime WITH the CUDA backend (Stage 9 — built only on the box that has the card) ──
# The same server-runtime plus exactly the CUDA runtime libraries `ldd` of libggml-cuda.so
# asks for, shipped beside the payload (its rpath `$ORIGIN` resolves them). `libcuda.so.1`
# stays unresolved by design: it comes from the HOST driver through the NVIDIA Container
# Toolkit (docs/implementation/gpu-handoff.md). Two matching build-args (the devel base that
# builds the backend, the runtime base whose libraries ship), target this stage:
#
#   docker build --target server-runtime-cuda \
#     --build-arg BASE_IMAGE=nvidia/cuda:11.8.0-devel-ubuntu22.04 \
#     --build-arg CUDA_RUNTIME_IMAGE=nvidia/cuda:11.8.0-runtime-ubuntu22.04 \
#     --build-arg CUDA_ARCHS=61 …        # plain number, or a fat list `61,70,…`: the builder's
#                                        # apt cmake 3.22.1 rejects dotted 6.1 and native alike
#                                        # (measured 2026-11-04); commas are normalized to
#                                        # CMake's semicolons in the rust-engine-builder RUN
#                                        # (measured 2026-11-05, publish run #6)
#
# The `cuda-libs` stage is pulled only by this target: CPU builds (target server-runtime)
# prune it and never touch a CUDA image. The ldd gate below fails the build if a shipped
# library does not resolve — ship exactly these, from a CUDA *runtime* base, never the
# toolkit, never a second copy of libggml*.
FROM ${CUDA_RUNTIME_IMAGE} AS cuda-libs
# Copy by glob, never by hand-typed filenames: the CUDA 11.8 runtime image ships exactly the
# SONAMEs ggml-cuda asks for — libcublas.so.11 -> libcublas.so.11.11.3.6, libcublasLt.so.11 ->
# libcublasLt.so.11.11.3.6, libcudart.so.11.0 -> libcudart.so.11.8.89, libnvrtc.so.11.2 and
# libnvrtc-builtins.so.11.8 -> their .so.11.8.89 (measured on this box 2026-11-04, from
# nvidia/cuda:11.8.0-runtime-ubuntu22.04). The dotted names (libcublasLt.so.11.8, …) belong to
# the -dev packages and are NOT in a runtime base: a COPY of them dies with "not found" —
# measured. `cp -a` carries each symlink together with its target, and the glob travels with
# the CUDA version, so the stage needs no retyping when the base moves.
RUN set -eux; \
    dir=""; \
    for base in /usr/local/cuda /usr/local/cuda-*; do \
      for target in "$base"/targets/*/lib; do [ -d "$target" ] && { dir="$target"; break; }; done; \
      [ -n "$dir" ] && break; \
    done; \
    test -n "$dir"; \
    mkdir -p /out; \
    for lib in cudart cublas cublasLt nvrtc nvrtc-builtins; do cp -a "$dir/lib${lib}.so"* /out/; done; \
    # libnccl.so.2 (measured in the 11.8 runtime base as /usr/lib/x86_64-linux-gnu/libnccl.so.2
    # -> 2.15.5, 268 M, package libnccl2 …+cuda11.8 — outside the targets dir): libggml-cuda
    # links it (multi-GPU comms), ldd asks for it, so the ldd gate below needs it. A single
    # GPU never calls nccl, but an unresolved DT_NEEDED fails the load.
    nccl_found=""; \
    for loc in "$dir" "/usr/lib/$(uname -m)-linux-gnu" /usr/lib/x86_64-linux-gnu; do \
      if [ -e "$loc/libnccl.so.2" ]; then cp -a "$loc"/libnccl.so* /out/ && nccl_found=1 && break; fi; \
    done; \
    test -n "$nccl_found"

FROM server-runtime AS server-runtime-cuda
COPY --from=cuda-libs /out/ /opt/digup/
RUN ldd /opt/digup/libggml-cuda.so \
    && ! ldd /opt/digup/libggml-cuda.so | grep 'not found' | grep -v 'libcuda.so.1'


# The contract suite's Rust-payload name: server-test now CARRIES the Rust payload (the
# engine-builder/engine-payload Swift stages are retired), so this stage is the name the
# CI jobs and the agent notes use for the suite image — a FROM alias, zero added layers.
# The suite is unchanged: same tests, same mounts, same zero-skip bar. This is stage 4f's gate.
FROM server-test AS server-test-rust

# The shipping image with the Rust engine (the deploy compose can target it unchanged):
# same alias shape — server-runtime's payload COPY is the Rust one, so the name stays for
# the CI steps and the compose override that target it, with no layers of its own.
FROM server-runtime AS server-runtime-rust

# ── Stage 6: the shipping image with the Rust server (no Python, no uvicorn) ──────────────────
# The pure-Rust shape: `digup-server` (crates/server, the axum port of the FastAPI wrapper) +
# the Rust engine payload + the Lit bundle, supervised by tini. The user's Stage-6 sketch
# (`rust:1.78-bookworm` builder, `nvidia/cuda:12.1.0-runtime`, `digup server --host 0.0.0.0
# --port 8000`, image `ocean572/digup:latest`) reconciles with the repo's measured conventions:
# the compiler is the pinned `RUST_VERSION` in rust-engine-builder (1.99.1 — the workspace's
# `rust-version = "1.88"` floor kills a 1.78 toolchain on sha2 0.11, measured 2026-11-13, the
# same way a 1.85 builder died); the CUDA base is `CUDA_RUNTIME_IMAGE`, whose default is the
# CUDA the box's card speaks (11.8 for sm_61, docs/implementation/gpu-handoff.md — a 12.1
# runtime base cannot satisfy an sm_61 build of that era); the port is the repo's 8787
# (EXPOSE + compose), bound through the config env, not argv; the image is pushed as
# `ghcr.io/ocean572/digup` (the appcast/CI naming) and `ocean572/digup:latest` is its alias.
#
#   docker build --target server-rust-runtime -f server/docker/Dockerfile -t digup-rust:local .
#
# The supervision chain, unchanged from the measured Swift-era runtime image: tini (PID 1) → digup-server → the
# engine's helpers (search-service/index-service are spawned by digup-server; ParentWatch
# ends a helper whose parent's parent is PID 1 — so a stopped API takes its whole tree with
# it, by design). SIGTERM (docker stop): digup-server drains, runs the lifespan's shutdown
# (the runners record a running job *interrupted*, `quit()`s every helper) and exits 143 —
# the measured uvicorn parity line, and compose's `stop_grace_period: 90s` is the same contract.
FROM ${BASE_IMAGE} AS server-rust-runtime

RUN apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    ca-certificates curl libcurl4 libgomp1 libstdc++6 tzdata tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-engine-payload /out/ /opt/digup/
ENV PATH=/opt/digup:$PATH
# The Lit bundle, baked in (the frontend-builder stage is the only place it is built — a
# host `dist` is git-ignored and dockerignored, so a stale host build can never ship
# silently). It rides INSIDE the payload folder, beside `digup-server` — the Rust
# server's default UI mount is install-relative like Python's (`Path(__file__)-
# ../../ui/dist`, api.py:936): resolved beside the running binary, so this layout
# mounts with no knob at all. `DIGUP_SERVER_UI` remains the operator's override
# (config.rs); an explicit value with no index.html refuses startup.
# The COPY IS the mount: this stage's own gate below tests for
# /opt/digup/ui/dist/index.html (the audit's first CUDA run of THIS stage failed on
# exactly that line — the COPY was missing while the gate already demanded the file;
# CI never caught it because CI exercises server-test-rust/server-runtime-rust, not
# this stage).
COPY --from=frontend-builder /build/dist /opt/digup/ui/dist

# The same engine user (uid 990) and the same /data layout as every shipping stage: index /
# models / config are bind mounts, so recreation never destroys them, and the writer lock
# lives beside the index (`<index>/.digup-server.writer.lock`).
RUN useradd --system --create-home --uid 990 digup \
    && mkdir -p /data/index /data/models /data/config \
    && chown -R digup:digup /data \
    && digup --help > /dev/null \
    && digup --help | sed -n '2p' \
    && test -f /opt/digup/digup-server \
    && test -f /opt/digup/ui/dist/index.html

# Host + port come from the config env (DIGUP_SERVER_HOST/PORT, config.rs:221): the compose
# base sets them for the FastAPI image, so the same env drives this one. 0.0.0.0 is the
# container's own face; a bare-metal run keeps the 127.0.0.1 default.
ENV DIGUP_SERVER_INDEX=/data/index \
    DIGUP_SERVER_ROOTS=/media/photos \
    DIGUP_SERVER_MODELS=/data/models \
    DIGUP_SERVER_CONFIG=/data/config \
    DIGUP_SERVER_HOST=0.0.0.0 \
    DIGUP_SERVER_PORT=8787

USER digup
WORKDIR /data
EXPOSE 8787

# The healthcheck is the runtime stages' rule with curl instead of Python (no interpreter in
# this image): ask the API's own /api/v1/health (server/docker/healthcheck.py asks the same
# path), and count 503 as ALIVE — /health answers 503 while the searcher warms or respawns
# (the engine honestly saying "not right now"), and docker must not restart a live server
# over it. curl answers a 5xx with exit 22; 200 is exit 0; anything else (a refused
# connection, a 4xx, a timeout) is the failure.
HEALTHCHECK --interval=30s --timeout=10s --start-period=40s --retries=3 \
    CMD curl -fsS -o /dev/null http://127.0.0.1:8787/api/v1/health || test $? -eq 22

ENTRYPOINT ["tini", "--"]
CMD ["digup-server"]

# ── Stage 6: the same image with the CUDA backend (built and measured on the box that has the
# card, same two build-args as server-runtime-cuda above) ──────────────────────────────────────
# The Rust engine loads ggml's CUDA module the way its backend.rs defines it (backend.rs:
# `$DIGUP_BACKENDS`, then beside the executable), so the CUDA payload is the same file set:
# libggml-cuda.so from the Vendor build (rust-engine-payload copies every Vendor .so, the
# CUDA flavor among them when the builder ran on a CUDA devel base) plus exactly the CUDA
# runtime libraries `cuda-libs` ships — never the toolkit, never a second libggml*, and
# `libcuda.so.1` stays the host driver's, never baked in.
FROM server-rust-runtime AS server-rust-runtime-cuda
COPY --from=cuda-libs /out/ /opt/digup/
RUN ldd /opt/digup/libggml-cuda.so \
    && ! ldd /opt/digup/libggml-cuda.so | grep 'not found' | grep -v 'libcuda.so.1'
