//! `emit` (IndexService.swift:519): one JSON object per line on stdout, written
//! straight to the file descriptor (print() would buffer in a pipe) with Foundation's
//! `.sortedKeys`. The app may be gone by now (it quit while the helper lingered): a
//! closed pipe is an error to ignore here, not the exception that crashes the helper
//! on its way out — `std::io::stdout()`'s write errors are swallowed the same way.

use serde_json::Value;

/// Compact JSON on one line; a NaN/infinite number refuses the line (`to_line` →
/// `None`), and `emit` writes nothing, as `guard let data = try?` does. The line
/// ends with `0x0A` — the JSON-lines contract (`data + [0x0A]`).
pub fn emit(fields: &Value) {
    if let Some(line) = digup_core::jsonline::to_line(fields) {
        let _ = write_line(format!("{line}\n").as_bytes());
    }
}

/// Straight to fd 1, no buffering. A short write on a full pipe is dropped whole,
/// like `try? FileHandle.write` drops a failed write.
pub fn write_line(bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout();
    out.write_all(bytes)?;
    out.flush()
}
