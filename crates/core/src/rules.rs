//! What gets indexed (`Rules.swift`, `Files+Portable.swift`): the extension sets, the
//! folder rules, the screenshot names, the machine-name and dataset detectors, and the
//! classification — by extension, as Linux and Windows do it (macOS has the type system,
//! the screenshot xattr and iCloud placeholders).
//!
//! The defaults come from a real developer's Mac: Spotlight types 57k TypeScript files as
//! movies, one repo holds a 55k-clip dataset, and screenshot tools move screenshots into
//! code projects. The reason strings ("code or data file", …) are reported verbatim by the
//! apps, so they are part of the contract.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::model::FileKind;
use crate::options::IndexOptions;
use crate::paths;

/// The classification of one entry (`Classification`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    Index(FileKind),
    /// A reason worth reporting ("code or data file").
    Skip(String),
    /// A type we don't handle at all.
    Ignore,
}

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// Folders never worth walking into (the enumerator skips hidden folders and bundles itself).
pub const SKIPPED_FOLDER_NAMES: [&str; 8] = [
    "node_modules",
    "bower_components",
    "venv",
    "site-packages",
    "__pycache__",
    "DerivedData",
    "Pods",
    "Carthage",
];

/// Any of these in a folder makes it a code project, where only screenshots are indexed.
pub const PROJECT_MARKERS: [&str; 14] = [
    ".git",
    "package.json",
    "Package.swift",
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "CMakeLists.txt",
];

pub const DOC_EXTENSIONS: [&str; 16] = [
    "txt", "text", "md", "markdown", "typ", "rtf", "docx", "doc", "odt", "html", "htm", "xlsx",
    "pptx", "ods", "odp", "emlx",
];

/// FairPlay-protected music: only the Music app can play it; reading it fails.
pub const PROTECTED_EXTENSIONS: [&str; 1] = ["m4p"];

/// Source code and data files: off by default.
pub const CODE_EXTENSIONS: [&str; 84] = [
    "py", "ipynb", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "m", "mm", "swift", "tsx", "cts",
    "js", "jsx", "mjs", "cjs", "go", "rs", "java", "kt", "kts", "scala", "rb", "php", "cs", "fs",
    "sh", "bash", "zsh", "fish", "ps1", "lua", "pl", "r", "jl", "dart", "sql", "vue", "svelte",
    "css", "scss", "sass", "less", "json", "jsonl", "yaml", "yml", "toml", "xml", "plist",
    "gradle", "cmake", "mk", "lock", "log", "csv", "tsv", "ini", "cfg", "conf", "proto", "graphql",
    "tf", "nix", "zig", "hs", "ex", "exs", "erl", "clj", "elm", "ml", "vim", "el", "asm", "metal",
    "glsl", "hlsl", "wgsl", "cu", "pyi", "pyc",
];

pub const SCREENSHOT_PREFIXES: [&str; 4] = ["Screenshot", "Screen Shot", "CleanShot", "SCR-"];

/// Source files code search reads.
pub const SOURCE_EXTENSIONS: [&str; 88] = [
    "py", "pyi", "pyx", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm", "swift", "ts",
    "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "go", "rs", "java", "kt", "kts", "scala",
    "groovy", "rb", "php", "cs", "fs", "sh", "bash", "zsh", "fish", "ps1", "lua", "pl", "pm", "r",
    "jl", "dart", "sql", "vue", "svelte", "astro", "html", "htm", "css", "scss", "sass", "less",
    "gradle", "cmake", "mk", "proto", "graphql", "gql", "tf", "hcl", "nix", "zig", "hs", "ex",
    "exs", "erl", "clj", "cljs", "elm", "ml", "mli", "vim", "el", "lisp", "asm", "s", "metal",
    "glsl", "hlsl", "wgsl", "cu", "cuh", "ino", "v", "sv", "tex",
];

/// What goes with code: a repo's docs and its settings.
pub const CODE_TEXT_EXTENSIONS: [&str; 10] = [
    "md", "markdown", "rst", "adoc", "org", "typ", "txt", "yaml", "yml", "toml",
];

/// Code known by its name alone.
pub const CODE_FILE_NAMES: [&str; 14] = [
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "justfile",
    "Rakefile",
    "Gemfile",
    "Podfile",
    "Brewfile",
    "Procfile",
    "Vagrantfile",
    "Jenkinsfile",
];

