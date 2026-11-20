"""The API reference stays true to the app (docs/API.md is the compatibility promise).

`create_app()` builds the routes without spawning anything — the engine only starts inside the
lifespan — so this is a cheap tripwire that needs no engine, no corpus and no container: add an
endpoint and forget the docs, and this fails. The parameter *names* in the doc may differ from
the code's (`{id}` vs `{job_id}`), so each `{parameter}` segment matches any non-slash token.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from conftest import _cfg
from support import live_server

DOC = Path(__file__).resolve().parent.parent / "docs" / "API.md"


@pytest.fixture(scope="module")
def api_surface(tmp_path_factory):
    """The live server's published surface: `openapi.json` is the route table the binary
    answers with — the same tripwire `create_app` gave, read from the shipping product."""
    roots = tmp_path_factory.mktemp("docs-roots")
    with live_server(_cfg(tmp_path_factory.mktemp("docs-index"), (roots,)),
                     config_dir=str(tmp_path_factory.mktemp("docs-config"))) as c:
        yield c


def app_paths(c) -> list[str]:
    return sorted({p2 for p2 in c.get("/api/v1/openapi.json").json()["paths"]})


def test_every_api_endpoint_is_documented(api_surface):
    text = DOC.read_text(encoding="utf-8")
    missing = []
    for path in app_paths(api_surface):
        pattern = "/".join("[^/`]+" if seg.startswith("{") else re.escape(seg)
                           for seg in path.split("/"))
        if not re.search(pattern, text):
            missing.append(path)
    assert not missing, f"endpoints missing from server/docs/API.md: {missing}"


def test_the_documented_default_limit_is_the_models_default():
    """The doc's `limit` row and the model's default cannot drift apart (step 1's decision:
    the API default equals the engine's own default, 40)."""
    # The default the doc must carry is the engine's own (40) — the same fact
    # test_api's default-limit test pins on the HTTP leg (unnamed limit == explicit 40).
    default = 40
    rows = [line for line in DOC.read_text(encoding="utf-8").splitlines()
            if line.startswith("| `limit` |")]
    assert rows, "API.md documents no `limit` parameter row"
    assert all(f"**{default}**" in line for line in rows), \
        f"API.md's limit row(s) do not carry the model's default ({default}): {rows}"


def test_the_api_version_and_the_openapi_surface_are_named_in_the_docs(api_surface):
    text = DOC.read_text(encoding="utf-8")
    for url in ("/api/v1/openapi.json", "/api/v1/docs"):
        assert api_surface.get(url).status_code == 200, url   # the surface is live, not promised
        assert url in text, f"API.md does not name {url}"
