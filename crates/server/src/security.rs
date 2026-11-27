//! The security surface (`security.py`): explicit Host/Origin validation,
//! bearer scopes with a named-token map, and a CORS allowlist — all opt-in,
//! off by default (README §7, stage-12). The default deployment is
//! loopback-only and unguarded: the guard engages only when the operator
//! names the hosts this deployment serves.
//!
//! The middleware stack (stage-12's order, outermost first):
//!   PrincipalLog → Cors → HostOriginGuard → BearerAuth → routes
//! so the audit line sees every guarded request and its accepted principal;
//! CORS answers the widget preflights and marks allowlisted origins for the
//! guard; the guard refuses a wrong host before authentication; the bearer
//! gate compares tokens. All knobs are inert when unset.
//!
//! The minted-key rules (stage-13): the server issues what env left unbound,
//! persists 0600 in `<config>/tokens.json` (restart-stable — recreation never
//! silently revokes a connected application), and `/api/v1/tokens` shows only
//! what the server generated; environment-configured secrets are never copied
//! into the file and never echoed.

use std::path::Path;
use std::sync::{RwLock, RwLockReadGuard};

use serde_json::{json, Value};

pub const TOKENS_FILE: &str = "tokens.json";

/// The deployment's tokens. `read` guards every `/api/v1` endpoint; `admin`
/// guards the mutating/heavy ones on top of it. `readers` is the named map
/// (insertion order — `principal` takes the FIRST match). `minted` names the
/// entries the server issued itself — the ones `/api/v1/tokens` may show.
#[derive(Clone, Default)]
pub struct Auth {
    pub read: Option<String>,
    pub admin: Option<String>,
    pub readers: Vec<(String, String)>,
    pub minted: Vec<String>,
}

impl Auth {
    pub fn enabled(&self) -> bool {
        self.read.is_some() || self.admin.is_some() || !self.readers.is_empty()
    }

    /// `token_names`: every scope this exact token serves — () when it is
    /// nobody's. `admin` covers the read scope too (an admin client searches
    /// like a read client), and its match short-circuits the rest.
    /// Constant-time comparison throughout: these are secrets.
    pub fn token_names(&self, supplied: &str) -> Vec<String> {
        if let Some(admin) = &self.admin {
            if constant_time_eq(supplied.as_bytes(), admin.as_bytes()) {
                return vec!["admin".to_string()];
            }
        }
        let mut names = Vec::new();
        if let Some(read) = &self.read {
            if constant_time_eq(supplied.as_bytes(), read.as_bytes()) {
                names.push("read".to_string());
            }
        }
        for (name, token) in &self.readers {
            if constant_time_eq(supplied.as_bytes(), token.as_bytes()) {
                names.push(name.clone());
            }
        }
        names
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `hmac.compare_digest` for the endpoints that compare the bearer against
/// the admin key themselves (`_admin_scope`).
pub fn tokens_match(a: &str, b: &str) -> bool {
    constant_time_eq(a.as_bytes(), b.as_bytes())
}

/// `auth_from_env`: `DIGUP_SERVER_TOKEN` (read), `DIGUP_SERVER_ADMIN_TOKEN`
/// (admin), `DIGUP_SERVER_READ_TOKENS` (the named map).
pub fn auth_from_env() -> Auth {
    Auth {
        read: std::env::var("DIGUP_SERVER_TOKEN")
            .ok()
            .filter(|v| !v.is_empty()),
        admin: std::env::var("DIGUP_SERVER_ADMIN_TOKEN")
            .ok()
            .filter(|v| !v.is_empty()),
        readers: read_tokens_from_env(),
        minted: Vec::new(),
    }
}

/// `read_tokens_from_env`: `nc-a:tokA,app:nc:tokB` → named pairs. The
/// separator is the LAST colon (names may contain colons — the target-bound
/// form; tokens must not). An entry without a colon is a typo, skipped with a
/// warning naming it — a silently-dropped token is an outage nobody asked
/// for. Names keep the FIRST entry of a duplicate; names are lowercased.
pub fn read_tokens_from_env() -> Vec<(String, String)> {
    let raw = std::env::var("DIGUP_SERVER_READ_TOKENS").unwrap_or_default();
    let mut readers: Vec<(String, String)> = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, token) = match entry.rfind(':') {
            Some(at) if !entry[..at].trim().is_empty() && !entry[at + 1..].trim().is_empty() => (
                entry[..at].trim().to_lowercase(),
                entry[at + 1..].trim().to_string(),
            ),
            _ => {
                eprintln!("DIGUP_SERVER_READ_TOKENS: entry '{entry}' is not a 'name:token' pair — skipped");
                continue;
            }
        };
        if !readers.iter().any(|(n, _)| *n == name) {
            readers.push((name, token));
        }
    }
    readers
}

