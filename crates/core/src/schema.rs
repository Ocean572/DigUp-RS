//! Schema SQL, reproduced verbatim from `Sources/DigUpKit/IndexStore.swift` (ARCHITECTURE_MAP.md
//! §2). Every string here is byte-identical to the engine's so a rusqlite-built index and a
//! Swift-built one are interchangeable at `user_version = 6`.

/// Keyword search's tokenizer (IndexStore.swift:29). Marks (M*) count as letters so Bengali vowel
/// signs and Arabic harakat stay inside their words.
pub const TOKENIZER: &str = "porter unicode61 remove_diacritics 2 categories 'L* N* Co M*'";

pub fn fts_columns() -> String {
    format!("name, body, original UNINDEXED, tokenize = \"{TOKENIZER}\"")
}

/// The `CASE … WHEN 'partial' THEN …` row term (IndexStore.swift:382), the exact contribution of
/// one row to `unfinishedCount`'s SUM. COALESCE **outside** MAX. One source for all six trigger
/// sites, the fill, and the check — so a fill can never disagree with the triggers it seeds.
pub fn partial_term(state: &str, info: &str) -> String {
    format!("CASE WHEN {state} = 'partial' THEN COALESCE(MAX(0, CAST(json_extract({info}, '$.read_end') AS INTEGER) - CAST(json_extract({info}, '$.read_to') AS INTEGER)), 0) ELSE 0 END")
}

/// The aggregate sibling of `partial_term` for full-scan SELECTs (fill and check), with the extra
/// `json_valid` guard so a malformed legacy `info` contributes 0 (IndexStore.swift:393).
pub fn partial_sum(prefix: &str) -> String {
    format!("SUM(CASE WHEN {prefix}state = 'partial' AND json_valid({prefix}info) THEN COALESCE(MAX(0, CAST(json_extract({prefix}info, '$.read_end') AS INTEGER) - CAST(json_extract({prefix}info, '$.read_to') AS INTEGER)), 0) ELSE 0 END)")
}

/// `create()` (IndexStore.swift:225): the base layout, stamped to version 5. The counters step
/// (5 → 6) follows in the same transaction.
pub fn create_sql() -> String {
    let fts = fts_columns();
    format!(
        "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime REAL NOT NULL,
    inode INTEGER NOT NULL,
    device INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',   -- pending | done | partial | skipped | failed
    detail TEXT,                              -- why skipped/failed, or how far a long file has been read
    info TEXT,                                -- JSON: pixels, pages, duration, date taken; read_to/read_end
    indexed_at REAL
);
CREATE INDEX files_inode ON files(device, inode);
CREATE INDEX files_state ON files(state, kind);
CREATE TABLE segments(
    id INTEGER PRIMARY KEY,
    file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,       -- SegmentKind
    modality TEXT NOT NULL,   -- Modality of the vector ('none' when there is no vector)
    loc REAL,                 -- PDF page number, or start second for audio/video
    loc_end REAL,
    excerpt TEXT,
    vec BLOB                  -- 768 × float16, L2-normalized
);
CREATE INDEX segments_file ON segments(file);
CREATE VIRTUAL TABLE fts USING fts5({fts});
PRAGMA user_version = 5;"
    )
}

/// `countersTablesSQL` (IndexStore.swift:419).
pub const COUNTERS_TABLES_SQL: &str = "CREATE TABLE IF NOT EXISTS counters(
    bucket TEXT NOT NULL,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    n INTEGER NOT NULL DEFAULT 0,
    leftsum INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(bucket, kind, state)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS segcounters(
    bucket TEXT NOT NULL,
    hasvec INTEGER NOT NULL,
    n INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(bucket, hasvec)
) WITHOUT ROWID;";

