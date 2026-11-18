# Python-free plan — flipping the product to pure Rust (drafted 2026-11-18)

**Status: plan only, nothing here is binding until its phase runs; the binding § rules
(`README.md` of this folder) keep their precedence throughout.**

The Rust server already exists and is measured: `crates/server` is the axum port of
`server/digup_server/api.py` (full endpoint surface, PrincipalLog → Cors → HostOriginGuard →
BearerAuth, the media-handle `HitCache`, the flock-guarded job runner, minted tokens, the UI
mount), and the contract suite runs green against it as the `server-test-rust` target of the
Server chain. The Dockerfile already carries **stage 6: the shipping image with the Rust
server (no Python, no uvicorn), `CMD ["digup-server"]`**. The desktop payloads ship
`digup-server` today; the only Python that ships is the Docker server image
(`CMD ["uvicorn", …digup_server.api:app_factory…]`).

So this is a **flip, not a conversion**. The goal: everything that ships is Rust (engine,
server, CLI, shell, payload libraries) + the Lit UI (JS by design). Python stays only where
it is a harness: the contract suite (`server/tests/`) and the fixture generators
(`scripts/*.py`) — test code, vendored, never shipped.

## P0 — the Packages chain lands green (gates everything below)

Current: installer/AppImage gates in the fix loop (helper probe, MSVC OpenMP OFF, `libgomp1`,
gate dumps `worker.log`/index folder/mount roots on the red paths).
**Gate:** all Packages jobs green on one tip — Windows installer check, Linux deb check,
AppImage checks (ubuntu:22.04, ubuntu:24.04, debian:12), macOS .app/.dmg + signature —
artifacts dragged onto v0.8.0. No P1 work merges while a packages gate is red.

## P1 — measure the Rust server as the deploy shape (this box, measured, not assumed)

1. **Stop semantics** — the README's measured contract rerun against `digup-server`:
   `docker stop` with `stop_grace_period: 90s` → exit **143** + shutdown-complete line + the
   running job recorded *interrupted* with partial counters, next job resumes. 137 on a
   graceful stop = red.
2. **Health/warm behavior** — 503 while the searcher warms, healthcheck counts 503 alive;
   exactly-one-process invariants (media-id LRU, job runner, flock, 409 busy) under the
   suite's shapes plus a load check with two named targets.
3. **Bench stack on the Rust image** — full file chain AND every boot env var (never
   defaults — the measured recreation rule), headless proof (index + search + eval answered
   by the engine's own words), Nextcloud pilot shape (kinds filter, dav profile) against
   `digup-server` on the pilot's :8791 contract.
4. ParentWatch (helpers exit at PID 1, `tini`, `--init`) verified for the Rust spawner.

**Gate:** items 1–4 answered with numbers on this box, filed in `server/IMPLEMENTATION_STATUS.md`.

## P2 — deploy default flips to the Python-free image

1. `deploy/docker-compose.yml`: base service → stage-6 target (the Rust server image). The
   base-file byte-identity discipline ends here **by this user-directed change only**; every
   override shape (`.ghcr`, `.nvidia`, `.lan`, `.app-targets`, `.nextcloud`, `.all-in-one`)
   revalidated with `docker compose config --quiet`, with AND without profiles.
2. FastAPI becomes an **opt-in parity override** (a `.fastapi.yml` compose override + the
   existing FastAPI test alias) — one release cycle as the measured escape hatch, so any
   divergence is reproducible against the Python baseline, not argued.
3. server.yml: the suite keeps running against BOTH aliases for this cycle (it already
   gates `server-test-rust`); the boot-check and headless-proof steps move to the Rust image.
4. Tokens/named indexes/jobs/preview: pilot + suite numbers posted; UI served by the Rust
   mount (the Lit bundle served end-to-end pinned).

**Gate:** bench + CI chains green on the flipped default; the FastAPI alias green in
parallel (parity); zero skipped.

## P3 — retire FastAPI (one release after P2, deletion commit)

1. Delete `server/digup_server/` (the uvicorn factory), the Python deps of the shipping
   image (uv/pyproject in server stages), `healthcheck.py` (the stage-6 curl healthcheck
   already replaces it), the FastAPI compose override, the FastAPI suite alias, and the
   FastAPI path pins in `server/docs/API.md` (the API contract itself is unchanged).
2. Docs follow in the same commit: `ARCHITECTURE_MAP.md` seams, `IMPLEMENTATION_STATUS.md`,
   the README § notes that describe the wrapper as Python, the server.yml comments.
3. `.gitattributes` marks `server/tests/**` and `scripts/**` `linguist-vendored` — the
   language bar then reads the product: Rust + JS, no Python counted.
4. The contract suite stays pytest, unchanged, against `server-test-rust` (now the only
   alias); the generators stay. This is deliberate: the harness is not the product, and the
   0-skipped bar that protects every future change lives there.

**Gate:** suite 0 failed / 0 skipped against the Rust server alone; shipping image contains
no Python (checked in the image-build step); packages chain stays green on the same tip.

## P4 — what stays Python, on purpose (recorded, not a TODO)

* `server/tests/` (pytest contract suite) — the gates; runs in CI containers that carry
  Python as a harness. The test image is a CI artifact, not a shipping image.
* `scripts/*.py` (testbed/fixture/notices generators) — the suite's raw material.
* Node/Vite (the Lit UI) — product JS, untouched.

Migrating these to Rust is explicitly NOT planned: it converts the tool that catches
regressions into the change being regressed, and buys nothing measurable. If the user ever
wants the repo Python-free including the harness, that is a separate, larger project
(re-deriving the suite's pins in a Rust integration harness) — recorded here so the choice
stays visible, not implied.

## Sequencing rule

P0 → P1 → P2 → P3 strictly. Each phase's gate numbers land in `IMPLEMENTATION_STATUS.md`
(and this file's status line) before the next phase's first commit merges. Git safety
(README §9) holds throughout: no commits/pushes without explicit ask, `upstream` untouched.
