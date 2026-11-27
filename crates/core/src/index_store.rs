//! The on-disk index: one SQLite file with files, their segments (fp16 vectors), and an FTS5
//! table — reproducing `Sources/DigUpKit/IndexStore.swift` (ARCHITECTURE_MAP.md §2).
//!
//! Vectors live in the same database as everything else (a crash can't leave them out of step with
//! the rows that describe them). At this scale brute-force search over an in-memory copy is a few
//! milliseconds; no vector database is needed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::db::{Database, Result, Sql};
use crate::model::*;
use crate::schema::*;
use crate::search_text::{has_words, normalized, PageTextProblem};
use crate::vectors;

/// The layout version this build writes (IndexStore.swift:25 was 6; v7 is the portable layout,
/// #195). A v6 store opens and READS verbatim — `mode()` says `V6` and every reader answers the
/// stored absolute paths. The v7 conversion is an explicit WRITE verb (`migrate_portable_store`,
/// S2): reads never migrate, and a v6 store's write verbs refuse with the migration sentence.
pub const SCHEMA_VERSION: i64 = 7;
const MAX_PAGES: i64 = 5_000; // IndexOptions.maxPages
const MAX_CHUNKS: i64 = 5_000; // IndexOptions.maxChunks

/// The main index's file in the index folder (IndexStore.swift:178).
pub const MAIN_FILE: &str = "index.sqlite";
/// The code index, a file of its own beside it (IndexStore.swift:181).
pub const CODE_FILE: &str = "code.sqlite";

/// Which layout this store's file speaks (v7 = anchors + relative paths, #195; v6 = the
/// Swift-era absolute-path layout, read-only for this build).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreMode {
    V6,
    V7,
}

/// One crawl root of a v7 store: `abs` in the engine's ONE form, `real` when symlinks resolve
/// it differently (the dual-spelling law).
#[derive(Debug, Clone)]
pub struct Anchor {
    pub id: i64,
    pub abs: String,
    pub real: Option<String>,
}

/// The assembly SQL for an absolute answer path from (anchor, rel) at the row edge: the POSIX
/// root anchor `"/"` carries its own slash, every other root gets one between. The clients —
/// server HitCache, media scope, MCP, shell — keep seeing one absolute path, byte-identical to
/// what v6 stored.
pub fn abs_path_sql() -> &'static str {
    "CASE WHEN substr(a.abs, -1, 1) = '/' THEN a.abs || f.path ELSE a.abs || '/' || f.path END"
}

/// v7 scope range lists: merged per-anchor relative ranges (anchor, lo, hi) and the merged
/// counter-bucket ranges over the anchor-prefixed key space.
type ScopeFileRanges = Vec<(i64, String, String)>;
type ScopeBucketRanges = Vec<(String, String)>;

#[derive(Debug, Clone, Default)]
pub struct Counts {
    pub by_kind_and_state: HashMap<String, HashMap<String, i64>>,
    pub vectors: i64,
    pub segments: i64,
    pub unfinished: i64,
    pub left: i64,
}

pub struct IndexStore {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub db: Database,
    scope: RefCell<Option<Vec<String>>>,
    scope_sql: RefCell<String>,
    mode: std::cell::Cell<StoreMode>,
    anchors: RefCell<Vec<Anchor>>,
    anchors_loaded: std::cell::Cell<bool>,
}

impl IndexStore {
    pub fn main(directory: &Path) -> Result<Self> {
        Self::open(directory, MAIN_FILE)
    }

    pub fn open(directory: &Path, name: &str) -> Result<Self> {
        std::fs::create_dir_all(directory).map_err(|e| crate::db::Error::Failure(e.to_string()))?;
        let file = directory.join(name);
        let db = Database::open(&file)?;
        let store = Self {
            directory: directory.to_path_buf(),
            file,
            db,
            scope: RefCell::new(None),
            scope_sql: RefCell::new(String::new()),
            mode: std::cell::Cell::new(StoreMode::V6),
            anchors: RefCell::new(Vec::new()),
            anchors_loaded: std::cell::Cell::new(false),
        };
        store.migrate()?;
        Ok(store)
    }

    // MARK: Migration (IndexStore.swift:202)

    /// Which layout the opened file speaks (`V6` until the migration verb converts it).
    pub fn mode(&self) -> StoreMode {
        self.mode.get()
    }

    fn is_v7(&self) -> bool {
        self.mode.get() == StoreMode::V7
    }

    /// The write guard (#195): a v6 store reads verbatim; its write verbs refuse with the
    /// migration sentence. The index verb answers it until S2 wires the auto-migration.
    fn write_guard(&self) -> Result<()> {
        if self.mode.get() == StoreMode::V6 {
            return Err(crate::db::Error::Failure(
                "this index is schema 6 (absolute paths); the portable migration runs on the first write verb".to_string(),
            ));
        }
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        let version = self.db.user_version()?;
        if version > SCHEMA_VERSION {
            return Err(crate::db::Error::Failure(format!(
                "this index was made by a newer DigUp (schema {version})"
            )));
        }
        if version == SCHEMA_VERSION {
            self.mode.set(StoreMode::V7);
            return Ok(());
        }
        if version == 6 {
            // v6 read compatibility (the compat law): opening a v6 store is a verbatim v6 read —
            // no re-fill, no re-check, no stamp. The v6 → v7 conversion is the migration verb.
            self.mode.set(StoreMode::V6);
            return Ok(());
        }
        // The app and its helper can open the index at the same moment: one migrates while the
        // other waits for it (60 s, not the usual 5 s), then sees it done.
        self.db
            .set_busy_timeout_ms(crate::db::MIGRATION_BUSY_TIMEOUT_MS)?;
        let result = self.db.transaction(|| {
            // The version is read once and drives every step, exactly as the Swift chain does —
            // a fresh database (version 0) runs `create` then the counters step in one transaction.
            let version = self.db.user_version()?;
            if version == 0 {
                self.db.execute(&create_sql())?;
            }
            if version == 1 {
                self.migrate_keyword_search()?;
            }
            if (1..=2).contains(&version) {
                self.migrate_screenshot_text()?;
            }
            if (1..=3).contains(&version) {
                self.migrate_vector_names()?;
            }
            if (1..=4).contains(&version) {
                self.migrate_long_files()?;
            }
            self.migrate_counters()?; // 5 → 6, fresh databases included
            if version == 0 {
                // A fresh store is born portable (#195): the v7 conversion runs on the empty
                // tables — no rows to relativize, the counters check already passed in
                // `migrate_counters`. A store with rows converts through the migration verb.
                self.migrate_portable()?;
            }
            Ok(())
        });
        self.db.set_busy_timeout_ms(crate::db::BUSY_TIMEOUT_MS)?;
        if self.db.user_version()? == SCHEMA_VERSION {
            self.mode.set(StoreMode::V7);
        }
        result
    }

    /// Fresh-store v6 → v7 (schema shape only, `schema::portable_sql`): the anchors table, the
    /// `files` rebuild with `anchor` + `UNIQUE(anchor, path)` (the table swap preserves ids —
    /// the copy carries `id` explicitly, the v6 UNIQUE law is replaced), and all eight triggers
    /// re-created on the new table (the three segment triggers reference `files` and go
    /// schema-invalid mid-swap — they come down with the files triggers). The counters tables
    /// and their fill stand untouched.
    fn migrate_portable(&self) -> Result<()> {
        self.db.execute(&portable_sql())
    }

