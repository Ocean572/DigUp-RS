#!/usr/bin/env python3
"""Writes the update feeds DigUp for Windows and Linux check (plugins.updater.endpoints in shell/src-tauri/tauri.conf.json:
updates/<os>-<arch>.json, one file per platform, so a release for one system alone moves only that system's feed): the
release's version, notes, date, the file to download and its signature.

    scripts/update-feed.py <folder> <updates folder> [--notes notes.txt] [--tag v<version>] [--also-latest]

<folder> holds the packages the release publishes with their .sig files, as the packages workflow makes them with the
signing key (DigUp_<version>_x64-setup.exe and/or DigUp_<version>_amd64.AppImage, each with a .sig): a feed is written
for each one found. The Windows installer and the AppImage update themselves; a .deb install is told of the update by
the AppImage's entry and opens the release page. --also-latest writes updates/latest.json too, with every platform
found: the one feed copies from before 0.7.0-beta.2 read (needed as long as some may still be out there; a release of
one platform alone can't write it). Downloads point at GitHub Releases ($DOWNLOAD_BASE, by default
https://github.com/ARahim3/DigUp/releases/download/<tag>/, the tag v<version>). Notes are plain text, shown in the
update's dialog. Publishing the packages and then the feeds (in that order: installed copies read the feeds from the
repository's main branch) is a separate step.
"""
import argparse
import datetime
import json
import os
import re
import sys
from pathlib import Path

# Platform keys the updater looks up: `{os}-{arch}` (a .deb install finds the AppImage's entry, reads only its version).
# The macOS app is a .app.tar.gz the updater un-tars; a universal build answers both slices' keys.
PLATFORMS = {
    "windows-x86_64": re.compile(r"^DigUp_(?P<version>[^_]+)_x64-setup\.exe$"),
    "linux-x86_64": re.compile(r"^DigUp_(?P<version>[^_]+)_amd64\.AppImage$"),
    "macos-aarch64": re.compile(r"^DigUp_(?P<version>[^_]+)_aarch64\.app\.tar\.gz$"),
    "macos-x86_64": re.compile(r"^DigUp_(?P<version>[^_]+)_x86_64\.app\.tar\.gz$"),
    "macos-universal": re.compile(r"^DigUp_(?P<version>[^_]+)_universal\.app\.tar\.gz$"),
}
# A universal macOS build is one artifact read by both slices' keys: the updater asks
# {os}-{arch} with the runtime slice, so the universal package writes both feeds.
MACOS_SLICES = {"macos-universal": ("macos-aarch64", "macos-x86_64")}
# latest.json serves the copies from before the per-platform feeds: Windows and Linux,
# the platforms that existed then (macOS feeds ride the per-platform keys only).
LATEST_PLATFORMS = ("windows-x86_64", "linux-x86_64")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("folder")
    parser.add_argument("updates", help="the folder the feeds are written to (updates/ in the repository)")
    parser.add_argument("--notes", help="a plain text file: what's new, for the update's dialog")
    parser.add_argument("--tag", help="the release's tag (default: v<version>)")
    parser.add_argument("--also-latest", action="store_true",
                        help="write latest.json too, for copies from before the per-platform feeds")
    args = parser.parse_args()

    folder = Path(args.folder)
    platforms, versions = {}, set()
    for key, pattern in PLATFORMS.items():
        found = [path for path in folder.iterdir() if pattern.match(path.name)]
        if not found:
            continue
        if len(found) > 1:
            sys.exit(f"update-feed: more than one {pattern.pattern} in {folder}")
        package = found[0]
        signature = package.with_name(package.name + ".sig")
        if not signature.is_file():
            sys.exit(f"update-feed: {package.name} has no {signature.name}: build the packages with the signing key")
        versions.add(pattern.match(package.name)["version"])
        platforms[key] = {"package": package.name, "signature": signature.read_text().strip()}
    # The universal macOS package answers both slices' feed keys (the updater asks with
    # its runtime slice: macos-aarch64 on Apple Silicon, macos-x86_64 on Intel).
    for key, slice_keys in MACOS_SLICES.items():
        if key in platforms:
            for feed_key in slice_keys:
                platforms[feed_key] = platforms[key]
    if not platforms:
        sys.exit(f"update-feed: no package in {folder} (an installer or an AppImage, with its .sig)")
    if len(versions) != 1:
        sys.exit(f"update-feed: the packages are of different versions: {sorted(versions)}")
    version = versions.pop()
    tag = args.tag or f"v{version}"
    base = os.environ.get("DOWNLOAD_BASE", f"https://github.com/ARahim3/DigUp/releases/download/{tag}/")
    notes = Path(args.notes).read_text().strip() if args.notes else ""
    date = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")

    def feed(keys):
        return {"version": version, "notes": notes, "pub_date": date,
                "platforms": {key: {"signature": platforms[key]["signature"], "url": base + platforms[key]["package"]}
                              for key in keys}}

    out = Path(args.updates)
    out.mkdir(parents=True, exist_ok=True)
    written = {key: out / f"{key}.json" for key in platforms}
    if args.also_latest:
        if not set(LATEST_PLATFORMS) <= set(platforms):
            sys.exit("update-feed: latest.json needs the Windows and Linux packages (it's the feed for old copies)")
        written["latest"] = out / "latest.json"
    for name, path in written.items():
        keys = list(platforms) if name == "latest" else [name]
        path.write_text(json.dumps(feed(keys), indent=2, ensure_ascii=False) + "\n")
        print(f"{path}: DigUp {version}, " + ", ".join(f"{key} → {base + platforms[key]['package']}" for key in keys))


if __name__ == "__main__":
    main()
