# Test fixtures

- `DejaVuSans.ttf`, `NotoSansBengali-Regular.ttf` — the embedded font programs the PDF fixtures
  carry (DejaVu: its own permissive license, bitstream-vera-1.1; Noto Sans Bengali: SIL OFL 1.1).
  Unmodified system-font copies; the fixture builder reads their table directory (`/Length1`),
  `cmap` (glyph ids = the fixtures' CIDs) and `hmtx` (the `/W` widths) and embeds the bytes as
  `/FontFile2`.
- `pdf-answers.jsonl` — the engine's answers (`PDFExtractor.pages/renderPage`, `Estimator.pages`,
  the `PDFium.meta` tags, refusal texts, SHA-256 of every rendered page) measured through
  `PDF+Portable.swift` on these fixtures; the suite rebuilds each fixture byte-identically with
  the mirrored builder in `tests/pdf.rs` and compares.
- `sweep-strings.json` — the shared `PageTextProblem.check` string fixtures, with the engine's
  verdicts pinned in `tests/pdf.rs`.
- `media-*.{wav,m4a,mp4,ts}` — synthetic audio/video fixtures generated once by the pinned
  ffmpeg 4.4.2 from lavfi sources (flat color fields, sine tones, digital silence: no
  third-party content; ffmpeg itself is LGPL, its generated output here is our own synthetic
  data). They cover the media seam's shapes: stereo→16 kHz mono downmix across a silence edge,
  an up-sample of pure silence, an AAC tail under the hop minimum, three scene cuts for the
  8×8-hash shot dedup, video outlasting its soundtrack, MPEG-TS (the lead-seek branch),
  audio-only and video-only containers, a clip shorter than a window, a file cut before its
  `moov` (duration unknown), a faststart half-cut (decode dies mid-gop), junk, and empty.
  `gen-tests-media.py` rebuilds them byte-identically with the factory and pins their digests
  in `tests/media.rs`.
- `media-answers.jsonl` — the engine's answers (`MediaInfo.duration/streams`,
  `AudioExtractor.windows`, `VideoExtractor.keyframes`, `VideoExtractor.frame`, refusal texts,
  SHA-256 of every WAV copy and frame JPEG the seam writes) measured through
  `Media+Portable.swift` on these fixtures; the suite replays every line in `tests/media.rs`
  and compares field for field.
