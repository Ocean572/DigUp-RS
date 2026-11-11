//! Dev cross-check (not shipped): open an index with the Rust layer and print what the engine
//! reports, so an index built by the Swift `digup` can be compared field-for-field with one the
//! rusqlite layer built.
//!
//! Two modes:
//!
//! ```sh
//! cargo run -q -p digup-core --example counts -- counts <index-dir> [file.sqlite]   # counters
//! cargo run -q -p digup-core --example counts -- schema <index-dir> [file.sqlite]   # CREATE text
//! ```
//!
//! `counts` prints the counter table, the consistency check, and the full-aggregation oracle: a
//! Swift-built index must report the same numbers `digup status` prints. `schema` prints one sorted
//! line per `sqlite_master` object as `type name | <CREATE text, whitespace collapsed>`. Byte-for-byte
//! *file* equality across SQLite builds is not achievable (the Swift vendor builds SQLite 3.54, the
//! `bundled` feature builds 3.4x, and page layout differs for identical SQL), so the schema-text
//! comparison is the gate: same objects, same SQL.

use digup_core::{IndexStore, MAIN_FILE};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `schema <dir>` / `counts <dir>`; a bare `<dir>` is the counters mode, as before.
    let (mode, rest) = match args.first().map(String::as_str) {
        Some("schema") => ("schema", &args[1..]),
        Some("counts") => ("counts", &args[1..]),
        _ => ("counts", &args[..]),
    };
    let dir = rest
        .first()
        .expect("usage: counts|schema <index-dir> [file.sqlite]");
    let name = rest
        .get(1)
        .cloned()
        .unwrap_or_else(|| MAIN_FILE.to_string());
    let store = IndexStore::open(std::path::Path::new(dir), &name).expect("open index");

    if mode == "schema" {
        let mut rows = schema_text(&store);
        rows.sort();
        println!("{}", rows.join("\n"));
        return;
    }

    let counts = store.counts().expect("counts");
    println!("index: {}", store.file.display());
    println!("user_version: {}", store.db.user_version().unwrap());
    println!(
        "counters_consistent: {}",
        store.counters_are_consistent().unwrap()
    );
    println!(
        "vectors: {} in {} segments",
        counts.vectors, counts.segments
    );
    let mut kinds: Vec<_> = counts.by_kind_and_state.iter().collect();
    kinds.sort_by_key(|(kind, _)| (*kind).clone());
    for (kind, states) in kinds {
        let mut s: Vec<_> = states.iter().collect();
        s.sort();
        let line: Vec<String> = s
            .into_iter()
            .map(|(state, n)| format!("{n} {state}"))
            .collect();
        println!("  {kind:>11}   {}", line.join(", "));
    }
    let pending = store.pending_count().unwrap();
    let (unfinished, left) = store.unfinished_count().unwrap();
    println!("pending: {pending}  unfinished: {unfinished} ({left} left)");
    // The oracle: full aggregation must equal the counters, exactly as the engine's tests assert.
    let full = store.full_counts().unwrap();
    println!(
        "oracle_match: {}",
        full.by_kind_and_state == counts.by_kind_and_state
            && (full.segments, full.vectors) == (counts.segments, counts.vectors)
            && (full.unfinished, full.left) == (counts.unfinished, counts.left)
    );
}

/// One sorted line per `sqlite_master` object: `type name | <CREATE text, whitespace collapsed>`.
/// `sqlite_autoindex_*` rows are skipped: their SQL is NULL, and they follow from the CREATE TABLE
/// text, which is compared directly.
fn schema_text(store: &IndexStore) -> Vec<String> {
    let conn = store.db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT type, name, COALESCE(sql, '') FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_autoindex%' ORDER BY type, name",
        )
        .unwrap();
    stmt.query_map([], |row| {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let sql: String = row.get(2)?;
        Ok(format!("{kind} {name} | {}", flatten(&sql)))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// Collapse runs of whitespace to a single space and drop trailing semicolons, so a schema-text
/// comparison is about the SQL and not about how the source file was indented.
fn flatten(sql: &str) -> String {
    let words: Vec<&str> = sql.split_whitespace().collect();
    let mut out = words.join(" ");
    while out.ends_with(';') {
        out.truncate(out.len() - 1);
    }
    out.trim().to_string()
}