    /// Schema 1 → 2: whole Bengali/Arabic words over normalized text. Words are rebuilt from what's
    /// stored, without reading any file; garbled PDF pages (`PageTextProblem`) are re-queued.
    fn migrate_keyword_search(&self) -> Result<()> {
        self.db.execute(&format!(
            "CREATE VIRTUAL TABLE fts_new USING fts5({})",
            fts_columns()
        ))?;
        // Row by row: an index can hold tens of megabytes of text, and the app may be migrating.
        self.db
            .query("SELECT rowid, name, body FROM fts", [], |row| {
                let rowid = row.int(0);
                let name = row.text(1).unwrap_or_default();
                let body = row.text(2).unwrap_or_default();
                let (body_norm, original) = keyword_text(&body);
                self.db.run(
                    "INSERT INTO fts_new(rowid, name, body, original) VALUES(?, ?, ?, ?)",
                    vec![
                        Sql::Int(rowid),
                        Sql::Text(normalized(&name)),
                        Sql::Text(body_norm),
                        Sql::opt_text(original.as_deref()),
                    ],
                )
            })?;
        let mut garbled = Vec::new();
        self.db.query(
            "SELECT s.file, fts.body FROM fts JOIN segments s ON s.id = fts.rowid JOIN files f ON f.id = s.file WHERE f.kind = 'pdf' AND s.kind IN ('page', 'page_image')",
            [],
            |row| {
                if PageTextProblem::check(&row.text(1).unwrap_or_default()).is_some() {
                    garbled.push(row.int(0));
                }
                Ok(())
            },
        )?;
        self.db.execute(
            "DROP TABLE fts; ALTER TABLE fts_new RENAME TO fts; PRAGMA user_version = 2;",
        )?;
        for file in garbled {
            self.requeue_raw(file)?;
        }
        Ok(())
    }

    /// Schema 2 → 3: a screenshot's OCR text is keyword-only; its vectors go, nothing is read again.
    fn migrate_screenshot_text(&self) -> Result<()> {
        self.db.execute("UPDATE segments SET vec = NULL, modality = 'none' WHERE kind = 'ocr'; PRAGMA user_version = 3;")
    }

    /// Schema 3 → 4: name vectors by a vector version, not the llama.cpp build that made them.
    fn migrate_vector_names(&self) -> Result<()> {
        self.db.execute("UPDATE meta SET value = replace(value, ' llama.cpp-b11461-metal ', ' llama.cpp-vectors1 ') WHERE key IN ('fingerprint', 'vector_source'); PRAGMA user_version = 4;")
    }

    /// Schema 4 → 5: long documents become `partial` for the later pass; digit-only text loses its
    /// vectors. Nothing is read or embedded here.
    fn migrate_long_files(&self) -> Result<()> {
        let mut digits = Vec::new();
        self.db.query(
            "SELECT s.id, coalesce(fts.original, fts.body) FROM segments s JOIN fts ON fts.rowid = s.id WHERE s.kind IN ('page', 'chunk') AND s.vec IS NOT NULL",
            [],
            |row| {
                if !has_words(&row.text(1).unwrap_or_default()) {
                    digits.push(row.int(0));
                }
                Ok(())
            },
        )?;
        for id in digits {
            self.db.run(
                "UPDATE segments SET vec = NULL, modality = 'none' WHERE id = ?",
                vec![Sql::Int(id)],
            )?;
        }
        self.db.execute(&format!(
            "UPDATE files SET detail = 'read to page 30', info = json_set(info, '$.read_to', '30', '$.read_end', CAST(MIN(CAST(json_extract(info, '$.pages') AS INTEGER), {MAX_PAGES}) AS TEXT)) WHERE kind = 'pdf' AND state = 'partial' AND info IS NOT NULL;
UPDATE files SET state = 'partial', detail = 'read to chunk 40', info = json_set(info, '$.read_to', '40', '$.read_end', CAST(MIN(MAX(41, (CAST(json_extract(info, '$.chars') AS INTEGER) + 1599) / 1600), {MAX_CHUNKS}) AS TEXT)) WHERE kind = 'doc' AND state = 'done' AND json_extract(info, '$.chunks') = '40';
PRAGMA user_version = 5;"
        ))
    }

    /// Schema 5 → 6 (issue #193): the counters. Column + backfill come first (no triggers exist, so
    /// each backfill UPDATE costs one write and fires no maintenance); the tables, triggers, and
    /// index come next; the fill computes every group once, and the check must pass before the stamp
    /// — v6 always means "checked". A crash at any point leaves v5 intact.
    fn migrate_counters(&self) -> Result<()> {
        let mut has_bucket = false;
        self.db.query(
            "SELECT 1 FROM pragma_table_info('files') WHERE name = 'bucket'",
            [],
            |_| {
                has_bucket = true;
                Ok(())
            },
        )?;
        if !has_bucket {
            self.db
                .execute("ALTER TABLE files ADD COLUMN bucket TEXT NOT NULL DEFAULT ''")?;
        }
        // Backfill by rowid in batches of 5,000; the statement is prepared once (keeping:). Each
        // batch is read whole before it is written — updating a table while its own SELECT cursor is
        // open is a live-read hazard.
        let mut last: i64 = -1;
        let mut count = 5000usize;
        while count == 5000 {
            let mut batch: Vec<(i64, String)> = Vec::new();
            self.db.query(
                "SELECT id, path FROM files WHERE id > ? ORDER BY id LIMIT 5000",
                vec![Sql::Int(last)],
                |row| {
                    batch.push((row.int(0), bucket_of(&row.text(1).unwrap_or_default())));
                    last = row.int(0);
                    Ok(())
                },
            )?;
            self.db.run_keeping(
                "UPDATE files SET bucket = ? WHERE id = ? AND bucket != ?",
                |bind| {
                    for (id, bucket) in &batch {
                        bind(vec![
                            Sql::Text(bucket.clone()),
                            Sql::Int(*id),
                            Sql::Text(bucket.clone()),
                        ])?;
                    }
                    Ok(())
                },
            )?;
            count = batch.len();
        }
        self.db.execute(COUNTERS_TABLES_SQL)?;
        self.db.execute(&counter_triggers_sql())?;
        self.fill_counters()?;
        if !self.counters_are_consistent()? {
            // The fill disagrees with files: throw, roll the transaction back, stay v5.
            return Err(crate::db::Error::Failure(
                "counter fill disagrees with the files table".into(),
            ));
        }
        self.db.set_user_version(6)
    }

    pub fn fill_counters(&self) -> Result<()> {
        self.db.execute(&fill_counters_sql())
    }

    /// `countersAreConsistent` (IndexStore.swift:533): do counters and segcounters equal full
    /// aggregation, row for row, over the whole library?
    pub fn counters_are_consistent(&self) -> Result<bool> {
        let mut mismatches: i64 = 0;
        self.db.query(&counters_consistency_sql(), [], |row| {
            mismatches = row.int(0);
            Ok(())
        })?;
        Ok(mismatches == 0)
    }

    /// `rebuildCounters` (IndexStore.swift:558): recompute from full aggregation, in one
    /// transaction, and verify it — the documented self-heal.
    pub fn rebuild_counters(&self) -> Result<()> {
        self.db.transaction(|| {
            self.fill_counters()?;
            if !self.counters_are_consistent()? {
                return Err(crate::db::Error::Failure(
                    "counter rebuild disagrees with the files table".into(),
                ));
            }
            Ok(())
        })
    }

    pub fn prune_zero_counter_rows(&self) -> Result<()> {
        self.db.execute(PRUNE_ZERO_SQL)
    }

    // MARK: Anchors (v7, #195)

    /// The store's crawl roots, in id order. The list is small (roots, not files) and cached
    /// per connection; `ensure_anchor` invalidates the cache.
    pub fn anchors(&self) -> Result<Vec<Anchor>> {
        if !self.is_v7() {
            return Ok(Vec::new());
        }
        if self.anchors_loaded.get() {
            return Ok(self.anchors.borrow().clone());
        }
        let mut out = Vec::new();
        self.db
            .query("SELECT id, abs, real FROM anchors ORDER BY id", [], |row| {
                out.push(Anchor {
                    id: row.int(0),
                    abs: row.text(1).unwrap_or_default(),
                    real: row.text(2),
                });
                Ok(())
            })?;
        *self.anchors.borrow_mut() = out.clone();
        self.anchors_loaded.set(true);
        Ok(out)
    }

