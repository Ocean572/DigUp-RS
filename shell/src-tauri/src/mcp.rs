//! "Connect Claude Desktop" (Settings → General): the desktop app's MCP integration.
//!
//! Claude Desktop reads its Model Context Protocol servers from a settings file
//! (`claude_desktop_config.json` under the vendor's Claude folder — Application
//! Support on the Mac, `%APPDATA%` on Windows, `~/.config` on Linux). Turning the
//! switch on adds ONE server entry named `digup` pointing at this install's helper —
//! `digup mcp` with the app's own daemon arguments, the same folders, exclusions and
//! index `search_arguments` builds for the search helper, so the agent answers exactly
//! what this app indexes. Turning it off removes just that entry.
//!
//! The file is the person's, not ours: every other server and every other key is left
//! where it is, the write is atomic, the first write keeps one `.digupbak` copy, and a
//! file we cannot read as JSON is refused, never rebuilt — "your Claude settings are
//! yours" is the rule the panel states.

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Claude Desktop's settings file, per OS. A machine that names no user config folder
/// has none (and the switch says so) — the file's folder appearing is Claude's install.
pub fn config_file() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Claude"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("Claude"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|config| config.join("Claude"));
    base.map(|folder| folder.join("claude_desktop_config.json"))
}

/// The entry: `digup mcp` over the app's current daemon arguments. The `search-service`
/// token becomes `mcp` — the flags and folders after it are verbatim, so the agent reads
/// exactly what the app indexes (the entry refresh rides `apply_folders`, like the helper's
/// own launch list).
pub fn entry(helper: &Path, daemon_args: &[String]) -> Value {
    let mut args = vec!["mcp".to_string()];
    args.extend(daemon_args.iter().skip(1).cloned());
    json!({"command": helper.display().to_string(), "args": args})
}

/// Add/replace the `digup` server. A missing file inside an existing Claude folder is a
/// fresh install's shape: a clean document. A missing Claude folder is "not installed
/// here", said plainly — the app does not create another program's install folder.
pub fn enable(path: &Path, helper: &Path, daemon_args: &[String]) -> Result<(), String> {
    let folder_ok = path.parent().map(Path::exists).unwrap_or(false);
    if !folder_ok {
        return Err(
            "Claude Desktop is not installed here: its settings folder is not on this computer"
                .into(),
        );
    }
    let mut doc = read_document(path)?;
    backup_once(path)
        .map_err(|error| format!("cannot back up Claude Desktop's settings: {error}"))?;
    let servers = doc
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err(
            "Claude Desktop's mcpServers entry is not a settings list — fixing it is your call"
                .into(),
        );
    };
    servers.insert("digup".to_string(), entry(helper, daemon_args));
    write_document(path, &doc)
}

/// Remove the `digup` server, leaving every other entry. A missing file has nothing to
/// remove: a clean answer, and no file created to say so.
pub fn disable(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let mut doc = read_document(path)?;
    backup_once(path)
        .map_err(|error| format!("cannot back up Claude Desktop's settings: {error}"))?;
    if let Some(servers) = doc.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove("digup");
    }
    write_document(path, &doc)
}

fn read_document(path: &Path) -> Result<Map<String, Value>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("cannot read Claude Desktop's settings: {error}")),
    };
    if text.trim().is_empty() {
        return Ok(Map::new()); // an empty file means an empty document, not a broken one
    }
    match serde_json::from_str::<Value>(text.trim()) {
        Ok(Value::Object(doc)) => Ok(doc),
        Ok(_) => Err("Claude Desktop's settings are not a settings list — fixing them is your call".into()),
        Err(_) => Err(
            "Claude Desktop's settings do not read as JSON — fixing them is your call; DigUp will not rewrite them"
                .into(),
        ),
    }
}

fn write_document(path: &Path, doc: &Map<String, Value>) -> Result<(), String> {
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(doc.clone())).expect("a built document")
    );
    let temp = path.with_extension("json.digup-tmp");
    fs::write(&temp, &text)
        .map_err(|error| format!("cannot write Claude Desktop's settings: {error}"))?;
    fs::rename(&temp, path)
        .map_err(|error| format!("cannot write Claude Desktop's settings: {error}"))
}