/// `counterTriggersSQL` (IndexStore.swift:442): the eight triggers, both amendments applied
/// (`files_au_path`'s bucket guard; COALESCE-outside-MAX at all six row-term sites), plus the
/// state-first covering index. `IF NOT EXISTS` everywhere so the migration re-runs after a partial
/// state.
pub fn counter_triggers_sql() -> String {
    let term_new = partial_term("new.state", "new.info");
    let term_old = partial_term("old.state", "old.info");
    let move_counters = format!(
        "UPDATE counters SET n = counters.n - 1,
    leftsum = counters.leftsum - {term_old}
WHERE bucket = old.bucket AND kind = old.kind AND state = old.state;
INSERT INTO counters(bucket, kind, state, n, leftsum)
VALUES (new.bucket, new.kind, new.state, 1, {term_new})
ON CONFLICT(bucket, kind, state) DO UPDATE SET n = counters.n + 1, leftsum = counters.leftsum + excluded.leftsum;"
    );
    format!(
        "CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
    INSERT INTO counters(bucket, kind, state, n, leftsum)
    VALUES (new.bucket, new.kind, new.state, 1, {term_new})
    ON CONFLICT(bucket, kind, state) DO UPDATE SET n = counters.n + 1, leftsum = counters.leftsum + excluded.leftsum;
END;
CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
    UPDATE counters SET n = counters.n - 1,
        leftsum = counters.leftsum - {term_old}
    WHERE bucket = old.bucket AND kind = old.kind AND state = old.state;
END;
CREATE TRIGGER IF NOT EXISTS files_au_state AFTER UPDATE OF kind, state, info ON files WHEN old.path IS new.path BEGIN
    {move_counters}
END;
CREATE TRIGGER IF NOT EXISTS files_au_path AFTER UPDATE OF path, bucket ON files
    WHEN old.path IS NOT new.path OR old.bucket IS NOT new.bucket BEGIN
    {move_counters}
    UPDATE segcounters SET n = segcounters.n - (SELECT COUNT(*) FROM segments WHERE file = new.id AND kind != 'name' AND vec IS NULL)
    WHERE bucket = old.bucket AND hasvec = 0;
    UPDATE segcounters SET n = segcounters.n - (SELECT COUNT(*) FROM segments WHERE file = new.id AND kind != 'name' AND vec IS NOT NULL)
    WHERE bucket = old.bucket AND hasvec = 1;
    INSERT INTO segcounters(bucket, hasvec, n)
    SELECT new.bucket, 0, COUNT(*) FROM segments WHERE file = new.id AND kind != 'name' AND vec IS NULL HAVING COUNT(*) > 0
    ON CONFLICT(bucket, hasvec) DO UPDATE SET n = segcounters.n + excluded.n;
    INSERT INTO segcounters(bucket, hasvec, n)
    SELECT new.bucket, 1, COUNT(*) FROM segments WHERE file = new.id AND kind != 'name' AND vec IS NOT NULL HAVING COUNT(*) > 0
    ON CONFLICT(bucket, hasvec) DO UPDATE SET n = segcounters.n + excluded.n;
END;
CREATE TRIGGER IF NOT EXISTS files_bd BEFORE DELETE ON files BEGIN
    UPDATE segcounters SET n = segcounters.n - (SELECT COUNT(*) FROM segments WHERE file = old.id AND kind != 'name' AND vec IS NULL)
    WHERE bucket = old.bucket AND hasvec = 0;
    UPDATE segcounters SET n = segcounters.n - (SELECT COUNT(*) FROM segments WHERE file = old.id AND kind != 'name' AND vec IS NOT NULL)
    WHERE bucket = old.bucket AND hasvec = 1;
END;
CREATE TRIGGER IF NOT EXISTS seg_ai AFTER INSERT ON segments
    WHEN new.kind != 'name' AND EXISTS (SELECT 1 FROM files WHERE id = new.file) BEGIN
    INSERT INTO segcounters(bucket, hasvec, n)
    VALUES ((SELECT bucket FROM files WHERE id = new.file), new.vec IS NOT NULL, 1)
    ON CONFLICT(bucket, hasvec) DO UPDATE SET n = segcounters.n + 1;
END;
CREATE TRIGGER IF NOT EXISTS seg_ad AFTER DELETE ON segments WHEN old.kind != 'name' BEGIN
    UPDATE segcounters SET n = segcounters.n - 1
    WHERE bucket = (SELECT bucket FROM files WHERE id = old.file) AND hasvec = (old.vec IS NOT NULL);
END;
CREATE TRIGGER IF NOT EXISTS seg_au_vec AFTER UPDATE OF vec ON segments WHEN old.kind != 'name' AND new.kind != 'name' BEGIN
    UPDATE segcounters SET n = segcounters.n - 1
    WHERE bucket = (SELECT bucket FROM files WHERE id = new.file) AND hasvec = (old.vec IS NOT NULL);
    INSERT INTO segcounters(bucket, hasvec, n)
    VALUES ((SELECT bucket FROM files WHERE id = new.file), (new.vec IS NOT NULL), 1)
    ON CONFLICT(bucket, hasvec) DO UPDATE SET n = segcounters.n + 1;
END;
CREATE INDEX IF NOT EXISTS counters_state ON counters(state, bucket, kind, n, leftsum);"
    )
}