    /// Registers one crawl root (the engine's ONE form: slashes + standardize) and answers its
    /// id. `real` carries the symlink-resolved spelling when it differs — the dual-spelling law
    /// `path_ranges` already rides, carried into the rel space.
    pub fn ensure_anchor(&self, abs: &str) -> Result<i64> {
        self.write_guard()?;
        let abs = crate::paths::standardize(&crate::paths::to_slashes(abs));
        if let Some(found) = self
            .anchors()?
            .into_iter()
            .find(|a| a.abs == abs)
            .map(|a| a.id)
        {
            return Ok(found);
        }
        let real = crate::paths::real_path(&abs);
        let real = (real.as_bytes() != abs.as_bytes()).then_some(real);
        self.db.run(
            "INSERT INTO anchors(abs, real) VALUES(?, ?) ON CONFLICT(abs) DO NOTHING",
            vec![Sql::Text(abs.clone()), Sql::opt_text(real.as_deref())],
        )?;
        self.anchors_loaded.set(false);
        let mut id = 0i64;
        self.db.query(
            "SELECT id FROM anchors WHERE abs = ?",
            vec![Sql::Text(abs.clone())],
            |row| {
                id = row.int(0);
                Ok(())
            },
        )?;
        if let Ok(mut cache) = self.anchors.try_borrow_mut() {
            cache.push(Anchor { id, abs, real });
        }
        Ok(id)
    }

    /// The (anchor, rel) split of an absolute path: the anchor with the LONGEST bound prefix
    /// over both spellings (`abs` and `real` — a walk reports `/private/var/...` where the root
    /// says `/var/...`; ties break to the lower id, the cache is id-ordered). Err when no root
    /// covers the path — the honest out-of-anchor refusal.
    fn split_anchor(&self, path: &str) -> Result<(i64, String)> {
        let mut best: Option<(usize, i64)> = None;
        for anchor in self.anchors()? {
            for spelling in std::iter::once(&anchor.abs).chain(anchor.real.iter()) {
                let bound = crate::paths::bound(spelling);
                if path.starts_with(&bound) {
                    match best {
                        Some((len, _)) if len >= bound.len() => {}
                        _ => best = Some((bound.len(), anchor.id)),
                    }
                }
            }
        }
        match best {
            Some((cut, id)) => {
                let rel = path[cut..].trim_start_matches('/').to_string();
                if rel.is_empty() {
                    return Err(crate::db::Error::Failure(format!(
                        "{path} is an anchor folder, not a file under it"
                    )));
                }
                Ok((id, rel))
            }
            None => Err(crate::db::Error::Failure(format!(
                "{path} is under none of this index's roots"
            ))),
        }
    }

    /// The writer's split: a path under no registered root anchors at its own parent folder (a
    /// store whose roots were never registered — the unit pins, a store opened bare; the crawl
    /// registers its roots through `set_scope`, so its rows anchor at the roots themselves).
    fn split_for_write(&self, path: &str) -> Result<(i64, String)> {
        match self.split_anchor(path) {
            Ok(found) => Ok(found),
            Err(_) => {
                let parent = bucket_of(path);
                let anchor = if parent.is_empty() {
                    if path.starts_with('/') {
                        "/".to_string()
                    } else {
                        return Err(crate::db::Error::Failure(format!(
                            "{path} has no folder to anchor at"
                        )));
                    }
                } else {
                    parent
                };
                let id = self.ensure_anchor(&anchor)?;
                let name = crate::paths::last_component(path).to_string();
                Ok((id, name))
            }
        }
    }

    /// The counters' bucket key in v7: the anchor id prefixes the relative folder, so bucket
    /// strings stay globally unique and the eight triggers, the counters fill, the consistency
    /// check, and the scoped bucket ranges ride v6 SQL UNCHANGED. Root-level files get the
    /// prefix alone (`"<id>/"`).
    fn prefixed_bucket(&self, anchor: i64, rel: &str) -> String {
        format!("{}/{}", anchor, bucket_of(rel))
    }