/// The first write under our hand keeps one backup copy, named for our own touch.
fn backup_once(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.digupbak");
        if !backup.exists() {
            fs::copy(path, &backup)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon() -> Vec<String> {
        vec![
            "search-service".into(),
            "--index".into(),
            "/home/me/digup-index".into(),
            "--code-root".into(),
            "/home/me/code".into(),
            "/data/photos".into(),
        ]
    }

    #[test]
    fn the_entry_is_the_app_daemon_arguments_with_the_mcp_verb() {
        let entry = entry(Path::new("/usr/lib/DigUp/helper/digup"), &daemon());
        assert_eq!(entry["command"], "/usr/lib/DigUp/helper/digup");
        let args = entry["args"].as_array().expect("args");
        assert_eq!(args[0], "mcp");
        let joined = args
            .iter()
            .map(|a| a.as_str().expect("text"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!joined.contains("search-service"), "{joined}");
        assert!(joined.contains("--index /home/me/digup-index"), "{joined}");
        assert!(joined.contains("--code-root /home/me/code"), "{joined}");
        assert!(joined.ends_with("/data/photos"), "{joined}");
    }

    #[test]
    fn enabling_adds_only_the_digup_server_and_keeps_everything_else() {
        let work = tempfile::tempdir().expect("a work folder");
        let file = work.path().join("claude_desktop_config.json");
        fs::write(
            &file,
            r#"{"mcpServers":{"other":{"command":"x"}},"preferences":{"theme":"dark"}}"#,
        )
        .expect("an existing file");
        enable(&file, Path::new("/helper/digup"), &daemon()).expect("enable writes");
        let doc: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("read back")).expect("json");
        let servers = doc["mcpServers"].as_object().expect("servers");
        assert_eq!(servers.len(), 2, "the person's server and ours");
        assert!(servers.contains_key("other"));
        assert_eq!(
            doc["preferences"]["theme"], "dark",
            "a key outside mcpServers"
        );
        assert_eq!(servers["digup"]["args"].as_array().expect("args")[0], "mcp");
        assert!(
            work.path()
                .join("claude_desktop_config.json.digupbak")
                .exists(),
            "the first write backs up"
        );
    }

    #[test]
    fn disabling_removes_only_the_digup_server() {
        let work = tempfile::tempdir().expect("a work folder");
        let file = work.path().join("claude_desktop_config.json");
        fs::write(&file, r#"{"mcpServers":{"other":{"command":"x"}}}"#).expect("a file");
        enable(&file, Path::new("/helper/digup"), &daemon()).expect("enable");
        disable(&file).expect("disable");
        let doc: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("read back")).expect("json");
        let servers = doc["mcpServers"].as_object().expect("servers");
        assert!(!servers.contains_key("digup"), "our entry is gone");
        assert!(servers.contains_key("other"), "the person's entry stays");
    }

    #[test]
    fn a_fresh_install_file_is_created_and_a_missing_install_is_named() {
        let work = tempfile::tempdir().expect("a work folder");
        let file = work.path().join("claude_desktop_config.json");
        enable(&file, Path::new("/helper/digup"), &daemon())
            .expect("the folder is there, the file may be new");
        let doc: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("read back")).expect("json");
        assert!(doc["mcpServers"]["digup"].is_object());
        // No Claude folder at all: the switch's answer names the fact.
        let missing = work.path().join("nope/claude_desktop_config.json");
        assert_eq!(
            enable(&missing, Path::new("/helper/digup"), &daemon()).unwrap_err(),
            "Claude Desktop is not installed here: its settings folder is not on this computer"
        );
        disable(&missing).expect("nothing to remove is a clean answer");
        assert!(!missing.exists(), "disable created no file");
    }

    #[test]
    fn a_file_we_cannot_read_is_refused_not_rebuilt() {
        let work = tempfile::tempdir().expect("a work folder");
        let file = work.path().join("claude_desktop_config.json");
        fs::write(&file, "{ not json").expect("a broken file");
        let before = fs::read_to_string(&file).expect("read");
        let error = enable(&file, Path::new("/helper/digup"), &daemon()).expect_err("refused");
        assert!(error.contains("not read as JSON"), "{error}");
        assert_eq!(
            fs::read_to_string(&file).expect("read back"),
            before,
            "the person's bytes are untouched"
        );
        assert!(disable(&file).is_err(), "disable reads before it removes");
        assert_eq!(fs::read_to_string(&file).expect("still untouched"), before);
        fs::write(&file, r#"{"mcpServers":"not a list"}"#).expect("a hostile shape");
        assert!(enable(&file, Path::new("/helper/digup"), &daemon()).is_err());
    }

    #[test]
    fn an_empty_file_is_an_empty_document_not_a_broken_one() {
        let work = tempfile::tempdir().expect("a work folder");
        let file = work.path().join("claude_desktop_config.json");
        fs::write(&file, "\n").expect("an empty file");
        enable(&file, Path::new("/helper/digup"), &daemon())
            .expect("an empty file accepts the entry");
        assert!(fs::read_to_string(&file)
            .expect("read back")
            .contains("\"digup\""));
    }
}
