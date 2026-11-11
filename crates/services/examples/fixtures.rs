//! Dev cross-check: the speed check's fixture bytes (`ModelSpeed.bitmap` / `ModelSpeed.wave`),
//! hashed, so the Swift-made bytes (measured beside) can be compared field for field.
use digup_services::speed::{bitmap, wave};

fn main() {
    for (name, bytes) in [("bitmap", bitmap(960, 720)), ("wave", wave(30))] {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a, for a quick fingerprint
        for byte in &bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        println!("{name}: {} bytes, fnv {:016x}", bytes.len(), hash);
        // The head and tail bytes, for the field-for-field comparison.
        println!("  head: {:02x?}", &bytes[..16.min(bytes.len())]);
        println!("  tail: {:02x?}", &bytes[bytes.len() - 12..]);
    }
}