/// `env_secrets`: True when the DEPLOYMENT typed any key in its environment.
/// Minted keys (server-generated) are not env secrets: a minted deployment
/// guards the index operations and each application's own index, answers the
/// main index's READS openly, and shows its minted keys to the gear.
pub fn env_secrets(auth: &Auth) -> bool {
    if auth.read.is_some() {
        return true;
    }
    if auth.admin.is_some() && !auth.minted.iter().any(|m| m == "admin") {
        return true;
    }
    auth.readers
        .iter()
        .any(|(name, _)| !auth.minted.iter().any(|m| m == name))
}

/// `mint_tokens`: the deployment's Auth with minted keys for the app targets
/// env left unbound. Restart-stable: values come from `<config>/tokens.json`
/// and are re-used. Standalone with `DIGUP_SERVER_MINT_KEYS`, the box mints
/// its own API key at boot; without targets and without the knob, minting
/// stays inert — a plain tokenless server is unchanged.
pub fn mint_tokens(
    config_dir: &Path,
    app_target_names: &[String],
    mint_admin: bool,
) -> Result<Auth, String> {
    let mut auth = auth_from_env();
    if app_target_names.is_empty() && !mint_admin {
        return Ok(auth); // standalone: minting inert
    }
    let path = config_dir.join(TOKENS_FILE);
    let stored = read_tokens_file(&path)?;
    let mut minted: Vec<String> = Vec::new();

    if auth.admin.is_none() {
        // A key in the file IS a minted key (server-generated), fresh-minted
        // or from a previous boot — displayable either way.
        auth.admin = match stored.get("admin") {
            Some(Value::String(token)) => Some(token.clone()),
            _ => Some(urlsafe_token()?),
        };
        minted.push("admin".to_string());
    }
    for name in app_target_names {
        let key = format!("app:{name}");
        if auth.readers.iter().any(|(n, _)| *n == key) {
            continue; // env already bound this target
        }
        let token = match stored
            .get("readers")
            .and_then(|readers| readers.get(&key))
            .and_then(Value::as_str)
            .map(|token| token.to_string())
        {
            Some(token) => token,
            None => urlsafe_token()?,
        };
        auth.readers.push((key.clone(), token));
        minted.push(key);
    }
    auth.minted = minted;
    write_tokens_file(&path, &auth)?;
    Ok(auth)
}

/// `secrets.token_urlsafe(24)`: 24 random bytes as base64url, no padding
/// (32 chars). The randomness is REQUIRED, exactly as `secrets` has it:
/// `os.urandom` failing raises OSError with no fallback — at boot that
/// kills the lifespan (the mint runs inside `create_app`), on the rotate
/// and register paths it is an uncaught 500. The audit-base answered
/// `Ok` from a fallback that filled bytes[..8] with a timestamp^pid seed
/// and left 16 ZERO bytes: base64url renders zero bytes as the alphabet's
/// zero char, a literal `A`, so a fallback-minted key LED with a run of
/// 21 `A` chars and carried 8 bits of entropy — a key shape `secrets`
/// cannot produce, minted at the exact failure `secrets` refuses. The
/// server ships in a Linux container (AGENTS.md: /dev/urandom is part of
/// the kernel's ABI; report refusals, never paper over them): failure
/// propagates, naming the fact.
pub fn urlsafe_token() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|error| format!("no system randomness to mint a key: {error}"))?;
    Ok(base64url(&bytes))
}