/// `fillCounters` (IndexStore.swift:513): the aggregate is the truth, built with `partialSum` so it
/// agrees with the triggers by construction.
pub fn fill_counters_sql() -> String {
    format!(
        "DELETE FROM counters;
DELETE FROM segcounters;
INSERT INTO counters(bucket, kind, state, n, leftsum)
SELECT bucket, kind, state, COUNT(*), {full} FROM files GROUP BY bucket, kind, state;
INSERT INTO segcounters(bucket, hasvec, n)
SELECT f.bucket, s.vec IS NOT NULL, COUNT(*) FROM segments s JOIN files f ON f.id = s.file
WHERE s.kind != 'name' GROUP BY f.bucket, s.vec IS NOT NULL;",
        full = partial_sum("")
    )
}

/// `countersAreConsistent` (IndexStore.swift:533): a NULL-safe `IS NOT` FULL OUTER JOIN against
/// live aggregation. A group missing its counter row, a non-zero row with no group, and any
/// n/leftsum disagreement are mismatches. Zero-valued churn residue stands tolerated.
pub fn counters_consistency_sql() -> String {
    format!(
        "SELECT (SELECT COUNT(*) FROM (
                    SELECT bucket, kind, state, COUNT(*) AS n, {full} AS leftsum
                    FROM files GROUP BY bucket, kind, state) f
                FULL OUTER JOIN counters c
                    ON c.bucket = f.bucket AND c.kind = f.kind AND c.state = f.state
                    AND (c.n != 0 OR c.leftsum != 0)
                WHERE (f.n IS NOT c.n OR f.leftsum IS NOT c.leftsum)
                    AND NOT (f.bucket IS NULL AND c.n = 0 AND c.leftsum = 0))
              + (SELECT COUNT(*) FROM (
                    SELECT f.bucket, s.vec IS NOT NULL AS hasvec, COUNT(*) AS n
                    FROM segments s JOIN files f ON f.id = s.file
                    WHERE s.kind != 'name' GROUP BY f.bucket, s.vec IS NOT NULL) g
                FULL OUTER JOIN segcounters q ON q.bucket = g.bucket AND q.hasvec = g.hasvec AND q.n != 0
                WHERE g.n IS NOT q.n AND NOT (g.bucket IS NULL AND q.n = 0))",
        full = partial_sum("")
    )
}

/// `pruneZeroCounterRows` (IndexStore.swift:573): emptied groups keep zero rows (reads stay exact);
/// this clears them; the triggers' upserts re-create them on demand.
pub const PRUNE_ZERO_SQL: &str =
    "DELETE FROM counters WHERE n = 0 AND leftsum = 0; DELETE FROM segcounters WHERE n = 0;";
