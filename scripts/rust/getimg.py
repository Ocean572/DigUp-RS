#!/usr/bin/env python3
"""One Wikimedia Commons picture per semantic subject (public fixtures for the
Linux testbed, scripts/rust/make-testbed-linux.py): search Commons, take the
first File: hit that is a JPEG/PNG wide enough, download it through the API's own
URL — a UA-policy compliant string, no key, no upload, public content only."""
import json
import os
import sys
import urllib.parse
import urllib.request

UA = "DigUpTestbedBuilder/1.0 (contact: dev@example.org) python-urllib"


def api(params):
    url = "https://commons.wikimedia.org/w/api.php?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as reply:
        return json.load(reply)


def subject_url(search, min_width=700):
    found = api({
        "action": "query", "list": "search", "srsearch": search,
        "srnamespace": "6", "srlimit": "10", "format": "json",
    })
    titles = [h["title"] for h in found.get("query", {}).get("search", [])]
    if not titles:
        raise SystemExit(f"no Commons file found for {search!r}")
    info = api({
        "action": "query", "titles": "|".join(titles[:10]),
        "prop": "imageinfo", "iiprop": "url|size|mime", "format": "json",
    })
    pages = info.get("query", {}).get("pages", {})
    for title in titles:  # search rank order
        page = next((p for p in pages.values() if p.get("title") == title), None)
        if not page or "imageinfo" not in page:
            continue
        item = page["imageinfo"][0]
        if item.get("mime", "") not in ("image/jpeg", "image/png"):
            continue
        if item.get("width", 0) < min_width:
            continue
        return item["url"]
    raise SystemExit(f"no usable Commons file for {search!r}")


def get(url, out_path):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as reply:
        data = reply.read()
    with open(out_path, "wb") as file:
        file.write(data)
    return len(data)


def main():
    out_dir = sys.argv[1]
    os.makedirs(out_dir, exist_ok=True)
    fails = []
    for spec in sys.argv[2:]:
        out, search = spec.split("=", 1)
        try:
            written = get(subject_url(search), os.path.join(out_dir, out))
            print(f"ok  {out}  {written}")
        except (SystemExit, Exception) as e:
            print(f"FAIL {out}: {e}")
            fails.append(out)
    if fails:
        sys.exit(1)


if __name__ == "__main__":
    main()
