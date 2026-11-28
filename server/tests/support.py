"""The live-shape harness (docs/implementation/python-free-plan.md P3).

The contract suite drives the SHIPPING server: `digup-server` (the Rust product)
spawned as a child process with exactly the env names the compose ships — the
suite is a thin client of the binary, the same relationship every UI has to it.

ParentWatch keeps its measured meaning: the API's helpers are children of the
spawned server, whose parent is the test process — a killed server takes its
helper tree with it (the same chain the container runs: tini → server → helpers).

`stop()` sends SIGTERM — the stop contract gate 1 measured: a running job
records *interrupted* with its counters inside the grace and the process ends
143. `restart()` boots the same folders again, which is how the log-replay
contracts (interrupted recovery, resume) are read.
"""

from __future__ import annotations

import os
import signal
import socket
import subprocess
import tempfile
import time

import httpx

from engine import EngineConfig

# The server binary: the image ships it on PATH (/opt/digup); DIGUP_SERVER_BIN names
# it for a host run. The engine binary stays DIGUP_BIN (conftest's own knob).
SERVER_BIN_ENV = "DIGUP_SERVER_BIN"


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = int(s.getsockname()[1])
    s.close()
    return port


class DeadServerError(RuntimeError):
    """The spawned server died — the log tail rides in the message (the CI lesson:
    a dead server with no words is an hour of guessing)."""


class _LiveClient(httpx.Client):
    """The plain client with the same lesson baked in for MID-RUN deaths: a connection
    the server closes without an answer (`Server disconnected without sending a
    response`) is the server process dying between requests, and on a runner that
    traceback names nothing. The server's own log tail rides the exception, in the
    SAME exception type the suite's transports raise (a pin that expects a transport
    error keeps its type; every HTTP answer — 200/401/403/503 — rides untouched)."""

    def __init__(self, *args, tail_fn=None, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        self._tail_fn = tail_fn

    def send(self, request, **kwargs):
        try:
            return super().send(request, **kwargs)
        except (httpx.RemoteProtocolError, httpx.ConnectError) as exc:
            tail = self._tail_fn() if self._tail_fn is not None else "(no log)"
            raise type(exc)(f"{exc}\nserver log tail:\n{tail}") from exc


class LiveApp:
    def __init__(self, cfg: EngineConfig, *, config_dir: str | None = None,
                 env_extra: dict[str, str | None] | None = None,
                 ready_timeout: float = 120.0):
        self.cfg = cfg
        self.own_config = config_dir is None
        self.config_dir = config_dir if not self.own_config else tempfile.mkdtemp(
            prefix="digup-suite-config-")
        # The server writes its job log / server.log here: create the folder, the way
        # the compose deployment creates `<config>` before the first boot (the tests
        # hand in tmp dirs that do not exist yet — measured: opening server.log in a
        # missing folder is a FileNotFoundError, not a server failure).
        os.makedirs(self.config_dir, exist_ok=True)
        self.env_extra = dict(env_extra or {})
        self.ready_timeout = ready_timeout
        self.port = _free_port()
        self.log_path = os.path.join(self.config_dir, "server.log")
        self.proc: subprocess.Popen | None = None
        self.client: httpx.Client | None = None

    # ── the env the container ships ────────────────────────────────────────────
    def _env(self) -> dict[str, str]:
        env = dict(self.cfg.env())                     # inherits os.environ, DIGUP_MODELS
        env.update({
            "DIGUP_SERVER_HOST": "127.0.0.1",
            "DIGUP_SERVER_PORT": str(self.port),
            "DIGUP_SERVER_INDEX": str(self.cfg.index),
            "DIGUP_SERVER_ROOTS": ":".join(self.cfg.roots),
            "DIGUP_SERVER_CONFIG": str(self.config_dir),
        })
        for name, value in self.env_extra.items():
            if value is None:
                env.pop(name, None)
            else:
                env[name] = str(value)
        return env

    def start(self) -> "LiveApp":
        binary = os.environ.get(SERVER_BIN_ENV, "digup-server")
        log = open(self.log_path, "ab")
        self.proc = subprocess.Popen([binary], env=self._env(),
                                     stdout=log, stderr=subprocess.STDOUT)
        self.log = log
        self.client = _LiveClient(base_url=f"http://127.0.0.1:{self.port}", timeout=10.0,
                                  tail_fn=self.tail)
        deadline = time.monotonic() + self.ready_timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise DeadServerError(
                    f"{binary} exited rc={self.proc.returncode} during boot:\n{self.tail()}")
            try:
                # ANY HTTP answer means the server is up and speaking: 200 healthy,
                # 503 engine warming (the honest "not right now"), 400/401/403 the
                # opt-in guards refusing THIS probe's plain Host/key — the guarded
                # fixtures spawn servers whose guard rejects a default-Host request,
                # and readiness must not mistake the guard for a dead server.
                self.client.get("/api/v1/health")
                return self
            except httpx.HTTPError:
                pass                                         # connection refused: not yet
            time.sleep(0.2)
        raise DeadServerError(f"{binary} never answered /health:\n{self.tail()}")

    def stop(self, *, sig: int = signal.SIGTERM, timeout: float = 60.0) -> int:
        """SIGTERM: the measured graceful stop (interrupted + 143). SIGKILL: the kill
        path (137, log replay recovers *interrupted* at the next boot)."""
        if self.proc is None:
            return -1
        proc, self.proc = self.proc, None
        if self.client is not None:
            self.client.close()
            self.client = None
        proc.send_signal(sig)
        try:
            rc = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.kill()
            rc = proc.wait(timeout=10)
        self.log.close()
        return rc

    def restart(self) -> "LiveApp":
        self.start()
        return self

    def tail(self, chars: int = 1200) -> str:
        try:
            with open(self.log_path, "rb") as fh:
                return fh.read()[-chars:].decode("utf-8", "replace")
        except OSError:
            return "(no log)"

    def token_file(self) -> dict:
        import json
        with open(os.path.join(self.config_dir, "tokens.json"), encoding="utf-8") as fh:
            return json.load(fh)

    def bearer(self, token: str | None) -> dict[str, str]:
        return {} if token is None else {"Authorization": f"Bearer {token}"}

    # context-manager shape: the plain client fixtures
    def __enter__(self) -> httpx.Client:
        return self.start().client

    def __exit__(self, *exc) -> None:
        self.stop()


def live_server(cfg: EngineConfig, **kw):
    """`with live_server(cfg) as client:` — the fixture shape of the plain modules."""
    return LiveApp(cfg, **kw)