/// Folders code search doesn't go into, besides what git ignores.
pub const CODE_SKIPPED_FOLDER_NAMES: [&str; 23] = [
    "vendor",
    "Vendor",
    "vendored",
    "third_party",
    "third-party",
    "thirdparty",
    "3rdparty",
    "external",
    "extern",
    "deps",
    "build",
    "Build",
    "dist",
    "out",
    "target",
    "obj",
    "coverage",
    "htmlcov",
    "generated",
    "__generated__",
    "wandb",
    "lightning_logs",
    "mlruns",
];

/// Mac bundles that may sit on a shared or copied disk: skipped whole, by name.
pub const PACKAGE_EXTENSIONS: [&str; 19] = [
    "app",
    "framework",
    "bundle",
    "plugin",
    "appex",
    "xpc",
    "kext",
    "xcodeproj",
    "xcworkspace",
    "playground",
    "photoslibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
    "fcpbundle",
    "logicx",
    "band",
    "pkg",
    "dmg",
];

/// RAW pictures are images: where the build links LibRaw (#152: Linux and macOS) they decode,
/// elsewhere (Windows) they fail with `notReadYet`'s note. SVG joins the family through resvg
/// (Feature C) on every platform — it is pure Rust.
pub const IMAGE_EXTENSIONS: [&str; 26] = [
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif",
    "avif", "psd", "tga", "pnm", "ppm", "pgm", "cr2", "nef", "arw", "dng", "raf", "orf", "rw2",
    "svg",
];

pub const AUDIO_EXTENSIONS: [&str; 16] = [
    "mp3", "m4a", "m4b", "aac", "wav", "wave", "flac", "ogg", "oga", "opus", "aiff", "aif", "wma",
    "amr", "caf", "mka",
];

pub const VIDEO_EXTENSIONS: [&str; 15] = [
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "3gp", "3g2", "mpg", "mpeg", "m2v",
    "ogv", "mxf",
];

pub const CAMERA_PREFIXES: [&str; 10] = [
    "img",
    "dsc",
    "pxl",
    "mvimg",
    "vid",
    "gopr",
    "dji",
    "photo",
    "whatsapp",
    "screenshot",
];

fn in_set(names: &[&str], value: &str) -> bool {
    names.contains(&value)
}

fn has_prefix(name: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| name.starts_with(prefix))
}

/// `classify` as on macOS, by extension (`Files+Portable.swift:26-46`). `dataless` is the
/// iCloud/OneDrive placeholder check for the `.ts` rule; the crawl leaves placeholders alone.
/// Issue #8: the secret rule fires here too, not only in `classify_code` — `is_secret`'s own
/// comment reads "never read, wherever they are", and by extension a `secrets.txt` or a
/// `credentials.docx` IS a document: indexing it put the key material's text into search.
/// The reason string is the code path's, relayed verbatim by the apps. Deliberate divergence
/// from the Swift reference (its `classify` has no secret check, its `classifyCode` has —
/// `Rules.swift:80`), on user direction, disclosed in `docs/RUST_PORT_AUDIT.md`.
pub fn classify(path: &str, dataless: bool, options: &IndexOptions) -> Classification {
    let name = paths::last_component(path);
    let ext = paths::extension(name).to_lowercase();
    if options.excluded_extensions.contains(&ext) {
        return Classification::Skip("excluded type".to_string());
    }
    if is_secret(name) {
        return Classification::Skip("key or password file".to_string());
    }
    if in_set(&PROTECTED_EXTENSIONS, &ext) {
        return Classification::Skip("copy-protected".to_string());
    }
    // .ts / .mts are usually TypeScript. Only a real transport stream is video.
    if ext == "ts" || ext == "mts" || ext == "m2ts" {
        if dataless {
            return Classification::Skip("in the cloud, not downloaded".to_string());
        }
        if is_mpeg_transport_stream(path) {
            return Classification::Index(FileKind::Video);
        }
        return if options.include_code {
            Classification::Index(FileKind::Doc)
        } else {
            Classification::Skip("code or data file".to_string())
        };
    }
    if in_set(&DOC_EXTENSIONS, &ext) {
        return Classification::Index(FileKind::Doc);
    }
    if in_set(&CODE_EXTENSIONS, &ext) || in_set(&SOURCE_EXTENSIONS, &ext) {
        return if options.include_code {
            Classification::Index(FileKind::Doc)
        } else {
            Classification::Skip("code or data file".to_string())
        };
    }
    if ext == "pdf" {
        return Classification::Index(FileKind::Pdf);
    }
    if in_set(&IMAGE_EXTENSIONS, &ext) {
        return Classification::Index(if is_screenshot(path) {
            FileKind::Screenshot
        } else {
            FileKind::Image
        });
    }
    if in_set(&VIDEO_EXTENSIONS, &ext) {
        return Classification::Index(FileKind::Video);
    }
    if in_set(&AUDIO_EXTENSIONS, &ext) {
        return Classification::Index(FileKind::Audio);
    }
    Classification::Ignore
}

