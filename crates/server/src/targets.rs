//! Named indexes (stage-13, `targets.py`): the standalone media target plus
//! per-application targets. The main media index behaves exactly as it always
//! has; an application target is a NAMED (roots, index) pair the same server
//! supervises independently — its own writer lock (the flock lives beside the
//! index folder), its own jobs log, its own search-service child, its own
//! token. The engine changes nothing: it answers for any (roots, --index) pair.
//!
//! Registry sources, merged with env winning per name:
//!   DIGUP_SERVER_APP_TARGETS=nc=/media/nc:/media/nc2:/idx/nc;dc=/media/dc:/idx/dc
//!   `<config>/targets.json` — registrations made at runtime, persisted.
//! Validation is strict on purpose: names are lowercase slugs, roots and index
//! absolute, the index may not be the main index, `media` is reserved. A
//! malformed entry is refused at startup quoting the entry — a silently-dropped
//! target is an outage nobody asked for.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use regex::Regex;
use serde_json::Value;

/// `MAIN_TARGET` (targets.py): the main media index's own name — reserved.
pub const MAIN_TARGET: &str = "media";

/// Python's `{x!r}` for a string — `'nc'` with single quotes (a literal `'`
/// rides as `\'`, a backslash as `\\`, control chars as `\xNN`). The 400/409
/// detail texts quote names this way, and the gate compares them byte for
/// byte: Rust's `{:?}` prints `"nc"` — double quotes — which is NOT Python's
/// repr for a plain string.
pub fn py_repr(text: &str) -> String {
    let mut out = String::from("'");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub roots: Vec<String>,
    pub index: String,
    pub main: bool,
}

/// `^[a-z0-9][a-z0-9-]{0,31}$` — names ride in log lines, jobs files, and URLs.
fn name_ok(name: &str) -> bool {
    Regex::new(r"^[a-z0-9][a-z0-9-]{0,31}$")
        .expect("slug regex")
        .is_match(name)
}

/// `_validated`: slug rules, absolute-path rules, the "not the main index"
/// rule. The ValueError texts ride along — the API answers 400 with them.
pub fn validated(
    name: &str,
    roots_blob: &str,
    index: &str,
    main_index: Option<&str>,
) -> Result<Target, String> {
    if !name_ok(name) {
        return Err(format!(
            "application target name {} is not a lowercase slug (letters, digits, dashes, starting with a letter)",
            py_repr(name)
        ));
    }
    if name == MAIN_TARGET {
        return Err("the target name 'media' is the main media index's own — reserved".into());
    }
    let roots: Vec<String> = roots_blob
        .split(':')
        .filter(|part| !part.trim().is_empty())
        .map(|part| part.to_string())
        .collect();
    if roots.is_empty() {
        return Err(format!("application target {} has no roots", py_repr(name)));
    }
    for root in &roots {
        if !Path::new(root).is_absolute() {
            return Err(format!(
                "application target {} root {} is not an absolute path",
                py_repr(name),
                py_repr(root)
            ));
        }
    }
    if !Path::new(index).is_absolute() {
        return Err(format!(
            "application target {} index {} is not an absolute path",
            py_repr(name),
            py_repr(index)
        ));
    }
    if let Some(main) = main_index {
        // os.path.realpath: the missing tail is no obstacle — resolve what is
        // there, keep the rest.
        if resolve(index) == resolve(main) {
            return Err(format!(
                "application target {} index is the main index — targets are separate indexes by definition (docs/API.md, named indexes)",
                py_repr(name)
            ));
        }
    }
    Ok(Target {
        name: name.to_string(),
        roots,
        index: index.to_string(),
        main: false,
    })
}

/// `parse_app_targets`: entries split on ';', each `name=roots:index` (roots
/// colon-separated, index the LAST folder). Raises quoting the entry.
pub fn parse_app_targets(value: &str, main_index: Option<&str>) -> Result<Vec<Target>, String> {
    let mut targets = Vec::new();
    for entry in value.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, sep, rest) = match entry.find('=') {
            Some(at) => (&entry[..at], true, &entry[at + 1..]),
            None => (entry, false, ""),
        };
        let parts: Vec<&str> = rest
            .split(':')
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect();
        if !sep || parts.len() < 2 {
            return Err(format!(
                "DIGUP_SERVER_APP_TARGETS: {} is not 'name=roots:index' (the index is the last colon-separated folder)",
                py_repr(entry)
            ));
        }
        let name = name.trim().to_lowercase();
        targets.push(validated(
            &name,
            &parts[..parts.len() - 1].join(":"),
            parts[parts.len() - 1].trim(),
            main_index,
        )?);
    }
    Ok(targets)
}