    /// The FROM clause carrying (files, anchors) for v7 reads; v6 reads the plain table.
    pub fn files_from(&self) -> &'static str {
        if self.is_v7() {
            "files f JOIN anchors a ON a.id = f.anchor"
        } else {
            "files"
        }
    }

    /// The anchors JOIN for readers whose FROM spells the files table itself (the searcher's
    /// `files f` / `JOIN files f` shapes).
    pub fn anchors_join(&self) -> &'static str {
        if self.is_v7() {
            " JOIN anchors a ON a.id = f.anchor"
        } else {
            ""
        }
    }

    /// The SELECT-list expression for an absolute answer path: v6's stored path column, v7's
    /// assembly at the row edge (see `abs_path_sql`). Readers spell the files table as `f`.
    pub fn abs_path_sql(&self) -> &'static str {
        if self.is_v7() {
            crate::index_store::abs_path_sql()
        } else {
            "f.path"
        }
    }

    /// The SELECT list for `FileRecord`, per mode: v6's stored absolute path, v7's assembly at
    /// the row edge (every client answer keeps the byte-identical absolute shape).
    fn select_cols(&self) -> &'static str {
        if self.is_v7() {
            "f.id, CASE WHEN substr(a.abs, -1, 1) = '/' THEN a.abs || f.path ELSE a.abs || '/' || f.path END, f.kind, f.size, f.mtime, f.inode, f.device, f.state, f.detail"
        } else {
            Self::FILE_COLUMNS
        }
    }

    /// The assembled-path SELECT for id+path scans (`files_under`'s v7 form, `prune`): v6 reads
    /// the stored path, v7 assembles.
    fn select_abs(&self) -> &'static str {
        if self.is_v7() {
            "f.id, CASE WHEN substr(a.abs, -1, 1) = '/' THEN a.abs || f.path ELSE a.abs || '/' || f.path END"
        } else {
            "id, path"
        }
    }

    // MARK: Scope (IndexStore.swift:53)

    /// Sets the folder scope. When set, `pending`/`pendingCount`/`counts` see only files under these
    /// folders. The merged ranges ride in a temp table so scoped reads cost one PK seek per folder.
    /// v6 (read mode): the Swift-era ranges over the absolute path/bucket keys, verbatim.
    /// v7 (#195): the scope folders ARE the crawl roots — each is registered as an anchor (the
    /// argv-roots-to-anchors law), file ranges become per-anchor relative ranges, and counter
    /// bucket ranges ride the anchor-prefixed key space (globally unique strings, so the scoped
    /// `counts` SQL is shared with v6 unchanged).
    pub fn set_scope(&self, folders: Option<Vec<String>>) -> Result<()> {
        let folders = match folders {
            None => {
                *self.scope.borrow_mut() = None;
                *self.scope_sql.borrow_mut() = String::new();
                return Ok(());
            }
            Some(f) => f,
        };
        let folders: Vec<String> = folders
            .into_iter()
            .map(|folder| crate::paths::standardize(&crate::paths::to_slashes(&folder)))
            .collect();
        if self.is_v7() {
            for folder in &folders {
                self.ensure_anchor(folder)?;
            }
            let (file_ranges, bucket_ranges) = self.scope_ranges_v7(&folders)?;
            self.db.transaction(|| {
                create_scope_anchor(&self.db, "scope", &file_ranges)?;
                create_scope(&self.db, "scopeb", &bucket_ranges)?;
                Ok(())
            })?;
            *self.scope.borrow_mut() = Some(folders);
            *self.scope_sql.borrow_mut() = if file_ranges.is_empty() {
                " AND 0".to_string()
            } else {
                " AND (SELECT s.hi FROM scope s WHERE s.anchor = f.anchor AND s.lo <= f.path ORDER BY s.lo DESC LIMIT 1) > f.path".to_string()
            };
            return Ok(());
        }
        let ranges = merged_ranges(folders.iter().flat_map(|f| path_ranges(f)).collect());
        let bucket = merged_ranges(folders.iter().flat_map(|f| bucket_ranges(f)).collect());
        self.db.transaction(|| {
            create_scope(&self.db, "scope", &ranges)?;
            create_scope(&self.db, "scopeb", &bucket)?;
            Ok(())
        })?;
        *self.scope.borrow_mut() = Some(folders);
        *self.scope_sql.borrow_mut() = if ranges.is_empty() {
            " AND 0".to_string()
        } else {
            " AND (SELECT s.hi FROM scope s WHERE s.lo <= files.path ORDER BY s.lo DESC LIMIT 1) > files.path".to_string()
        };
        Ok(())
    }

    /// v7 scope ranges: (anchor, rel-lo, rel-hi) merged per anchor + the counter-bucket ranges
    /// over the anchor-prefixed key space. Two cases per (folder, anchor) spelling:
    /// - the anchor sits inside the folder (or is the folder): every row of the anchor is in
    ///   scope — the whole relative range `("", MAX)`, whole prefix range `("<id>/", <id>/+MAX)`;
    /// - the folder sits under the anchor: strip the anchor bound (of that spelling — the
    ///   `/var`↔`/private/var` law) from the folder bound → the relative prefix; its files range
    ///   is `(prefix, prefix+MAX)`, its buckets the own-key range (`prefix+MAX`, the folder's own
    ///   bucket, one unit past) plus the subfolder range — the v6 `bucket_ranges` shapes, in the
    ///   prefixed space.
    fn scope_ranges_v7(&self, folders: &[String]) -> Result<(ScopeFileRanges, ScopeBucketRanges)> {
        const MAX: char = char::MAX;
        let mut per_anchor: std::collections::BTreeMap<i64, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        let mut buckets: Vec<(String, String)> = Vec::new();
        for folder in folders {
            let folder_bound = crate::paths::bound(folder);
            for anchor in self.anchors()? {
                let prefix = format!("{}/", anchor.id);
                for spelling in std::iter::once(&anchor.abs).chain(anchor.real.iter()) {
                    let whole =
                        |ranges: &mut Vec<(String, String)>,
                         buckets: &mut Vec<(String, String)>| {
                            ranges.push((String::new(), MAX.to_string()));
                            buckets.push((prefix.clone(), format!("{prefix}{MAX}")));
                        };
                    if folder == spelling || crate::paths::is_inside(spelling, folder) {
                        whole(per_anchor.entry(anchor.id).or_default(), &mut buckets);
                        continue;
                    }
                    let spelling_bound = crate::paths::bound(spelling);
                    if folder.starts_with(&spelling_bound) {
                        let rel = folder_bound[spelling_bound.len()..].to_string();
                        per_anchor
                            .entry(anchor.id)
                            .or_default()
                            .push((rel.clone(), format!("{rel}{MAX}")));
                        let own = format!("{prefix}{}", bucket_of(&format!("{rel}x")));
                        buckets.push((own.clone(), format!("{own}\u{1}")));
                        let sub = format!("{prefix}{rel}");
                        buckets.push((sub.clone(), format!("{sub}{MAX}")));
                    }
                }
            }
        }
        let file_ranges: Vec<(i64, String, String)> = per_anchor
            .into_iter()
            .flat_map(|(id, ranges)| {
                merged_ranges(ranges)
                    .into_iter()
                    .map(move |r| (id, r.0, r.1))
            })
            .collect();
        Ok((file_ranges, merged_ranges(buckets)))
    }

    fn scope_active(&self) -> bool {
        self.scope.borrow().is_some()
    }

    /// `store.scope` (#178): the folders a scoped `counts()` answers for, nil when
    /// unscoped — the `status --json`/scope line's rows.
    pub fn scope(&self) -> Option<Vec<String>> {
        self.scope.borrow().clone()
    }

    fn scope_sql(&self) -> String {
        self.scope_sql.borrow().clone()
    }

    // MARK: Meta

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let mut value = None;
        self.db.query(
            "SELECT value FROM meta WHERE key = ?",
            vec![Sql::Text(key.to_string())],
            |row| {
                value = row.text(0);
                Ok(())
            },
        )?;
        Ok(value)
    }

    pub fn set_meta(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => self.db.run(
                "INSERT INTO meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                vec![Sql::Text(key.to_string()), Sql::Text(v.to_string())],
            ),
            None => self.db.run("DELETE FROM meta WHERE key = ?", vec![Sql::Text(key.to_string())]),
        }
    }

    // MARK: Files

    const FILE_COLUMNS: &'static str = "id, path, kind, size, mtime, inode, device, state, detail";

    fn record(row: &crate::db::RowView) -> FileRecord {
        FileRecord {
            id: row.int(0),
            path: row.text(1).unwrap_or_default(),
            kind: FileKind::parse(&row.text(2).unwrap_or_default()).unwrap_or(FileKind::Doc),
            size: row.int(3),
            mtime: row.double(4),
            inode: row.int(5) as u64,
            device: row.int(6) as u64,
            state: row.text(7).unwrap_or_else(|| "pending".to_string()),
            detail: row.text(8),
        }
    }

    /// Adds a file (pending) and returns its id — the row and its name entry together.
    /// v7: the path splits to (anchor, rel) and the bucket rides the anchor-prefixed key;
    /// the name row keeps tokenizing the ASSEMBLED absolute path (the s343 law: excerpts quote
    /// the folder path as FTS tokens — tokens are spelling-of-the-root, not relative).
    pub fn insert(&self, file: &Candidate) -> Result<i64> {
        self.write_guard()?;
        if !self.is_v7() {
            return self.insert_v6(file);
        }
        let path = crate::paths::standardize(&crate::paths::to_slashes(&file.path));
        self.db.transaction(|| {
            let (anchor, rel) = self.split_for_write(&path)?;
            let bucket = self.prefixed_bucket(anchor, &rel);
            self.db.run(
                "INSERT INTO files(anchor, path, kind, size, mtime, inode, device, bucket) VALUES(?, ?, ?, ?, ?, ?, ?, ?)",
                vec![
                    Sql::Int(anchor),
                    Sql::Text(rel),
                    Sql::Text(file.kind.as_str().to_string()),
                    Sql::Int(file.size),
                    Sql::Double(file.mtime),
                    Sql::Int(file.inode as i64),
                    Sql::Int(file.device as i64),
                    Sql::Text(bucket),
                ],
            )?;
            let id = self.db.last_insert_rowid();
            self.add_name_row(id, &path)?;
            Ok(id)
        })
    }

    /// The v6 write path kept for one release: a v6 store refuses writes (`write_guard`), so
    /// this runs only after the migration verb converts a store — it has no v6 call site.
    fn insert_v6(&self, file: &Candidate) -> Result<i64> {
        self.db.transaction(|| {
            self.db.run(
                "INSERT INTO files(path, kind, size, mtime, inode, device, bucket) VALUES(?, ?, ?, ?, ?, ?, ?)",
                vec![
                    Sql::Text(file.path.clone()),
                    Sql::Text(file.kind.as_str().to_string()),
                    Sql::Int(file.size),
                    Sql::Double(file.mtime),
                    Sql::Int(file.inode as i64),
                    Sql::Int(file.device as i64),
                    Sql::Text(bucket_of(&file.path)),
                ],
            )?;
            let id = self.db.last_insert_rowid();
            self.add_name_row(id, &file.path)?;
            Ok(id)
        })
    }

    /// Every file under one of `roots` (IndexStore.swift:603). A folder inside another listed is
    /// already covered; each file once, also where two spellings of a folder meet.
    /// v7: the roots map onto the anchors — an anchor inside a root answers whole, a root under
    /// an anchor answers its relative range (both spellings: `/var` reaches `/private/var`
    /// rows, the law `path_ranges` rides in v6); answers carry the ASSEMBLED absolute path.
    pub fn files_under(&self, roots: &[String]) -> Result<Vec<FileRecord>> {
        // Root strings arrive from callers in any spelling; the byte-wise ranges below
        // only see the engine's ONE form (slashes + standardize) — see `set_scope`.
        let normalized: Vec<String> = roots
            .iter()
            .map(|root| crate::paths::standardize(&crate::paths::to_slashes(root)))
            .collect();
        let roots: &[String] = &normalized;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if self.is_v7() {
            for root in roots {
                if roots
                    .iter()
                    .any(|other| crate::paths::is_inside(root, other))
                {
                    continue;
                }
                let root_bound = crate::paths::bound(root);
                for anchor in self.anchors()? {
                    // One mapping per spelling: None = the anchor sits under the root, answer
                    // whole; Some = the root sits under the anchor, answer its relative range.
                    let mut mappings: Vec<Option<(String, String)>> = Vec::new();
                    for spelling in std::iter::once(&anchor.abs).chain(anchor.real.iter()) {
                        if root == spelling || crate::paths::is_inside(spelling, root) {
                            mappings.push(None);
                            continue;
                        }
                        let spelling_bound = crate::paths::bound(spelling);
                        if root.starts_with(&spelling_bound) {
                            // The v6 bound+MAX shape: the lo keeps its trailing slash, so a
                            // sibling folder ("photos1/x") falls outside ("photos1/" > "photos/￿").
                            let lo = root_bound[spelling_bound.len()..].to_string();
                            mappings.push(Some((lo.clone(), format!("{lo}{}", char::MAX))));
                        }
                    }
                    for mapping in mappings {
                        let (sql, args) = match mapping {
                            None => (
                                format!(
                                    "SELECT {} FROM {} WHERE f.anchor = ?",
                                    self.select_cols(),
                                    self.files_from()
                                ),
                                vec![Sql::Int(anchor.id)],
                            ),
                            Some((lo, hi)) => (
                                format!(
                                    "SELECT {} FROM {} WHERE f.anchor = ? AND f.path >= ? AND f.path < ?",
                                    self.select_cols(),
                                    self.files_from()
                                ),
                                vec![Sql::Int(anchor.id), Sql::Text(lo), Sql::Text(hi)],
                            ),
                        };
                        self.db.query(&sql, args, |row| {
                            let record = IndexStore::record(row);
                            if seen.insert(record.id) {
                                out.push(record);
                            }
                            Ok(())
                        })?;
                    }
                }
            }
            return Ok(out);
        }
        for root in roots {
            if roots
                .iter()
                .any(|other| crate::paths::is_inside(root, other))
            {
                continue;
            }
            for range in path_ranges(root) {
                self.db.query(
                    &format!(
                        "SELECT {} FROM files WHERE path >= ? AND path < ?",
                        Self::FILE_COLUMNS
                    ),
                    vec![Sql::Text(range.0), Sql::Text(range.1)],
                    |row| {
                        let record = IndexStore::record(row);
                        if seen.insert(record.id) {
                            out.push(record);
                        }
                        Ok(())
                    },
                )?;
            }
        }
        Ok(out)
    }

    /// The row sharing a file's `(device, inode)`, wherever its path is (issue #7a: a scoped
    /// sync's move across the scope's edge — `Indexer::sync_scoped`'s reason). The lookup's
    /// shape is the schema's own `CREATE INDEX files_inode ON files(device, inode)`; a
    /// store-wide scan is not. Two spellings of one file are one inode, and re-pathing any
    /// of them lands on the live path, so the newest row is as good a pick as any.
    pub fn file_by_inode(&self, device: u64, inode: u64) -> Result<Option<FileRecord>> {
        let mut found: Option<FileRecord> = None;
        // v7 (#195): the lookup rides the assembly — `select_cols` + the anchors join, the
        // (device, inode) filter qualified for the joined FROM. The record's path answers
        // ASSEMBLED absolute, the shape every consumer follows (#7a's move detection
        // re-paths rows, and the relative form is not a client fact). v6 reads verbatim.
        self.db.query(
            &format!(
                "SELECT {} FROM {} WHERE {} = ? AND {} = ? LIMIT 1",
                self.select_cols(),
                self.files_from(),
                self.qualified("device"),
                self.qualified("inode")
            ),
            vec![Sql::Int(device as i64), Sql::Int(inode as i64)],
            |row| {
                let record = IndexStore::record(row);
                if found.is_none() {
                    found = Some(record);
                }
                Ok(())
            },
        )?;
        Ok(found)
    }

    /// The file changed on disk: drop what was extracted from it and queue it again
    /// (`requeue(_:as:)`, IndexStore.swift:634) — kind/size/date/inode/device ride the new
    /// reading; the name row stays.
    pub fn requeue_as(&self, id: i64, file: &Candidate) -> Result<()> {
        self.write_guard()?;
        self.db.transaction(|| {
            self.delete_segments(id, true)?;
            self.db.run(
                "UPDATE files SET kind = ?, size = ?, mtime = ?, inode = ?, device = ?, state = 'pending', detail = NULL,
                    info = NULL, indexed_at = NULL WHERE id = ?",
                vec![
                    Sql::Text(file.kind.as_str().to_string()),
                    Sql::Int(file.size),
                    Sql::Double(file.mtime),
                    Sql::Int(file.inode as i64),
                    Sql::Int(file.device as i64),
                    Sql::Int(id),
                ],
            )
        })
    }

    /// The file changed on disk: drop what was extracted and queue it again (keeps the name row).
    pub fn requeue(&self, id: i64) -> Result<()> {
        self.write_guard()?;
        self.requeue_raw(id)
    }

    /// Unguarded `requeue` for the migration chain (it runs INSIDE a migration, where the
    /// store's layout is being rewritten under it — the guard is for runtime writers).
    fn requeue_raw(&self, id: i64) -> Result<()> {
        self.db.transaction(|| {
            self.delete_segments(id, true)?;
            self.db.run(
                "UPDATE files SET state = 'pending', detail = NULL, info = NULL, indexed_at = NULL WHERE id = ?",
                vec![Sql::Int(id)],
            )
        })
    }

    /// Same inode/size/date at a new path: a move. Only path/bucket change (v7: the anchor too,
    /// when the move crosses roots); nothing re-embedded — the vectors ride `segments.vec`,
    /// which a path UPDATE never touches (the FK is delete-only).
    pub fn move_file(&self, id: i64, to: &str) -> Result<()> {
        self.write_guard()?;
        if !self.is_v7() {
            return self.move_file_v6(id, to);
        }
        let to = crate::paths::standardize(&crate::paths::to_slashes(to));
        self.db.transaction(|| {
            let (anchor, rel) = self.split_for_write(&to)?;
            let bucket = self.prefixed_bucket(anchor, &rel);
            self.db.run(
                "UPDATE files SET anchor = ?, path = ?, bucket = ? WHERE id = ?",
                vec![Sql::Int(anchor), Sql::Text(rel), Sql::Text(bucket), Sql::Int(id)],
            )?;
            self.db.run(
                "UPDATE fts SET name = ?, body = ? WHERE rowid IN (SELECT id FROM segments WHERE file = ? AND kind = 'name')",
                vec![
                    Sql::Text(normalized(&name_text(&to))),
                    Sql::Text(normalized(&folder_text(&to))),
                    Sql::Int(id),
                ],
            )
        })
    }

    fn move_file_v6(&self, id: i64, to: &str) -> Result<()> {
        self.db.transaction(|| {
            self.db.run(
                "UPDATE files SET path = ?, bucket = ? WHERE id = ?",
                vec![Sql::Text(to.to_string()), Sql::Text(bucket_of(to)), Sql::Int(id)],
            )?;
            self.db.run(
                "UPDATE fts SET name = ?, body = ? WHERE rowid IN (SELECT id FROM segments WHERE file = ? AND kind = 'name')",
                vec![
                    Sql::Text(normalized(&name_text(to))),
                    Sql::Text(normalized(&folder_text(to))),
                    Sql::Int(id),
                ],
            )
        })
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.write_guard()?;
        self.db.run(
            "DELETE FROM fts WHERE rowid IN (SELECT id FROM segments WHERE file = ?)",
            vec![Sql::Int(id)],
        )?;
        self.db
            .run("DELETE FROM files WHERE id = ?", vec![Sql::Int(id)])
    }

    /// Queues every file again and forgets the fingerprint (after the model/settings changed).
    pub fn reset_all(&self) -> Result<()> {
        self.write_guard()?;
        self.db.transaction(|| {
            self.db.run(
                "DELETE FROM fts WHERE rowid IN (SELECT id FROM segments WHERE kind != 'name')",
                vec![],
            )?;
            self.db
                .run("DELETE FROM segments WHERE kind != 'name'", vec![])?;
            self.db.run(
                "UPDATE files SET state = 'pending', detail = NULL, info = NULL, indexed_at = NULL",
                vec![],
            )?;
            self.set_meta("fingerprint", None)
        })
    }

    /// The mode-qualified `id` column for filters in `files_from()`'s FROM: v7's join makes a
    /// bare `id` ambiguous (`files.id` vs `anchors.id`) — the `only:` filter must qualify.
    fn qualified_id(&self) -> &'static str {
        if self.is_v7() {
            "f.id"
        } else {
            "id"
        }
    }

    /// A mode-qualified column name for filters in `files_from()`'s FROM: v7's join spells
    /// the files table as `f`; v6's FROM is the aliasless table.
    fn qualified(&self, col: &str) -> String {
        if self.is_v7() {
            format!("f.{col}")
        } else {
            col.to_string()
        }
    }

    /// Files waiting to be embedded, in indexing order; `only` narrows it to some files
    /// (`pending`, IndexStore.swift:699). An empty `only` answers empty, as Swift does.
    pub fn pending(
        &self,
        limit: Option<i64>,
        only: Option<&std::collections::HashSet<i64>>,
    ) -> Result<Vec<FileRecord>> {
        if let Some(ids) = only {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
        }
        let filter = only.map(|ids| {
            let mut sorted: Vec<i64> = ids.iter().copied().collect();
            sorted.sort_unstable();
            format!(
                " AND {} IN ({})",
                self.qualified_id(),
                sorted
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        });
        let sql = format!(
            "SELECT {} FROM {} WHERE state = 'pending'{}{}
             ORDER BY {}, mtime DESC LIMIT ?",
            self.select_cols(),
            self.files_from(),
            filter.as_deref().unwrap_or(""),
            self.scope_sql(),
            FileKind::INDEXING_ORDER_SQL,
        );
        let mut out = Vec::new();
        self.db
            .query(&sql, vec![Sql::Int(limit.unwrap_or(-1))], |row| {
                out.push(IndexStore::record(row));
                Ok(())
            })?;
        Ok(out)
    }

    /// The kinds of the files waiting to be embedded (`pendingKinds`, IndexStore.swift:737):
    /// the queue's look the shared embedder decides the model's shape from (issue #155) — a run
    /// of text and code alone never loads the image and audio encoders.
    pub fn pending_kinds(
        &self,
        only: Option<&std::collections::HashSet<i64>>,
    ) -> Result<std::collections::HashSet<FileKind>> {
        if let Some(ids) = only {
            if ids.is_empty() {
                return Ok(std::collections::HashSet::new());
            }
        }
        let filter = only.map(|ids| {
            let mut sorted: Vec<i64> = ids.iter().copied().collect();
            sorted.sort_unstable();
            format!(
                " AND {} IN ({})",
                self.qualified_id(),
                sorted
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        });
        let sql = format!(
            "SELECT DISTINCT kind FROM {} WHERE state = 'pending'{}{}",
            self.files_from(),
            filter.as_deref().unwrap_or(""),
            self.scope_sql(),
        );
        let mut out = std::collections::HashSet::new();
        self.db.query(&sql, [], |row| {
            if let Some(kind) = FileKind::parse(&row.text(0).unwrap_or_default()) {
                out.insert(kind);
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// Long files with pages or chunks still to read after their first step, in indexing order
    /// (`unfinished`, IndexStore.swift:749).
    pub fn unfinished(&self) -> Result<Vec<FileRecord>> {
        let sql = format!(
            "SELECT {} FROM {} WHERE state = 'partial'{} ORDER BY {}, mtime DESC",
            self.select_cols(),
            self.files_from(),
            self.scope_sql(),
            FileKind::INDEXING_ORDER_SQL,
        );
        let mut out = Vec::new();
        self.db.query(&sql, [], |row| {
            out.push(IndexStore::record(row));
            Ok(())
        })?;
        Ok(out)
    }

    /// What indexing learned about a file (`info(of:)`, IndexStore.swift:780). Swift casts the
    /// JSON to `[String: String]`: one non-string value fails the cast and the whole info reads
    /// empty, mirrored here.
    pub fn info_of(&self, id: i64) -> Result<std::collections::HashMap<String, String>> {
        let mut info = std::collections::HashMap::new();
        self.db.query(
            "SELECT info FROM files WHERE id = ?",
            vec![Sql::Int(id)],
            |row| {
                if let Some(json) = row.text(0) {
                    if let Ok(serde_json::Value::Object(map)) =
                        serde_json::from_str::<serde_json::Value>(&json)
                    {
                        let strings: std::collections::HashMap<String, String> = map
                            .iter()
                            .filter_map(|(key, value)| {
                                value.as_str().map(|text| (key.clone(), text.to_string()))
                            })
                            .collect();
                        if strings.len() == map.len() {
                            info = strings;
                        }
                    }
                }
                Ok(())
            },
        )?;
        Ok(info)
    }

    /// The last page or chunk stored for a file (`lastPlace(of:)`, IndexStore.swift:791): where a
    /// long file's reading got to, when its info doesn't say.
    pub fn last_place(&self, id: i64) -> Result<Option<i64>> {
        let mut last = None;
        self.db.query(
            "SELECT MAX(loc) FROM segments WHERE file = ? AND kind IN ('page', 'page_image', 'chunk')",
            vec![Sql::Int(id)],
            |row| {
                last = if row.is_null(0) {
                    None
                } else {
                    Some(row.double(0) as i64)
                };
                Ok(())
            },
        )?;
        Ok(last)
    }

    /// What indexing learned about a file (IndexStore.swift:798). `info` is stored as sorted-key
    /// JSON (the counters' `leftsum` reads `read_to`/`read_end` from it via `json_extract`).
    pub fn finish(
        &self,
        id: i64,
        state: &str,
        detail: Option<&str>,
        info: HashMap<String, String>,
    ) -> Result<()> {
        self.write_guard()?;
        let json = if info.is_empty() {
            None
        } else {
            Some(json_object(&info))
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.db.run(
            "UPDATE files SET state = ?, detail = ?, info = ?, indexed_at = ? WHERE id = ?",
            vec![
                Sql::Text(state.to_string()),
                Sql::opt_text(detail),
                Sql::opt_text(json.as_deref()),
                Sql::Double(now),
                Sql::Int(id),
            ],
        )
    }

    // MARK: Segments

    /// Adds a segment; `text` goes into keyword search (normalized), `vector` into meaning search.
    // The 9-arg shape is the engine's `addSegment` signature (IndexStore.swift:847), kept verbatim.
    #[allow(clippy::too_many_arguments)]
    pub fn add_segment(
        &self,
        file: i64,
        kind: SegmentKind,
        modality: Modality,
        loc: Option<f64>,
        loc_end: Option<f64>,
        excerpt: Option<&str>,
        text: Option<&str>,
        vector: Option<&[f32]>,
    ) -> Result<()> {
        self.write_guard()?;
        self.db.transaction(|| {
            let modality_text = if vector.is_some() { modality } else { Modality::None };
            let blob = vector.map(vectors::half_bytes);
            self.db.run(
                "INSERT INTO segments(file, kind, modality, loc, loc_end, excerpt, vec) VALUES(?, ?, ?, ?, ?, ?, ?)",
                vec![
                    Sql::Int(file),
                    Sql::Text(kind.as_str().to_string()),
                    Sql::Text(modality_text.as_str().to_string()),
                    Sql::opt_double(loc),
                    Sql::opt_double(loc_end),
                    Sql::opt_text(excerpt),
                    match blob {
                        Some(b) => Sql::Blob(b),
                        None => Sql::Null,
                    },
                ],
            )?;
            if let Some(t) = text {
                if !t.is_empty() {
                    let (body, original) = keyword_text(t);
                    self.db.run(
                        "INSERT INTO fts(rowid, name, body, original) VALUES(?, '', ?, ?)",
                        vec![Sql::Int(self.db.last_insert_rowid()), Sql::Text(body), Sql::opt_text(original.as_deref())],
                    )?;
                }
            }
            Ok(())
        })
    }

    fn delete_segments(&self, file: i64, keeping_name: bool) -> Result<()> {
        let filter = if keeping_name {
            "AND kind != 'name'"
        } else {
            ""
        };
        self.db.run(
            &format!(
                "DELETE FROM fts WHERE rowid IN (SELECT id FROM segments WHERE file = ? {filter})"
            ),
            vec![Sql::Int(file)],
        )?;
        self.db.run(
            &format!("DELETE FROM segments WHERE file = ? {filter}"),
            vec![Sql::Int(file)],
        )
    }

    fn add_name_row(&self, file: i64, path: &str) -> Result<()> {
        self.db.run(
            "INSERT INTO segments(file, kind, modality) VALUES(?, 'name', 'none')",
            vec![Sql::Int(file)],
        )?;
        self.db.run(
            "INSERT INTO fts(rowid, name, body) VALUES(?, ?, ?)",
            vec![
                Sql::Int(self.db.last_insert_rowid()),
                Sql::Text(normalized(&name_text(path))),
                Sql::Text(normalized(&folder_text(path))),
            ],
        )
    }

    // MARK: Stats (counters reads; #193)

    pub fn counts(&self) -> Result<Counts> {
        let mut counts = Counts::default();
        let row_sql = if self.scope_active() {
            "SELECT c.kind, c.state, SUM(c.n) FROM scopeb b CROSS JOIN counters c ON c.bucket >= b.lo AND c.bucket < b.hi GROUP BY c.kind, c.state"
        } else {
            "SELECT kind, state, SUM(n) FROM counters GROUP BY kind, state"
        };
        self.db.query(row_sql, [], |row| {
            let n = row.int(2);
            if n == 0 {
                return Ok(());
            }
            counts
                .by_kind_and_state
                .entry(row.text(0).unwrap_or("?".to_string()))
                .or_default()
                .insert(row.text(1).unwrap_or("?".to_string()), n);
            Ok(())
        })?;
        let (unfinished, left) = self.unfinished_count()?;
        counts.unfinished = unfinished;
        counts.left = left;
        let seg_sql = if self.scope_active() {
            "SELECT c.hasvec, SUM(c.n) FROM scopeb b CROSS JOIN segcounters c ON c.bucket >= b.lo AND c.bucket < b.hi GROUP BY c.hasvec"
        } else {
            "SELECT hasvec, SUM(n) FROM segcounters GROUP BY hasvec"
        };
        self.db.query(seg_sql, [], |row| {
            counts.segments += row.int(1);
            if row.int(0) == 1 {
                counts.vectors += row.int(1);
            }
            Ok(())
        })?;
        Ok(counts)
    }

    pub fn pending_count(&self) -> Result<i64> {
        let mut count = 0;
        if !self.scope_active() {
            self.db.query(
                "SELECT COALESCE(SUM(n), 0) FROM counters WHERE state = 'pending'",
                [],
                |row| {
                    count = row.int(0);
                    Ok(())
                },
            )?;
        } else {
            self.db.query("SELECT COALESCE(SUM(c.n), 0) FROM scopeb b CROSS JOIN counters c ON c.state = 'pending' AND c.bucket >= b.lo AND c.bucket < b.hi", [], |row| {
                count = row.int(0);
                Ok(())
            })?;
        }
        Ok(count)
    }

    pub fn unfinished_count(&self) -> Result<(i64, i64)> {
        let mut files = 0;
        let mut left = 0;
        if !self.scope_active() {
            self.db.query("SELECT COALESCE(SUM(n), 0), COALESCE(SUM(leftsum), 0) FROM counters WHERE state = 'partial'", [], |row| {
                files = row.int(0);
                left = row.int(1);
                Ok(())
            })?;
        } else {
            self.db.query("SELECT COALESCE(SUM(c.n), 0), COALESCE(SUM(c.leftsum), 0) FROM scopeb b CROSS JOIN counters c ON c.state = 'partial' AND c.bucket >= b.lo AND c.bucket < b.hi", [], |row| {
                files = row.int(0);
                left = row.int(1);
                Ok(())
            })?;
        }
        Ok((files, left))
    }

    // MARK: What the index services ask of the store (IndexService.swift)

    /// Files under none of the kept folders leave the index — the app's folders taken out
    /// (`prune(keepingUnder:)`, IndexStore.swift:674). The kept prefixes are `pathRanges`'
    /// `from` bounds, so a folder under `/var` keeps `/private/var` rows too (the same
    /// aliasing the scope ranges make). Each gone file is deleted as the store deletes a
    /// file that moved away.
    pub fn prune_keeping_under(&self, roots: &[String]) -> Result<i64> {
        self.write_guard()?;
        let prefixes: Vec<String> = roots
            .iter()
            // The engine's ONE path form (see `set_scope`): the byte-wise prefix test
            // below compares against rows stored in slash form.
            .map(|root| crate::paths::standardize(&crate::paths::to_slashes(root)))
            .flat_map(|root| path_ranges(&root))
            .map(|range| range.0)
            .collect();
        let mut gone: Vec<i64> = Vec::new();
        // v7 compares the ASSEMBLED absolute path against the same prefixes (the ghost-row
        // cure is root-shape law, not storage-shape law); v6 reads the stored path.
        let sql = format!("SELECT {} FROM {}", self.select_abs(), self.files_from());
        self.db.query(&sql, [], |row| {
            let path = row.text(1).unwrap_or_default();
            if !prefixes.iter().any(|p| path.starts_with(p)) {
                gone.push(row.int(0));
            }
            Ok(())
        })?;
        if gone.is_empty() {
            return Ok(0);
        }
        self.db.transaction(|| {
            for id in &gone {
                self.delete(*id)?;
            }
            Ok(())
        })?;
        Ok(gone.len() as i64)
    }

    /// What older indexes couldn't read the first time — a scanned page's words (once OCR
    /// exists) and a small vector picture (once the system can draw it) — queued once,
    /// each capability stamped in `meta` so the pass happens only once per index
    /// (`readAgainOnce`, IndexStore.swift:810). Both capabilities are Apple-only today:
    /// the portable `OCR.isAvailable` and `ImageExtractor.drawsVectorPictures` are false,
    /// so on this platform the answer is 0 by the engine's own reading.
    pub fn read_again_once(&self) -> Result<i64> {
        self.write_guard()?;
        let (ocr_available, draws_pictures) = (false, false);
        let mut files: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let mut flags: Vec<&str> = Vec::new();
        if ocr_available && self.meta("scans_ocr")?.is_none() {
            self.db.query(
                "SELECT DISTINCT file FROM segments WHERE kind = 'page_image'",
                [],
                |row| {
                    files.insert(row.int(0));
                    Ok(())
                },
            )?;
            flags.push("scans_ocr");
        }
        if draws_pictures && self.meta("pictures_drawn")?.is_none() {
            // The mark is the engine's own skip detail: "small image w×h" for any w, h.
            self.db.query(
                "SELECT id FROM files WHERE kind = 'image' AND state = 'skipped' \
                 AND detail LIKE 'small image %'",
                [],
                |row| {
                    files.insert(row.int(0));
                    Ok(())
                },
            )?;
            flags.push("pictures_drawn");
        }
        if flags.is_empty() {
            return Ok(0);
        }
        self.db.transaction(|| {
            for file in &files {
                self.requeue(*file)?;
            }
            for flag in &flags {
                self.set_meta(flag, Some("1"))?;
            }
            Ok(())
        })?;
        Ok(files.len() as i64)
    }

    /// A file the indexing helper had to stop on (reading it ran away): it isn't read
    /// again until it changes (`markStopped`, IndexStore.swift:832). A long file that was
    /// being finished keeps what was read of it — `partial` closes to `done` with the
    /// note; anything else becomes `failed`. False when no file has that path.
    pub fn mark_stopped(&self, path: &str, why: &str) -> Result<bool> {
        self.write_guard()?;
        let mut found: Option<(i64, String)> = None;
        if self.is_v7() {
            // The stopped-file note carries an absolute path; the row lookup rides the
            // (anchor, rel) key. A path under no root names no row — the honest false.
            let path = crate::paths::standardize(&crate::paths::to_slashes(path));
            let Ok((anchor, rel)) = self.split_anchor(&path) else {
                return Ok(false);
            };
            self.db.query(
                "SELECT id, state FROM files WHERE anchor = ? AND path = ?",
                [Sql::Int(anchor), Sql::Text(rel)],
                |row| {
                    found = Some((row.int(0), row.text(1).unwrap_or_default()));
                    Ok(())
                },
            )?;
        } else {
            self.db.query(
                "SELECT id, state FROM files WHERE path = ?",
                [Sql::Text(path.to_string())],
                |row| {
                    found = Some((row.int(0), row.text(1).unwrap_or_default()));
                    Ok(())
                },
            )?;
        }
        let Some((id, state)) = found else {
            return Ok(false);
        };
        if state == "partial" {
            self.finish(
                id,
                "done",
                Some(&format!("The rest couldn't be read: it {why}")),
                HashMap::new(),
            )?;
        } else {
            self.finish(id, "failed", Some(&format!("It {why}")), HashMap::new())?;
        }
        Ok(true)
    }

    // MARK: Full aggregation — the oracle (#193 keeps it; the counters tests compare against it)

    pub fn full_pending_count(&self) -> Result<i64> {
        let mut count = 0;
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE state = 'pending'{}",
            self.files_from(),
            self.scope_sql()
        );
        self.db.query(&sql, [], |row| {
            count = row.int(0);
            Ok(())
        })?;
        Ok(count)
    }

    pub fn full_unfinished_count(&self) -> Result<(i64, i64)> {
        let mut files = 0;
        let mut left = 0;
        let sql = format!(
            "SELECT COUNT(*), SUM(MAX(0, CAST(json_extract(info, '$.read_end') AS INTEGER) - CAST(json_extract(info, '$.read_to') AS INTEGER))) FROM {} WHERE state = 'partial'{}",
            self.files_from(),
            self.scope_sql()
        );
        self.db.query(&sql, [], |row| {
            files = row.int(0);
            left = row.int(1);
            Ok(())
        })?;
        Ok((files, left))
    }

    pub fn full_counts(&self) -> Result<Counts> {
        let mut counts = Counts::default();
        let sql = format!(
            "SELECT kind, state, COUNT(*) FROM {} WHERE 1{} GROUP BY kind, state",
            self.files_from(),
            self.scope_sql()
        );
        self.db.query(&sql, [], |row| {
            counts
                .by_kind_and_state
                .entry(row.text(0).unwrap_or("?".to_string()))
                .or_default()
                .insert(row.text(1).unwrap_or("?".to_string()), row.int(2));
            Ok(())
        })?;
        let (unfinished, left) = self.full_unfinished_count()?;
        counts.unfinished = unfinished;
        counts.left = left;
        let in_scope = if self.scope_active() {
            format!(
                " AND file IN (SELECT f.id FROM {} WHERE 1{})",
                self.files_from(),
                self.scope_sql()
            )
        } else {
            String::new()
        };
        self.db.query(
            &format!("SELECT COUNT(*), COUNT(vec) FROM segments WHERE kind != 'name'{in_scope}"),
            [],
            |row| {
                counts.segments = row.int(0);
                counts.vectors = row.int(1);
                Ok(())
            },
        )?;
        Ok(counts)
    }
}

// MARK: Helpers (IndexStore.swift)

/// `keywordText` (IndexStore.swift:862): the normalized text, and the text as written if that differs.
fn keyword_text(text: &str) -> (String, Option<String>) {
    let norm = normalized(text);
    (
        norm.clone(),
        if norm == text {
            None
        } else {
            Some(text.to_string())
        },
    )
}

/// The `info` JSON, sorted keys (IndexStore.swift's `JSONSerialization([.sortedKeys])`): the shape
/// the counters read via `json_extract(info, '$.read_to')`. Values are stored as strings.
fn json_object(map: &HashMap<String, String>) -> String {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let parts: Vec<String> = keys
        .into_iter()
        .map(|k| format!("{}:{}", json_str(k), json_str(&map[k])))
        .collect();
    format!("{{{}}}", parts.join(","))
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn name_text(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn folder_text(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut folder = Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !home.is_empty() && folder.starts_with(&home) {
        folder = folder[home.len()..].to_string();
    }
    folder
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Writes the merged ranges of one scope table in one transaction (IndexStore.swift:57). Multi-row
/// INSERTs are kept under SQLite's compound-select cap (200 rows per statement).
fn create_scope(db: &Database, table: &str, ranges: &[(String, String)]) -> Result<()> {
    db.execute(&format!("CREATE TEMP TABLE IF NOT EXISTS {table}(lo TEXT NOT NULL, hi TEXT NOT NULL, PRIMARY KEY(lo, hi)) WITHOUT ROWID"))?;
    db.execute(&format!("DELETE FROM {table}"))?;
    let mut start = 0;
    while start < ranges.len() {
        let end = (start + 200).min(ranges.len());
        let chunk = &ranges[start..end];
        let values: Vec<Sql> = chunk
            .iter()
            .flat_map(|r| [Sql::Text(r.0.clone()), Sql::Text(r.1.clone())])
            .collect();
        let placeholders = vec!["(?, ?)"; chunk.len()].join(",");
        db.run(
            &format!("INSERT INTO {table}(lo, hi) VALUES{placeholders}"),
            values,
        )?;
        start = end;
    }
    Ok(())
}

/// The v7 scope table (#195): ranges are anchor-relative, so the temp table carries the anchor
/// and the scoped predicate matches `f.anchor` — a rel range is only meaningful inside its
/// anchor. The bucket scope table (`scopeb`) rides `create_scope` unchanged: anchor-prefixed
/// bucket strings are globally unique, so v6's range join is exact in v7 too.
fn create_scope_anchor(db: &Database, table: &str, ranges: &[(i64, String, String)]) -> Result<()> {
    db.execute(&format!("CREATE TEMP TABLE IF NOT EXISTS {table}(anchor INTEGER NOT NULL, lo TEXT NOT NULL, hi TEXT NOT NULL, PRIMARY KEY(anchor, lo)) WITHOUT ROWID"))?;
    db.execute(&format!("DELETE FROM {table}"))?;
    let mut start = 0;
    while start < ranges.len() {
        let end = (start + 200).min(ranges.len());
        let chunk = &ranges[start..end];
        let values: Vec<Sql> = chunk
            .iter()
            .flat_map(|r| {
                [
                    Sql::Int(r.0),
                    Sql::Text(r.1.clone()),
                    Sql::Text(r.2.clone()),
                ]
            })
            .collect();
        let placeholders = vec!["(?, ?, ?)"; chunk.len()].join(",");
        db.run(
            &format!("INSERT INTO {table}(anchor, lo, hi) VALUES{placeholders}"),
            values,
        )?;
        start = end;
    }
    Ok(())
}