/// Source files code search reads, a repo's docs and notebooks. Never data, minified
/// bundles, licenses, or anything that looks like a key (`classifyCode`).
pub fn classify_code(path: &str, options: &IndexOptions) -> Classification {
    let name = paths::last_component(path);
    let ext = paths::extension(name).to_lowercase();
    if options.excluded_extensions.contains(&ext) {
        return Classification::Skip("excluded type".to_string());
    }
    if is_secret(name) {
        return Classification::Skip("key or password file".to_string());
    }
    let lower = name.to_lowercase();
    if lower.ends_with(".min.js") || lower.ends_with(".min.mjs") || lower.ends_with(".min.css") {
        return Classification::Skip("minified".to_string());
    }
    if ["license", "licence", "copying", "notice"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return Classification::Skip("license".to_string());
    }
    if (ext == "ts" || ext == "mts") && is_mpeg_transport_stream(path) {
        return Classification::Ignore; // a video
    }
    if in_set(&SOURCE_EXTENSIONS, &ext)
        || in_set(&CODE_TEXT_EXTENSIONS, &ext)
        || ext == "ipynb"
        || in_set(&CODE_FILE_NAMES, name)
        || name.starts_with("Dockerfile")
    {
        return Classification::Index(FileKind::Code);
    }
    if in_set(&CODE_EXTENSIONS, &ext) {
        return Classification::Skip("data file".to_string()); // json, csv, lockfiles, logs, plists
    }
    Classification::Ignore
}

/// Keys, certificates, password and credential files: never read, wherever they are.
pub fn is_secret(name: &str) -> bool {
    let lower = name.to_lowercase();
    let secret_types: &[&str] = &[
        "pem",
        "key",
        "p12",
        "pfx",
        "keystore",
        "jks",
        "ppk",
        "kdbx",
        "gpg",
        "asc",
        "crt",
        "cer",
        "der",
        "p8",
        "mobileprovision",
    ];
    if in_set(secret_types, paths::extension(&lower)) {
        return true;
    }
    has_prefix(
        &lower,
        &[
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            ".env",
            "secret",
            "credential",
            ".netrc",
            ".npmrc",
            ".pypirc",
            "htpasswd",
            ".htpasswd",
        ],
    )
}

/// Why code search doesn't go into a folder, or None (`codeSkipReason`).
pub fn code_skip_reason(path: &str) -> Option<String> {
    if in_set(&CODE_SKIPPED_FOLDER_NAMES, paths::last_component(path)) {
        return Some("vendored or built code".to_string());
    }
    // A submodule or a worktree (".git" is a file there): someone else's code, or a copy.
    let git = Path::new(path).join(".git");
    if let Ok(meta) = std::fs::metadata(&git) {
        if !meta.is_dir() {
            return Some("submodule or worktree".to_string());
        }
    }
    None
}

/// MPEG-TS packets start with 0x47 every 188 bytes (or every 192 with a 4-byte timecode, as
/// in .m2ts / AVCHD .mts).
pub fn is_mpeg_transport_stream(path: &str) -> bool {
    let Ok(data) = read_head(path, 600) else {
        return false;
    };
    if data.len() < 400 {
        return false;
    }
    (data[0] == 0x47 && data[188] == 0x47 && data[376] == 0x47)
        || (data[4] == 0x47 && data[196] == 0x47 && data[388] == 0x47)
}

fn read_head(path: &str, count: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; count];
    let mut filled = 0;
    while filled < count {
        let read = file.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    buffer.truncate(filled);
    Ok(buffer)
}

/// Only macOS marks screenshots with an xattr; here names are all there is
/// (`isScreenshot`: the prefixes cover Snipping Tool, Win+PrtScn, GNOME and KDE; a
/// picture inside a "Screenshots" folder is a screenshot whatever it's called).
pub fn is_screenshot(path: &str) -> bool {
    let name = paths::last_component(path);
    if has_prefix(name, &SCREENSHOT_PREFIXES) {
        return true;
    }
    paths::last_component(&paths::parent(path)).eq_ignore_ascii_case("Screenshots")
}