/// `load_targets_file`: a missing file is the empty registry; a corrupt one is
/// refused loudly (config files are operator facts, not suggestions).
pub fn load_targets_file(path: &Path, main_index: &str) -> Result<Vec<Target>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|exc| format!("{}: not valid JSON ({})", path.to_string_lossy(), exc))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {}", path.to_string_lossy(), error)),
    };
    let mut targets = Vec::new();
    if let Some(map) = raw.as_object() {
        for (key, spec) in map {
            let name = key.trim().to_lowercase();
            let spec_obj = spec.as_object();
            let roots: Vec<String> = spec_obj
                .and_then(|spec| spec.get("roots"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| item.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            let index = spec_obj
                .and_then(|spec| spec.get("index"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            targets.push(validated(
                &name,
                &roots.join(":"),
                &index,
                Some(main_index),
            )?);
        }
    }
    Ok(targets)
}

/// The registry with PYTHON dict order: main first, then the file targets,
/// then env additions (env wins per name, keeping the existing position). The
/// `/targets` listing publishes this order, so it is part of the contract.
#[derive(Default)]
pub struct Registry {
    order: Vec<String>,
    by_name: BTreeMap<String, Target>,
}

impl Registry {
    pub fn get(&self, name: &str) -> Option<&Target> {
        self.by_name.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Target> {
        self.order.iter().filter_map(|name| self.by_name.get(name))
    }

    pub fn insert(&mut self, target: Target) {
        if !self.order.contains(&target.name) {
            self.order.push(target.name.clone());
        }
        self.by_name.insert(target.name.clone(), target);
    }
}

/// `build_registry`: main first, app targets from the persisted file, then env
/// (env wins on a name clash: the operator's env is the authority).
pub fn build_registry(
    main_roots: &[String],
    main_index: &str,
    config_dir: &Path,
) -> Result<Registry, String> {
    let mut registry = Registry::default();
    registry.insert(Target {
        name: MAIN_TARGET.to_string(),
        roots: main_roots.to_vec(),
        index: main_index.to_string(),
        main: true,
    });
    for target in load_targets_file(&config_dir.join("targets.json"), main_index)? {
        registry.insert(target);
    }
    for target in parse_app_targets(
        &std::env::var("DIGUP_SERVER_APP_TARGETS").unwrap_or_default(),
        Some(main_index),
    )? {
        registry.insert(target);
    }
    Ok(registry)
}

/// `target_for_path`: which target owns a media path — the longest matching
/// root wins (a target folder mounted under the media root is the more
/// specific owner), falling back to the main target.
pub fn target_for_path(registry: &Registry, path: &str) -> String {
    let mut best = MAIN_TARGET.to_string();
    let mut best_len = -1isize;
    let resolved = resolve(path);
    for target in registry.iter() {
        for root in &target.roots {
            let r = resolve(root);
            if (resolved == r || resolved.starts_with(&format!("{r}/")))
                && r.chars().count() as isize > best_len
            {
                best = target.name.clone();
                best_len = r.chars().count() as isize;
            }
        }
    }
    best
}

/// `Path.resolve()` / `os.path.realpath`: symlinks and `.`/`..` resolved
/// against what exists; a missing tail rides on after the existing prefix
/// (Python's strict=False shape — the media paths the engine published exist,
/// so this is their real path).
pub fn resolve(path: &str) -> String {
    let target = Path::new(path);
    let absolute = if target.is_absolute() {
        PathBuf::from(target)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(target)
    };
    match std::fs::canonicalize(&absolute) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => clean(&lexical(&absolute)).to_string_lossy().into_owned(),
    }
}

/// Make the path absolute-lexical: drop `.` components, apply `..` lexically
/// (Python's resolve() applies them against the resolved tree; on the
/// missing-tail path this is the shape realpath produces for the existing part).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::RootDir => out = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::Prefix(_) => {}
        }
    }
    out
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::RootDir => out = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::Prefix(_) => {}
        }
    }
    out
}
