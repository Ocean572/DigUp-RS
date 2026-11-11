//! `UUID().uuidString` — the name the engine gives the copies it writes (the picture `prepare`
//! scales, the index's previews, the speed test's temp folder). Foundation spells a v4 UUID in
//! CANONICAL form with UPPERCASE hex: `7A19…-…-4…-8…-…`, and the path it names lands in the index,
//! so the port spells it the same way.
//!
//! The randomness comes from the operating system (`/dev/urandom`, the same entropy `UUID()` reads);
//! the version nibble is 4 and the variant nibble is one of 8/9/A/B, as RFC 4122 says.

use std::fs::File;
use std::io::Read;

/// A new v4 UUID as Foundation spells it: 36 characters, uppercase, dashes at 8/13/18/23.
pub fn uuid_string() -> String {
    let mut bytes = [0u8; 16];
    match File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)) {
        Ok(()) => {}
        Err(_) => {
            // No entropy device: keep the shape, take the bytes from the clock and this process.
            // (A name from here is still unique enough for a preview folder on one machine.)
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos() as u64)
                .unwrap_or(0);
            let pid = std::process::id() as u64;
            for (index, slot) in bytes.iter_mut().enumerate() {
                let source = if index < 8 { nanos } else { pid.rotate_left(8) };
                *slot = ((source >> (8 * (index % 8))) & 0xFF) as u8;
            }
        }
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC 4122 variant
    let hex: String = bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    let mut out = String::with_capacity(36);
    for (index, character) in hex.chars().enumerate() {
        if [8, 12, 16, 20].contains(&index) {
            out.push('-');
        }
        out.push(character);
    }
    out
}