pub fn is_package(name: &str) -> bool {
    in_set(&PACKAGE_EXTENSIONS, &paths::extension(name).to_lowercase())
}

/// Why the crawl skips a folder (`skipReason`), or None.
pub fn skip_reason(path: &str, home: &str, excluded: &[String]) -> Option<String> {
    let name = paths::last_component(path);
    if in_set(&SKIPPED_FOLDER_NAMES, name) {
        return Some("developer folder".to_string());
    }
    if name.ends_with(".noindex") {
        return Some(".noindex folder".to_string());
    }
    if is_cache_directory(path) {
        return Some("cache folder".to_string());
    }
    if path == format!("{}/Library", home) {
        return Some("~/Library".to_string());
    }
    if excluded
        .iter()
        .any(|folder| path == folder || path.starts_with(&format!("{}/", folder)))
    {
        return Some("excluded folder".to_string());
    }
    None
}

/// The Cache Directory Tagging specification requires this exact 43-byte header, not just
/// the filename.
pub fn is_cache_directory(path: &str) -> bool {
    let tag = Path::new(path).join("CACHEDIR.TAG");
    let Some(info) = crate::crawler::file_info(&tag.to_string_lossy()) else {
        return false;
    };
    if !info.is_regular || info.dataless {
        return false;
    }
    let signature = b"Signature: 8a477f597d28d172789f06886806bc55";
    read_head(&tag.to_string_lossy(), signature.len())
        .ok()
        .as_deref()
        == Some(signature)
}

/// A folder with a project marker in it (the listing includes hidden names: `.git`).
pub fn is_project_root(path: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    let names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.iter().any(|name| in_set(&PROJECT_MARKERS, name))
        || names.iter().any(|name| {
            // CASE-SENSITIVE, like `Rules.isProjectRoot` (Rules.swift:179): the set is the
            // lowercase markers and `pathExtension` is compared RAW — a folder spelled
            // `Foo.XcodeProj` (Xcode's own spelling on a zip-restored or Windows-authored
            // tree) is NOT a project root to the engine. `classify`/`isSecret` DO lowercase
            // their extension lookups (Rules.swift:78, :100) — they lower where Swift
            // lowers, this site does not. Foundation's leading-dot rule (`".app"` has no
            // extension, `"..app"` has "app") rides `paths::extension` — its
            // `Path::extension` rule is equal there (measured against the Foundation probe
            // 2026-11-14: `.app`→"", `..app`→"app", `.hidden`→"", `foo.`→"").
            in_set(&["xcodeproj", "xcworkspace"], paths::extension(name))
        })
}

/// Machine-made names like "clip_000123", "frame-0042" or "3f9a1c0e-…", but not camera
/// names like "IMG_1234" or the date and time phones write. The two engine regexes
/// (`looksMachineNamed`), hand-matched:
/// `^[0-9a-f]{8,}([-_][0-9a-f]{4,})*$` and `^[a-z]{0,16}[-_. ]?\d{3,}([-_.]\d+)*$`.
pub fn looks_machine_named(stem: &str) -> bool {
    let name = stem.to_lowercase();
    if has_prefix(&name, &CAMERA_PREFIXES) || starts_with_date(&name) {
        return false;
    }
    if matches_hex_uuid(&name) {
        return true;
    }
    matches_numbered(&name)
}

fn is_hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

/// `^[0-9a-f]{8,}([-_][0-9a-f]{4,})*$`
fn matches_hex_uuid(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    // A run of hex characters starting at `from`, greedy as a regex `{n,}` quantifier is.
    let hex_run = |from: usize| -> usize {
        let mut count = 0;
        while from + count < chars.len() && is_hex(chars[from + count]) {
            count += 1;
        }
        count
    };
    let mut index = 0;
    let head = hex_run(index);
    if head < 8 {
        return false;
    }
    index += head;
    while index < chars.len() {
        if chars[index] != '-' && chars[index] != '_' {
            return false;
        }
        index += 1;
        let group = hex_run(index);
        if group < 4 {
            return false;
        }
        index += group;
    }
    true
}