fn base64url(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut buf = [0u8; 3];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = (u32::from(buf[0]) << 16) | (u32::from(buf[1]) << 8) | u32::from(buf[2]);
        let count = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for i in 0..count {
            out.push(CHARS[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
        }
    }
    out
}

pub fn read_tokens_file(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|exc| {
            format!(
                "{}: not valid JSON ({exc}) — fix it or delete it; the server will not mint over a corrupt keys file",
                path.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(serde_json::Map::new())),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// `_write_tokens_file`: persist ONLY minted values, atomically, 0600 (the
/// keys file holds secrets). The config folder is the server's own to create.
pub fn write_tokens_file(path: &Path, auth: &Auth) -> Result<(), String> {
    let mut payload = serde_json::Map::new();
    if auth.minted.iter().any(|m| m == "admin") {
        if let Some(admin) = &auth.admin {
            payload.insert("admin".into(), json!(admin));
        }
    }
    let readers: serde_json::Map<String, Value> = auth
        .minted
        .iter()
        .filter(|name| name.starts_with("app:"))
        .filter_map(|name| {
            auth.readers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, token)| (name.clone(), json!(token)))
        })
        .collect();
    if !readers.is_empty() {
        payload.insert("readers".into(), Value::Object(readers));
    }
    let existing = read_tokens_file(path)?;
    let existing_map = existing.as_object().cloned().unwrap_or_default();
    let mut merged = serde_json::Map::new();
    for key in ["admin", "readers"] {
        if let Some(value) = existing_map.get(key) {
            merged.insert(key.into(), value.clone());
        }
    }
    for (key, value) in payload.iter() {
        merged.insert(key.clone(), value.clone());
    }
    // readers is a UNION (a restart with fewer targets must not drop keys
    // other applications still hold):
    let mut union = existing_map
        .get("readers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(payload_readers) = payload.get("readers").and_then(Value::as_object) {
        for (key, value) in payload_readers {
            union.insert(key.clone(), value.clone());
        }
    }
    if !union.is_empty() {
        merged.insert("readers".into(), Value::Object(union));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = format!("{}.new", path.display());
    write_private(
        Path::new(&tmp),
        serde_json::to_string(&Value::Object(merged))
            .expect("json")
            .as_bytes(),
    )?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    set_private(path);
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    use std::io::Write;
    file.write_all(bytes)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(unix)]
fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_private(_path: &Path) {}

/// `rotate_minted`: regenerate the server-issued keys. Environment-configured
/// keys are NEVER rotated (they are the deployment's own config; they keep
/// working; the response flags them configured-only). Returns None when
/// nothing is minted.
pub fn rotate_minted(auth: &Auth, config_dir: &Path) -> Result<Option<Auth>, String> {
    if auth.minted.is_empty() {
        return Ok(None);
    }
    let mut rotated = auth.clone();
    let mut minted: Vec<String> = Vec::new();
    if auth.minted.iter().any(|m| m == "admin") {
        rotated.admin = Some(urlsafe_token()?);
        minted.push("admin".to_string());
    }
    for name in &auth.minted {
        if name.starts_with("app:") {
            if let Some((_, token)) = rotated.readers.iter_mut().find(|(n, _)| n == name) {
                *token = urlsafe_token()?;
                minted.push(name.clone());
            }
        }
    }
    rotated.minted = minted;
    write_tokens_file(&config_dir.join(TOKENS_FILE), &rotated)?;
    Ok(Some(rotated))
}

// ── the env knobs of the guard/CORS layers ──────────────────────────────────

/// `allowed_hosts_from_env`: the normalized set, or None (= guard off).
pub fn allowed_hosts_from_env() -> Option<Vec<String>> {
    let raw = std::env::var("DIGUP_SERVER_ALLOWED_HOSTS").unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split(',')
            .filter(|host| !host.trim().is_empty())
            .map(|host| host.trim().split(':').next().unwrap_or("").to_lowercase())
            .collect(),
    )
}

/// `allowed_origins_from_env`: the exact set, or None (= CORS off).
pub fn allowed_origins_from_env() -> Option<Vec<String>> {
    let raw = std::env::var("DIGUP_SERVER_ALLOWED_ORIGINS").unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.split(',')
            .filter(|origin| !origin.trim().is_empty())
            .map(|origin| {
                origin
                    .trim()
                    .to_lowercase()
                    .trim_end_matches('/')
                    .to_string()
            })
            .collect(),
    )
}

