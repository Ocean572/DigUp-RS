//! The `digup` binary — Rust port. Scaffold in Phase 1.
//!
//! Phase 5+ implements the one-shot CLI (`estimate`/`index`/`search`/`status`/`speed`/`eval`) and
//! the JSON-lines services. This Phase-1 stub proves the workspace wires `core` and creates a v6
//! index, then exits.

use digup_core::{IndexStore, MAIN_FILE, SCHEMA_VERSION};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("digup-cli-selftest")
            .to_string_lossy()
            .into_owned()
    });
    match IndexStore::open(std::path::Path::new(&dir), MAIN_FILE) {
        Ok(store) => println!(
            "index: {}\nuser_version: {}",
            store.file.display(),
            SCHEMA_VERSION
        ),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
