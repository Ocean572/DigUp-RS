#!/usr/bin/env python3
"""The third-party notices for the Rust payload (scripts/rust/package-*.sh): the licenses
of the libraries the payload ships, from their own files, and of every Rust crate the
workspace's binaries are built from (Cargo's own record, normal dependencies only).

    scripts/rust/notices-rust.py linux|windows|macos <out.txt>

The twin of scripts/notices.py for the Rust port: the same Vendor/ license sources
(llama.cpp, PDFium, ffmpeg + dav1d + zlib, stb, miniz, SQLite, ICU), and the crate graph
of THIS workspace (`crates/`, digup + digup-server) instead of the Tauri shell's — the
payload has no Swift runtime and no shell in it, so Swift's and WebView2's sections are
not its story. Run after scripts/fetch-vendor.sh, scripts/build-llama.sh and
scripts/build-ffmpeg.sh: it stops if a license it needs is missing.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
VENDOR = ROOT / "Vendor"
RULE = "=" * 80
LICENSE_FILE = re.compile(r"(?i)^(licen[cs]e|copying|copyright|notice|unlicense)([-_.].*)?$")
TARGET = {"linux": "x86_64-unknown-linux-gnu", "windows": "x86_64-pc-windows-msvc",
          "macos": "aarch64-apple-darwin"}


def read(path):
    return Path(path).read_text(encoding="utf-8", errors="replace").strip()


def need(path):
    path = Path(path)
    if not path.is_file():
        sys.exit(f"notices: missing {path} (run the vendor and ffmpeg scripts first)")
    return read(path)


def if_present(path, note):
    """Vendor/ holds the C libraries the non-macOS payload ships; on macOS
    scripts/fetch-vendor.sh fetches none (Apple's own frameworks read PDFs, pictures
    and zips), so those sections name the library and record its license for the
    builds the OTHER systems ship — with no file to read, the note says so."""
    path = Path(path)
    return read(path) if path.is_file() else note


def section(title, *texts):
    return "\n".join([RULE, title, RULE, "", *[t.strip() + "\n" for t in texts if t]])


def ffmpeg_versions():
    script = read(ROOT / "scripts/build-ffmpeg.sh")
    return (re.search(r"^FFMPEG_VERSION=(\S+)", script, re.M).group(1),
            re.search(r"^DAV1D_VERSION=(\S+)", script, re.M).group(1))


def stb_license():
    header = need(VENDOR / "stb/include/stb_image.h")
    start = header.rfind("This software is available under 2 licenses")
    if start < 0:
        sys.exit("notices: stb_image.h has no license block")
    block = header[start:].replace("*/", "")
    return "\n".join(line.rstrip() for line in block.splitlines()).strip()


def miniz_license():
    if (VENDOR / "miniz/LICENSE").is_file():
        return read(VENDOR / "miniz/LICENSE")
    sys.exit("notices: Vendor/miniz/LICENSE is missing (run scripts/fetch-vendor.sh)")


def crates(platform):
    """The crates digup/digup-server ship, per Cargo's resolve graph for the platform,
    normal dependencies only (build tools and proc-macros don't end up in the binary).
    One graph for the workspace (cargo metadata has no -p), walked from each binary."""
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET[platform]],
        cwd=ROOT, check=True, capture_output=True, encoding="utf-8").stdout)
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    graph = set()
    for binary in ("digup-cli", "digup-server"):
        root = next(pid for pid, package in packages.items()
                    if package["name"] == binary and package["source"] is None)
        stack = [root]
        while stack:
            current = stack.pop()
            if current in graph:
                continue
            graph.add(current)
            for dependency in nodes[current]["deps"]:
                if not any(kind["kind"] is None for kind in dependency["dep_kinds"]):
                    continue
                package = packages[dependency["pkg"]]
                if any("proc-macro" in target["kind"] for target in package["targets"]):
                    continue
                stack.append(dependency["pkg"])
        graph.discard(root)
    return sorted((packages[i] for i in graph), key=lambda package: (package["name"], package["version"]))


def rust_section(platform):
    found = crates(platform)
    by_text = {}
    without = []
    for crate in found:
        folder = Path(crate["manifest_path"]).parent
        files = sorted(path for path in folder.iterdir() if path.is_file() and LICENSE_FILE.match(path.name))
        label = f"{crate['name']} {crate['version']} ({crate.get('license') or 'see its files'})"
        if not files:
            without.append((label, crate))
        for path in files:
            text = read(path)
            by_text.setdefault(" ".join(text.split()), (text, []))[1].append(label)
    parts = [RULE, f"Rust crates in the payload ({len(found)}), each under its own license", RULE, "",
             "digup and digup-server (the DigUp engine's Rust core) are built from these crates, with "
             "the license files they ship; a text several crates share is printed once, after their names.", ""]
    for text, labels in sorted(by_text.values(), key=lambda item: item[1][0]):
        parts += ["-" * 80, *labels, "-" * 80, "", text, ""]
    if without:
        parts += ["-" * 80, "Crates that ship no license file: their license, authors and source (the license texts "
                  "are above, as other crates ship them):", "-" * 80, ""]
        for label, crate in without:
            authors = ", ".join(crate.get("authors") or []) or "its authors"
            source = crate.get("repository") or f"https://crates.io/crates/{crate['name']}"
            parts += [f"{label}: copyright {authors}; source {source}"]
        parts += [""]
    return "\n".join(parts)


def main():
    # --count-crates <platform>: the crate count Cargo's own graph resolves for the
    # binaries this workspace ships. scripts/rust/audit.sh checks the notices file
    # against it, so the number in the file is Cargo's record, never a typed one.
    if len(sys.argv) == 3 and sys.argv[1] == "--count-crates":
        print(len(crates(sys.argv[2] if sys.argv[2] in TARGET else "linux")))
        return
    if len(sys.argv) != 3 or sys.argv[1] not in TARGET:
        sys.exit(__doc__)
    platform, out = sys.argv[1], Path(sys.argv[2])
    ffmpeg, dav1d = ffmpeg_versions()
    llama = VENDOR / "llama.cpp"
    pdfium = VENDOR / "pdfium"
    ffmpeg_licenses = VENDOR / "ffmpeg/licenses"

    parts = [
        "The DigUp Rust payload includes code from these projects, under their own licenses. Thank you.",
        "",
        "DigUp (https://github.com/ARahim3/DigUp) is MIT licensed; its license is the first one below. The model it "
        "uses, Google DeepMind's EmbeddingGemma 2 (Apache 2.0), isn't part of this download: DigUp downloads it from "
        "Hugging Face (`digup download-model`).",
        "",
        section("DigUp", need(ROOT / "LICENSE")),
        section("llama.cpp and ggml (https://github.com/ggml-org/llama.cpp), the embedding runtime, with one small "
                "change in scripts/llama.cpp-no-logits.patch",
                need(llama / "LICENSE"),
                *[f"{path.name}:\n\n{read(path)}" for path in sorted((llama / "licenses").glob("*")) if path.is_file()]),
        section("PDFium (https://pdfium.googlesource.com/pdfium/), built by Benoit Blanchon's pdfium-binaries "
                "(https://github.com/bblanchon/pdfium-binaries), reads PDFs; it includes the libraries whose licenses "
                "follow it" + (" — macOS reads PDFs with Apple's PDFKit, so this payload ships no PDFium; its license "
                               "is recorded for the builds the other systems ship" if platform == "macos" else ""),
                if_present(pdfium / "LICENSE", "PDFium (BSD 3-clause, with its third-party notices): "
                                               "https://github.com/bblanchon/pdfium-binaries."),
                *[f"{path.name}:\n\n{read(path)}" for path in sorted((pdfium / "licenses").glob("*")) if path.is_file()]),
        section(f"FFmpeg {ffmpeg} (https://ffmpeg.org), reads audio, video and some pictures, as the separate programs "
                f"ffmpeg and ffprobe" + (" (macOS reads them with Apple's AVFoundation; ffmpeg is not shipped there)" if platform == "macos" else ""),
                "FFmpeg is licensed under the GNU Lesser General Public License, version 2.1 or later. This build uses "
                "no GPL or nonfree parts. Its source is FFmpeg's release tarball, "
                f"https://ffmpeg.org/releases/ffmpeg-{ffmpeg}.tar.xz, unmodified, configured and built by "
                "scripts/build-ffmpeg.sh in DigUp's repository (https://github.com/ARahim3/DigUp), which also builds "
                "dav1d and links zlib. ffmpeg and ffprobe run as programs of their own: you can replace them beside "
                "the payload's binaries with your own build."
                if platform != "macos" else
                "macOS reads audio and video with Apple's AVFoundation, so this payload ships no FFmpeg at all; the "
                "license of the build scripts/build-ffmpeg.sh makes (LGPL 2.1 or later, no GPL parts) is recorded "
                "here for the builds DigUp ships on the other systems.",
                *([] if platform == "macos" else [
                    f"What this build says about itself:\n\n{need(ffmpeg_licenses / 'ffmpeg-version.txt')}",
                    need(ffmpeg_licenses / "ffmpeg-LICENSE.md"),
                    need(ffmpeg_licenses / "ffmpeg-COPYING.LGPLv2.1")]),
                ),
        section(f"dav1d {dav1d} (https://code.videolan.org/videolan/dav1d), decodes AV1 inside ffmpeg",
                *([] if platform == "macos" else [need(ffmpeg_licenses / "dav1d-COPYING")]),
                ),
        section("zlib (https://zlib.net), linked into ffmpeg",
                if_present(pdfium / "licenses/zlib.txt", "zlib is zlib License-licensed: https://zlib.net.")),
        section("SQLite (https://sqlite.org), the index",
                "SQLite is in the public domain: \"The author disclaims copyright to this source code. In place of a "
                "legal notice, here is a blessing: May you do good and not evil. May you find forgiveness for yourself "
                "and forgive others. May you share freely, never taking more than you give.\""),
        section("stb (https://github.com/nothings/stb), reads and writes pictures"
                + (" — macOS reads them with Apple's ImageIO, so this payload ships no stb; its license is recorded "
                   "for the builds the other systems ship" if platform == "macos" else ""),
                if_present(VENDOR / "stb/include/stb_image.h", "stb is MIT-licensed (public domain alternative): "
                                                               "https://github.com/nothings/stb.")
                if platform == "macos" else stb_license()),
        section("miniz (https://github.com/richgel999/miniz), reads docx and odt files"
                + (" — not shipped on macOS; its license is recorded for the builds the other systems ship"
                   if platform == "macos" else ""),
                if_present(VENDOR / "miniz/LICENSE", "miniz is MIT-licensed: https://github.com/richgel999/miniz.")
                if platform == "macos" else miniz_license()),
        section("ICU (https://icu.unicode.org), in PDFium (the same license, printed with PDFium's above)",
                if_present(pdfium / "licenses/icu.txt", "ICU is ICU License-licensed: https://icu.unicode.org.")),
        rust_section(platform),
    ]
    out.write_text("\n".join(parts) + "\n", encoding="utf-8")
    print(f"{out}: {out.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