/// `_origin_host`: `http://127.0.0.1:8787` → `127.0.0.1` (no port).
pub fn origin_host(origin: &str) -> String {
    let tail = origin.split("://").last().unwrap_or(origin);
    tail.split(':')
        .next()
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// `OPEN_PATHS`: not token-guarded by design — the container's HEALTHCHECK
/// calls /health with no credential (a guarded /health would mark a live
/// container unhealthy), and the OpenAPI page is documentation, not data.
pub const OPEN_PATHS: [&str; 3] = ["/api/v1/health", "/api/v1/docs", "/api/v1/openapi.json"];

/// `ADMIN_ROUTES`: POST endpoints that change the index or crawl the whole tree.
/// Faces (FACES.md Phase 3): consent writes, scans (they crawl the tree) and the
/// person mutations are the deployment's mutating acts; the faces READS ride the
/// ordinary bearer gate.
pub const ADMIN_ROUTES: [(&str, &str); 10] = [
    ("POST", "/api/v1/index"),
    ("POST", "/api/v1/estimate"),
    ("POST", "/api/v1/faces/scan"),
    ("POST", "/api/v1/faces/consent"),
    ("DELETE", "/api/v1/faces/consent"),
    ("POST", "/api/v1/faces/name"),
    ("POST", "/api/v1/faces/merge"),
    ("POST", "/api/v1/faces/delete"),
    ("POST", "/api/v1/faces/unload"),
    ("POST", "/api/v1/faces/confirm"),
];

/// The live Auth: rotation swaps the value in place — the gate and the
/// endpoints read the SAME live object (Python's `current = {"auth": ...}`).
pub type AuthState = RwLock<Auth>;

pub fn read_auth(state: &AuthState) -> RwLockReadGuard<'_, Auth> {
    state.read().expect("the auth state")
}

/// `_bearer`: the Authorization header's bearer token, if any.
pub fn bearer_of(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let text = value.trim();
    let (scheme, token) = match text.split_once(' ') {
        Some((scheme, token)) => (scheme, token),
        None => (text, ""),
    };
    if scheme.to_lowercase() == "bearer" && !token.trim().is_empty() {
        Some(token.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::urlsafe_token;

    #[test]
    fn minted_tokens_are_thirty_two_base64url_chars_of_full_entropy() {
        // `secrets.token_urlsafe(24)`: 24 bytes -> 32 base64url chars
        // (alphabet A-Za-z0-9-_, no padding). With the fallback gone
        // (F-11) every minted byte is an os-level random byte: no leading
        // run of the alphabet's zero char `A` (16 zero bytes print 21 of
        // them — the fallback's signature; 24 random bytes reach one by
        // 2^-126 luck), and two mints differ (a rotate must show new
        // keys, and `tokens.json` keys must not repeat).
        let token = urlsafe_token().expect("/dev/urandom is the kernel's ABI here");
        assert_eq!(token.len(), 32, "{token}");
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "{token}"
        );
        assert!(!token.starts_with("AAAAAAAAAAAA"), "{token}");
        assert_ne!(token, urlsafe_token().expect("urandom"));
    }

    #[test]
    fn the_audit_base_fallback_minted_a_zero_filled_key() {
        // F-11's measurement, recorded at the fixed tip: the audit-base
        // function VERBATIM, with the urandom open pointed at a file that
        // cannot open — the exact failure the fallback was written for.
        // It answers Ok, and the key is 24 bytes of which 16 are ZERO:
        // base64url renders every leading zero byte as `A`, so the minted
        // key leads with a run of literal `A` chars and carries 8 bits of
        // entropy (timestamp^pid). `secrets` cannot mint this shape; it
        // raises. The fixed urlsafe_token has no fallback: it answers
        // Err("no system randomness to mint a key: ...") right here.
        use std::io::Read;
        let mut bytes = [0u8; 24];
        let outcome = std::fs::File::open("/proc/digup-no-such-urandom")
            .and_then(|mut f| f.read_exact(&mut bytes));
        assert!(outcome.is_err(), "the probe needs the open to fail");
        match outcome {
            Ok(()) => {}
            Err(_) => {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0)
                    ^ (std::process::id() as u64) << 32;
                bytes[..8].copy_from_slice(&seed.to_le_bytes());
            }
        }
        // The audit-base's own base64url, inline (the module's CHARS).
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            out.push(CHARS[(b0 >> 2) as usize] as char);
            out.push(CHARS[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(CHARS[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(CHARS[(b2 & 0x3f) as usize] as char);
            }
        }
        // The seed fills bytes[..8], the zeros TRAIL: the minted key ENDS
        // with the alphabet-zero run — 16 zero bytes cover bits 64..191,
        // chars 11..31 (21 of them) render as literal `A`, above a
        // timestamp^pid head. `secrets` mints no such shape.
        let trailing_a = out.chars().rev().take_while(|c| *c == 'A').count();
        assert_eq!(out.len(), 32, "{out}");
        assert!(
            trailing_a >= 20,
            "the fallback key {out} ends {trailing_a} literal A chars"
        );
    }
}
