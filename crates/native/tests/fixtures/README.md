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