/// `^[a-z]{0,16}[-_. ]?\d{3,}([-_.]\d+)*$`
fn matches_numbered(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    let mut index = 0;
    let mut letters = 0;
    while index < chars.len() && chars[index].is_ascii_lowercase() {
        letters += 1;
        index += 1;
    }
    if letters > 16 {
        return false;
    }
    if index < chars.len()
        && (chars[index] == '-'
            || chars[index] == '_'
            || chars[index] == '.'
            || chars[index] == ' ')
    {
        index += 1;
    }
    let digits_start = index;
    while index < chars.len() && chars[index].is_ascii_digit() {
        index += 1;
    }
    if index - digits_start < 3 {
        return false;
    }
    while index < chars.len() {
        if chars[index] != '-' && chars[index] != '_' && chars[index] != '.' {
            return false;
        }
        index += 1;
        let group_start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        if index == group_start {
            return false;
        }
    }
    true
}

/// A name that starts with a date (year, month and day, with or without separators):
/// `^(19|20)\d{2}[-_.]?(0[1-9]|1[0-2])[-_.]?(0[1-9]|[12]\d|3[01])([-_. (]|$)`.
fn starts_with_date(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() < 8 {
        return false;
    }
    let digit = |c: char| c.is_ascii_digit();
    if !(chars[0] == '1' && chars[1] == '9') && !(chars[0] == '2' && chars[1] == '0') {
        return false;
    }
    if !digit(chars[2]) || !digit(chars[3]) {
        return false;
    }
    let mut index = 4;
    if index < chars.len() && (chars[index] == '-' || chars[index] == '_' || chars[index] == '.') {
        index += 1;
    }
    // month 01..12
    if index + 1 >= chars.len() || !digit(chars[index]) || !digit(chars[index + 1]) {
        return false;
    }
    let month: u32 =
        chars[index].to_digit(10).unwrap() * 10 + chars[index + 1].to_digit(10).unwrap();
    if !(1..=12).contains(&month) {
        return false;
    }
    index += 2;
    if index < chars.len() && (chars[index] == '-' || chars[index] == '_' || chars[index] == '.') {
        index += 1;
    }
    // day 01..31
    if index + 1 >= chars.len() || !digit(chars[index]) || !digit(chars[index + 1]) {
        return false;
    }
    let day: u32 = chars[index].to_digit(10).unwrap() * 10 + chars[index + 1].to_digit(10).unwrap();
    if !(1..=31).contains(&day) {
        return false;
    }
    index += 2;
    // the date must end at a separator boundary: [-_. (] or end of name
    index == chars.len()
        || chars[index] == '-'
        || chars[index] == '_'
        || chars[index] == '.'
        || chars[index] == ' '
        || chars[index] == '('
}

/// Folders with hundreds of same-kind, machine-named media files: training data, not your
/// stuff (`datasetFolders`). Roots you pick yourself are never datasets.
///
/// The report names WHICH KINDS triggered each folder (issue #1): the crawl drops the media
/// of the triggering kinds only — a video parked beside 300 numbered images is not that
/// dataset, and its row lives. The wire shape (`datasets`: folder → count) is unchanged.
#[derive(Debug, Default)]
pub struct Datasets {
    /// folder → files in its triggering groups
    pub counts: HashMap<String, i64>,
    /// folder → the kinds whose groups crossed the line
    pub kinds: HashMap<String, HashSet<FileKind>>,
}

pub fn dataset_folders(
    files: &[(String, String, FileKind)],
    roots: &HashSet<String>,
    minimum: usize,
) -> Datasets {
    let mut groups: HashMap<(String, FileKind), Vec<String>> = HashMap::new();
    for (folder, stem, kind) in files {
        if matches!(kind, FileKind::Image | FileKind::Audio | FileKind::Video)
            && !roots.contains(folder)
        {
            groups
                .entry((folder.clone(), *kind))
                .or_default()
                .push(stem.clone());
        }
    }
    let mut datasets = Datasets::default();
    for ((folder, kind), stems) in groups {
        if stems.len() < minimum {
            continue;
        }
        let machine = stems
            .iter()
            .filter(|stem| looks_machine_named(stem))
            .count();
        if machine as f64 / stems.len() as f64 >= 0.7 {
            *datasets.counts.entry(folder.clone()).or_default() += stems.len() as i64;
            datasets.kinds.entry(folder).or_default().insert(kind);
        }
    }
    datasets
}

/// `IndexOptions.extensions` needs a HashSet; `set` keeps the const tables compact.
pub fn extension_set(extensions: &[&str]) -> HashSet<String> {
    set(extensions)
}
