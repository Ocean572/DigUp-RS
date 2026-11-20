//! The `digup` binary — Rust port. Phase 2+ implements the one-shot CLI verb by verb; the
//! services (JSON-lines) land in Phase 3+.
//!
//! Implemented here: `speed` and `speed --model` (`main.swift:474-613`), which is how a Linux
//! machine learns where its model should run — the GPU measured against the processor, the
//! winner kept as `device` in `costs.json` beside the index. Everything else is the Phase-1
//! self-test: opening a v6 index and reporting it, so the workspace wiring stays visible.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use digup_services::costs::{MeasuredCosts, Speed};
use digup_services::emit;
use digup_services::speed as modelspeed;
use digup_services::system_load;
use unicode_segmentation::UnicodeSegmentation;

/// `usage` (main.swift:11-53): the CLI's own words, byte-for-byte (the Swift literal's
/// 4-space strip is baked into the file; Swift prints it verbatim).
const USAGE: &str = include_str!("usage.txt");

/// `Arguments` (main.swift:59-95): known valued flags take exactly one value, repeated
/// ones collect a list, `--*` are bare flags, everything else is positional.
struct Arguments {
    flags: Vec<String>,
    values: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
    positional: Vec<String>,
}

/// `Arguments.valued`: flags that need a value.
const VALUED: [&str; 16] = [
    "--index",
    "--limit",
    "--kind",
    "-n",
    "--zgate",
    "--word-weight",
    "--new-limit",
    "--linger",
    "--shot-tokens",
    "--code-stretch",
    "--encoder-idle",
    "--to",
    "--memory-limit",
    "--time-limit",
    "--tries",
    "--when-quiet",
];

/// `Arguments.repeated`: flags that collect a list.
const REPEATED: [&str; 4] = [
    "--exclude",
    "--exclude-ext",
    "--code-root",
    "--code-exclude",
];

fn main() {
    // The engine's Estimator counts PDF pages with PDFium (`PDF+Portable.swift:136`); install
    // the same library's count before any verb reads a folder.
    digup_native::pdf::install_pages_hook();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // main.swift:726 — no args, `-h`, `--help`, `help`: the usage text, exit 0. An
    // unknown command is the same text with exit 2 (the switch's default).
    match argv.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            return;
        }
        _ => {}
    }
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let command = args.positional.first().map(String::as_str);
    // An app's helper goes when the app does (main.swift:717): the JSON-lines daemons and
    // the `--json` estimate watch their parent (ParentWatch); a manual run has a parent
    // that outlives them.
    if matches!(
        command,
        Some("index-service" | "worker" | "search-service" | "download-model")
    ) || (command == Some("estimate") && args.flags.iter().any(|f| f == "--json"))
    {
        digup_services::parentwatch::start();
    }
    // Where this machine's speed check found its processor faster than its GPU, the model
    // runs on the processor (main.swift:725).
    if command != Some("speed") {
        let folder = args.index_folder();
        if digup_services::costs::MeasuredCosts::load(&folder)
            .speed
            .as_ref()
            .and_then(|speed| speed.device.as_deref())
            == Some("cpu")
        {
            digup_runtime::set_cpu_only(true);
        }
    }
    // Queries are a few words: the processor encodes one in hundredths of a second, and
    // starts sooner than a GPU (an RTX 4050 laptop: a search from scratch 1.8 s on its i7,
    // 3.0 s on the GPU under Vulkan). Indexing keeps the GPU (main.swift:728, non-macOS).
    if command == Some("search-service") {
        digup_runtime::set_cpu_only(true);
    }
    match command {
        Some("status") => {
            // `status(...)` (main.swift:398): the engine's own words — the server
            // parses these lines, so the shape is the contract.
            if let Err(e) = status(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("speed") => {
            if let Err(e) = speed(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("estimate") => {
            if let Err(e) = estimate(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("index") => {
            if let Err(e) = index(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("search") => {
            if let Err(e) = search(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("eval") => {
            if let Err(e) = evaluate(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Some("worker") => {
            let text_only = args.flags.iter().any(|flag| flag == "--text-only");
            std::process::exit(digup_services::worker::worker_service(text_only));
        }
        Some("search-service") => {
            // searchService(_:) (SearchService.swift:379): the folders as the
            // daemon sees them, the idle seconds as the shell asked (`--encoder-idle`).
            let config = digup_services::search_service::SearchConfig {
                index: args.index_folder(),
                roots: args.positional[1..]
                    .iter()
                    .map(|p| digup_services::search_service::folder_url(p))
                    .collect(),
                skipped: args
                    .list("--exclude")
                    .iter()
                    .map(|p| digup_services::search_service::folder_url(p))
                    .collect(),
                types: digup_core::options::IndexOptions::extensions(
                    &args.list("--exclude-ext").join(","),
                ),
                code_roots: args
                    .list("--code-root")
                    .iter()
                    .map(|p| digup_services::search_service::folder_url(p))
                    .collect(),
                code_skipped: args
                    .list("--code-exclude")
                    .iter()
                    .map(|p| digup_services::search_service::folder_url(p))
                    .collect(),
                idle_seconds: args
                    .value("--encoder-idle")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(300.0),
            };
            std::process::exit(digup_services::search_service::serve(config));
        }
        Some("download-model") => {
            // downloadModel(_:) (DownloadModel.swift): the events are the contract;
            // it exits 0 on `done`, 1 on `error`.
            std::process::exit(download_model(&args));
        }
        Some("index-service") => {
            // indexService(_:) (main.swift:421): the media options and the code
            // index's overrides, the watchdog and the crash notes beside the index,
            // the decode budget the same limit (issue #182).
            let index = args.index_folder();
            let mut code_options = args.options();
            code_options.code = true;
            code_options.excluded_folders = args
                .list("--code-exclude")
                .iter()
                .map(|p| folder_url(p).to_string_lossy().into_owned())
                .collect();
            let config = digup_services::index_service::IndexServiceConfig {
                index: index.clone(),
                roots: args.positional[1..].iter().map(|p| folder_url(p)).collect(),
                code_roots: args
                    .list("--code-root")
                    .iter()
                    .map(|p| folder_url(p))
                    .collect(),
                options: args.options(),
                code_options,
                prune: args.flag("--prune"),
                backfill: args.flag("--backfill"),
                new_files_limit: args
                    .value("--new-limit")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(50),
                linger: args
                    .value("--linger")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60.0),
            };
            // `&*` and not `<<`: a --memory-limit above 2^44 MB would overflow-shift
            // and trap in the helper's own setup (#182 review N1); saturating gives a
            // limit no file reaches, which is what such an argument means.
            let memory_limit = args
                .value("--memory-limit")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|megabytes| megabytes.wrapping_mul(1 << 20))
                .unwrap_or_else(digup_services::watchdog::default_memory_limit);
            let time_limit = args
                .value("--time-limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(digup_services::watchdog::DEFAULT_TIME_LIMIT);
            digup_services::watchdog::Watchdog::new(&index, memory_limit, time_limit).start();
            digup_native::image::set_decode_memory_budget(memory_limit);
            digup_services::crashnotes::install(&index);
            std::process::exit(digup_services::index_service::index_service(config));
        }
        _ => {
            // main.swift's switch default: the usage text, exit 2.
            println!("{USAGE}");
            std::process::exit(2);
        }
    }
}

mod formats;

fn parse(argv: &[String]) -> Result<Arguments, String> {
    let mut flags = Vec::new();
    let mut values = BTreeMap::new();
    let mut lists: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let argument = &argv[index];
        if VALUED.contains(&argument.as_str()) || REPEATED.contains(&argument.as_str()) {
            let Some(value) = argv.get(index + 1) else {
                return Err(format!("{argument} needs a value"));
            };
            if REPEATED.contains(&argument.as_str()) {
                lists
                    .entry(argument.clone())
                    .or_default()
                    .push(value.clone());
            } else {
                values.insert(argument.clone(), value.clone());
            }
            index += 2;
        } else if argument.starts_with("--") || argument == "-h" {
            flags.push(argument.clone());
            index += 1;
        } else {
            positional.push(argument.clone());
            index += 1;
        }
    }
    Ok(Arguments {
        flags,
        values,
        lists,
        positional,
    })
}

impl Arguments {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn list(&self, name: &str) -> Vec<String> {
        self.lists.get(name).cloned().unwrap_or_default()
    }

    /// `Arguments.options` (main.swift:104-114): what the crawl and the estimate run under.
    fn options(&self) -> digup_core::options::IndexOptions {
        let mut options = digup_core::options::IndexOptions::default();
        options.code = self.flag("--code");
        options.include_code = self.flag("--include-code");
        options.include_datasets = self.flag("--include-datasets");
        options.excluded_folders = self
            .list("--exclude")
            .iter()
            .map(|folder| folder_url(folder).to_string_lossy().into_owned())
            .collect();
        options.excluded_extensions =
            digup_core::options::IndexOptions::extensions(&self.list("--exclude-ext").join(","));
        options
    }

    /// `Arguments.store()` (main.swift:101-103): which SQLite file this verb reads and writes.
    /// `--code` names the code index, so `index`/`status` work on `code.sqlite` and its own
    /// fingerprint; the main store's media fingerprint (`photo140 shot280 …`) has nothing to
    /// say about a code run. Measured 2026-11-13 by `scripts/rust/twin-gate.sh`: on a folder
    /// holding a media index, `index --code` answers exit 0 with its rows in `code.sqlite` —
    /// gating a code run by the main store's settings is the port refusing its own index.
    fn store_file(&self) -> &'static str {
        if self.flag("--code") {
            digup_core::CODE_FILE
        } else {
            digup_core::MAIN_FILE
        }
    }

    /// `Arguments.indexFolder`: `--index` (with `~`), else the platform's default
    /// (`AppPaths.defaultIndex`: `$XDG_DATA_HOME/digup/index`, or `~/.local/share/digup/index`).
    fn index_folder(&self) -> PathBuf {
        // `Arguments.indexFolder` (main.swift:94-95): `--index` is
        // `URL(fileURLWithPath: expandingTildeInPath).standardizedFileURL` and
        // the default is `AppPaths.defaultIndex.standardizedFileURL` — the
        // standardize is part of the relayed fact: status's `index`, the
        // folder the WAL sidecars and the server's writer lock ride, the
        // strings in every reply. `/data/idx/../idx/`, `/data//idx` and
        // `/data/idx/.` all answer `/data/idx` in the engine; the audit-base
        // relayed the caller's string verbatim. The roots ride the same
        // shape (`folder_url`, main.swift:127-128).
        if let Some(chosen) = self.value("--index") {
            return url_path(chosen);
        }
        let base = match std::env::var("XDG_DATA_HOME") {
            Ok(value) if !value.is_empty() => PathBuf::from(value),
            _ => std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".local/share"),
        };
        url_path(&base.join("digup").join("index").to_string_lossy())
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(path)
}

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `speed(_:)` (main.swift:476): the model-free check is Metal's (none here), then what
/// indexing has measured on this machine.
fn speed(args: &Arguments) -> Result<(), String> {
    if args.flag("--model") {
        return model_speed(args);
    }
    println!("no speed check here (no Metal): the fixed costs stand in until indexing has measured this machine's");
    let folder = args.index_folder();
    let measured = MeasuredCosts::load(&folder);
    let home = std::env::var("HOME").unwrap_or_default();
    let line = format!(
        "measured by indexing ({}):",
        formats::tilde(&MeasuredCosts::file(&folder).to_string_lossy(), &home)
    ) + if measured.tallies.is_empty() {
        " nothing yet"
    } else {
        ""
    };
    println!("{line}");
    for (name, tally) in &measured.tallies {
        println!(
            "  {name} {:.4} s each, over {:.0}",
            tally.seconds / tally.units,
            tally.units
        );
    }
    Ok(())
}

/// `processorAlone` (main.swift:497): the model timed on the processor alone, in a process of
/// its own (llama.cpp loads its backends once a process), with the vectors it made, and how
/// long it took to load.
fn processor_alone(folder: &Path, tries: i64) -> Result<(modelspeed::Measurement, f64), String> {
    let program = std::env::current_exe().map_err(|e| format!("where is digup? {e}"))?;
    let output = Command::new(program)
        .args([
            "speed",
            "--model",
            "--measure-only",
            "--json",
            "--tries",
            &tries.to_string(),
            "--index",
            &folder.to_string_lossy(),
        ])
        .env(digup_runtime::DEVICE_ENV, "cpu")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|child| child.wait_with_output())
        .map_err(|e| format!("the processor's speed check failed: {e}"))?;
    if !output.status.success() {
        return Err("the processor's speed check failed".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let line = text.lines().last().unwrap_or("").to_string();
    let reply: serde_json::Value = serde_json::from_str(&line)
        .map_err(|_| "the processor's speed check failed".to_string())?;
    let seconds: BTreeMap<String, f64> = serde_json::from_value(
        reply
            .get("seconds")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|_| "the processor's speed check failed".to_string())?;
    let times =
        modelspeed::Times::from_fields(&seconds).ok_or("the processor's speed check failed")?;
    let vectors: std::collections::HashMap<String, Vec<f32>> = reply
        .get("vectors")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(name, value)| {
                    let items = value.as_array()?;
                    Some((
                        name.clone(),
                        items
                            .iter()
                            .filter_map(|item| item.as_f64().map(|x| x as f32))
                            .collect(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let measurement = modelspeed::Measurement {
        times,
        vectors,
        tries: reply
            .get("tries")
            .and_then(|v| v.as_array())
            .map(|items| items.iter().filter_map(|item| item.as_f64()).collect())
            .unwrap_or_default(),
        steady: reply
            .get("steady")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        others: reply.get("others").and_then(|v| v.as_f64()).unwrap_or(0.0),
    };
    let load = reply.get("load").and_then(|v| v.as_f64()).unwrap_or(0.0);
    Ok((measurement, load))
}

/// `embedderSource()` (main.swift:208): llama.cpp's, known without starting it — a worker
/// folder in the environment (the payload's helper) means the CLI can't know.
fn embedder_source() -> Option<&'static str> {
    if std::env::var_os("DIGUP_WORKER_DIR").is_some() {
        None
    } else {
        Some("llama.cpp")
    }
}

/// `modelSpeed` (main.swift:534): the model timed on this machine, kept in costs.json beside
/// the index for estimates. Where there's a GPU it's timed against the processor alone, and
/// the faster is kept (`device`) for every helper: a weak integrated GPU can be slower than its
/// processor. The GPU is kept only if its vectors are the processor's too (cosine 0.999, as
/// ReferenceVectorTests asks). The processor goes first, so a GPU that's clearly slower stops
/// after two tries.
fn model_speed(args: &Arguments) -> Result<(), String> {
    let folder = args.index_folder();
    let json = args.flag("--json");
    let tries = args
        .value("--tries")
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let log = folder.join(digup_core::paths::WORKER_LOG);
    let open_embedder = || -> Result<digup_runtime::LlamaEmbedder, String> {
        let models = digup_runtime::LlamaModels::locate().map_err(|e| e.to_string())?;
        digup_runtime::LlamaEmbedder::new(&models, false, None, Some(&log))
            .map_err(|e| e.to_string())
    };
    if args.flag("--measure-only") {
        let started = Instant::now();
        let model = open_embedder()?;
        let load = started.elapsed().as_secs_f64();
        let measured = modelspeed::measure(&model, tries, None)?;
        drop(model);
        let mut fields = serde_json::Map::new();
        fields.insert("event".into(), serde_json::json!("speed"));
        fields.insert("seconds".into(), serde_json::json!(measured.times.fields()));
        fields.insert("load".into(), serde_json::json!(load));
        fields.insert("vectors".into(), serde_json::json!(measured.vectors));
        fields.insert("tries".into(), serde_json::json!(measured.tries));
        fields.insert("steady".into(), serde_json::json!(measured.steady));
        fields.insert("others".into(), serde_json::json!(measured.others));
        emit::emit(&serde_json::Value::Object(fields));
        return Ok(());
    }
    let saved = MeasuredCosts::load(&folder);
    let mut speed = saved.speed.clone().unwrap_or(Speed {
        embedding: 1.0,
        ocr: 1.0,
        ..Default::default()
    });
    let compares =
        !digup_runtime::cpu_only() && digup_runtime::has_gpu() && embedder_source().is_some();
    let checks = if speed.kinds.is_none() {
        0
    } else {
        speed.checks.unwrap_or(1)
    };
    let again = checks < 3
        && (speed.doubtful == Some(true) || saved.pace().map(|pace| pace < 0.25) == Some(true));
    if args.flag("--if-needed") {
        if let Some(kinds) = &speed.kinds {
            if (speed.device.is_some() || !compares) && !again {
                if json {
                    let mut fields = serde_json::Map::new();
                    fields.insert("event".into(), serde_json::json!("speed"));
                    fields.insert("kinds".into(), serde_json::json!(kinds));
                    fields.insert("measured".into(), serde_json::json!(false));
                    emit::emit(&serde_json::Value::Object(fields));
                }
                return Ok(());
            }
        }
    }
    let quiet = args
        .value("--when-quiet")
        .and_then(|value| value.parse().ok())
        .map(system_load::wait_for_quiet)
        .unwrap_or((0.0, true, None));
    let (waited, settled, calmest) = quiet;
    let started = Instant::now();
    let alone = if compares {
        Some(processor_alone(&folder, tries)?)
    } else {
        None
    };
    let loading = Instant::now();
    let model = open_embedder()?;
    let mut load = loading.elapsed().as_secs_f64();
    let measured = modelspeed::measure(
        &model,
        tries,
        alone.as_ref().map(|(alone, _)| alone.total()),
    )?;
    drop(model);
    let mut kept = measured.clone();
    let mut other = alone.as_ref().map(|(alone, _)| alone.clone());
    let mut compared = serde_json::Map::new();
    if let Some((alone, alone_load)) = &alone {
        let agreement = modelspeed::agreement(&measured.vectors, &alone.vectors);
        let fields_of = |run: &modelspeed::Measurement, load: f64| {
            serde_json::json!({
                "seconds": run.times.fields(),
                "load": load,
                "tries": run.tries,
                "steady": run.steady,
                "others": run.others,
            })
        };
        compared.insert("gpu".into(), fields_of(&measured, load));
        compared.insert("cpu".into(), fields_of(alone, *alone_load));
        compared.insert("agreement".into(), serde_json::json!(agreement));
        speed.device = Some(if agreement >= 0.999 && measured.total() <= alone.total() {
            "gpu".to_string()
        } else {
            "cpu".to_string()
        });
        if speed.device.as_deref() == Some("cpu") {
            kept = alone.clone();
            other = Some(measured.clone());
            load = *alone_load;
        }
    }
    // Close times from a run whose best wasn't seen twice could have gone the other way.
    let close = other
        .as_ref()
        .map(|other| !other.steady && other.total() < kept.total() * 1.3)
        .unwrap_or(false);
    speed.kinds = Some(modelspeed::factors(&kept.times));
    speed.checks = Some(checks + 1);
    speed.doubtful = if !kept.steady || close {
        Some(true)
    } else {
        None
    };
    // What indexing measured on the other device doesn't hold on this one.
    let forget = saved
        .speed
        .as_ref()
        .and_then(|saved| saved.device.as_ref())
        .map(|saved| Some(saved.as_str()) != speed.device.as_deref())
        .unwrap_or(false);
    MeasuredCosts::set_speed(speed.clone(), &folder, forget);
    let total = started.elapsed().as_secs_f64();
    if json {
        let mut fields = serde_json::Map::new();
        fields.insert("event".into(), serde_json::json!("speed"));
        fields.insert("kinds".into(), serde_json::json!(speed.kinds));
        fields.insert("seconds".into(), serde_json::json!(kept.times.fields()));
        fields.insert("load".into(), serde_json::json!(load));
        fields.insert("total".into(), serde_json::json!(total));
        fields.insert("measured".into(), serde_json::json!(true));
        fields.insert("tries".into(), serde_json::json!(kept.tries));
        fields.insert("others".into(), serde_json::json!(kept.others));
        fields.insert("waited".into(), serde_json::json!(waited));
        fields.insert("settled".into(), serde_json::json!(settled));
        fields.insert(
            "doubtful".into(),
            serde_json::json!(speed.doubtful.unwrap_or(false)),
        );
        fields.insert("checks".into(), serde_json::json!(checks + 1));
        if let Some(calmest) = calmest {
            fields.insert("calmest".into(), serde_json::json!(calmest));
        }
        if let Some(device) = &speed.device {
            fields.insert("device".into(), serde_json::json!(device));
        }
        for (key, value) in compared {
            fields.insert(key, value);
        }
        emit::emit(&serde_json::Value::Object(fields));
        return Ok(());
    }
    if waited > 0.0 {
        println!(
            "waited {waited:.0} s for other programs to settle{}",
            if settled { "" } else { " (they didn't)" }
        );
    }
    if let Some(device) = &speed.device {
        println!(
            "runs on the {}",
            if device == "cpu" {
                "processor (faster here than the GPU)"
            } else {
                "GPU"
            }
        );
    }
    println!(
        "the model loaded in {:.2} s; the check took {:.1} s; tries {}",
        load,
        total,
        kept.tries
            .iter()
            .map(|seconds| format!("{seconds:.2} s"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let baseline = modelspeed::baseline_fields();
    for (name, seconds) in kept.times.fields() {
        let base = baseline.get(&name).copied().unwrap_or(seconds);
        println!(
            "  {name} {:.3} s (M4 Pro {:.3} s, ×{:.1})",
            seconds,
            base,
            seconds / base
        );
    }
    if speed.doubtful == Some(true) {
        println!("in doubt (a busy moment): timed again at the next --if-needed");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    println!(
        "kept in {}: {}",
        formats::tilde(&MeasuredCosts::file(&folder).to_string_lossy(), &home),
        speed
            .kinds
            .as_ref()
            .map(|kinds| kinds
                .iter()
                .map(|(name, value)| format!("{name} ×{:.1}", value))
                .collect::<Vec<_>>()
                .join(" · "))
            .unwrap_or_default()
    );
    Ok(())
}

/// `estimate` (main.swift:212-300): what indexing these folders will cost here, from
/// metadata alone. Costs come from what indexing has measured on this machine
/// (`MeasuredCosts`); until then the M4 Pro's fixed numbers, scaled by the speed check.
/// `index(_:)` (main.swift:516): the human one-shot index — the store, the
/// full model with its log beside the index, the sync, the first pass over the
/// pending queue, and the rest of the long files. The lines are the CLI's own
/// words (`pad`/`lpad`/`duration`/`statesLine` in main.swift); the contract
/// suite seeds its store with exactly this verb.
fn index(args: &Arguments) -> Result<(), String> {
    use digup_core::IndexStore;
    use digup_runtime::indexer::{Indexer, MakeEmbedder};
    use digup_runtime::models::LlamaModels;
    use digup_runtime::worker::Embedder;
    use digup_runtime::LlamaEmbedder;
    if args.positional.len() < 2 {
        return Err("which folders? e.g. digup index ~/DigUpTestbed".to_string());
    }
    // `Arguments.folders()` (main.swift:117): the same refusals, the same words.
    let mut folders: Vec<PathBuf> = Vec::new();
    for folder_arg in &args.positional[1..] {
        let folder = folder_url(folder_arg);
        if !std::fs::metadata(&folder)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            return Err(format!("not a folder: {folder_arg}"));
        }
        if !digup_core::crawler::can_list(&folder.to_string_lossy()) {
            return Err(format!("can't read {folder_arg} (macOS privacy settings?)"));
        }
        folders.push(folder);
    }
    let index_folder = args.index_folder();
    let log = index_folder.join(digup_core::paths::WORKER_LOG);
    let store = IndexStore::open(&index_folder, args.store_file()).map_err(|e| e.to_string())?;
    if args.flag("--reindex") {
        store.reset_all().map_err(|e| e.to_string())?;
    }
    // `makeEmbedder(textOnly: false, log:)` (main.swift:519): the full model, its
    // llama.cpp log beside the index — the same file the app's runs append to.
    let make_embedder: MakeEmbedder = Box::new(move || {
        let models = LlamaModels::locate()?;
        let embedder = LlamaEmbedder::new(&models, false, None, Some(&log))?;
        Ok(Box::new(embedder) as Box<dyn Embedder>)
    });
    let mut indexer =
        Indexer::new(store, args.options(), make_embedder).map_err(|e| e.to_string())?;
    indexer.costs_folder = Some(index_folder.clone());
    indexer.embedder_source = Some(digup_runtime::model_info().vector_source());
    indexer.store.read_again_once().map_err(|e| e.to_string())?;
    indexer.check_model().map_err(|e| e.to_string())?;
    let sync = indexer
        .sync(&folders, &mut |_| {})
        .map_err(|e| e.to_string())?;
    let home = std::env::var("HOME").unwrap_or_default();
    println!(
        "index: {}",
        formats::tilde(&indexer.store.file.to_string_lossy(), &home)
    );
    println!(
        "sync: {} new · {} changed · {} moved · {} removed · {} unchanged",
        sync.added, sync.changed, sync.moved, sync.removed, sync.unchanged
    );
    if !sync.crawl.skipped.is_empty() {
        println!("skipped: {}", skipped_line(&sync.crawl.skipped));
    }
    let mut datasets: Vec<(&String, &i64)> = sync.crawl.datasets.iter().collect();
    datasets.sort();
    for (folder, count) in datasets {
        println!(
            "looks like a dataset (skipped): {} ({} files)",
            formats::tilde(folder, &home),
            formats::number(*count)
        );
    }
    let limit = args.value("--limit").and_then(|value| value.parse().ok());
    let started = Instant::now();
    let states = indexer
        .run(limit, None, &|| false, &mut |event| {
            let name = Path::new(&event.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| event.path.clone());
            let note = event
                .note
                .as_ref()
                .map(|note| format!("  ({note})"))
                .unwrap_or_default();
            println!(
                "[{}/{}] {} {} {:5.2}s  {}{}",
                formats::lpad(&event.done.to_string(), 4),
                event.total,
                formats::pad(event.kind.as_str(), 10),
                formats::pad(&event.state, 7),
                event.seconds,
                name,
                note
            );
        })
        .map_err(|e| e.to_string())?;
    if states.is_empty() {
        println!("nothing pending");
    } else {
        println!(
            "done in {}: {}",
            formats::duration(started.elapsed().as_secs_f64()),
            formats::states_line(&states.into_iter().collect())
        );
    }
    // The rest of long files, as the app does once nothing is pending (not
    // after a --limit run: things still are).
    if limit.is_none()
        && indexer
            .store
            .unfinished_count()
            .map_err(|e| e.to_string())?
            .0
            > 0
    {
        let started = Instant::now();
        let finished = indexer
            .finish_long_files(&|| false, &mut |step| {
                let name = Path::new(&step.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| step.path.clone());
                let note = step
                    .note
                    .as_ref()
                    .map(|note| format!("  ({note})"))
                    .unwrap_or_default();
                println!(
                    "[rest {}/{}] {} {} {:5.2}s  {}{}",
                    formats::lpad(&step.done.to_string(), 5),
                    step.total,
                    formats::pad(step.kind.as_str(), 4),
                    formats::pad(&step.state, 7),
                    step.seconds,
                    name,
                    note
                );
            })
            .map_err(|e| e.to_string())?;
        println!(
            "the rest of long files in {}: {}",
            formats::duration(started.elapsed().as_secs_f64()),
            if finished.is_empty() {
                "none finished".to_string()
            } else {
                formats::states_line(&finished.into_iter().collect())
            }
        );
    }
    indexer.finish(); // frees the model, clears the scratch folder
    Ok(())
}

/// `status(_:)` (main.swift:398): the CLI's own status text — the index path,
/// vector + segment counts, the per-kind table, the fingerprint, this build's
/// tag, the bytes on disk. `formats.rs` pins the exact shapes; the server
/// parses these lines and shows the engine's words, so a line this port
/// cannot produce is a line the server must not fake.
fn status(args: &Arguments) -> digup_core::db::Result<()> {
    use digup_core::model::FileKind;
    let store = digup_core::IndexStore::open(&args.index_folder(), args.store_file())?;
    let counts = store.counts()?;
    let home = std::env::var("HOME").unwrap_or_default();
    println!(
        "index: {}",
        formats::tilde(&store.file.to_string_lossy(), &home)
    );
    println!(
        "vectors: {} in {} segments",
        formats::number(counts.vectors),
        formats::number(counts.segments)
    );
    for kind in [
        FileKind::Screenshot,
        FileKind::Image,
        FileKind::Pdf,
        FileKind::Doc,
        FileKind::Audio,
        FileKind::Video,
        FileKind::Code,
    ] {
        let Some(states) = counts.by_kind_and_state.get(kind.as_str()) else {
            continue; // a kind with no rows prints no line (Swift's `guard let … else continue`)
        };
        let total: i64 = states.values().sum();
        println!(
            "  {} {}   {}",
            formats::pad(kind.as_str(), 11),
            formats::lpad(&formats::number(total), 6),
            formats::states_line(states)
        );
    }
    if counts.unfinished > 0 {
        println!(
            "long files still being read: {} ({} pages and chunks left)",
            formats::number(counts.unfinished),
            formats::number(counts.left)
        );
    }
    if let Some(fingerprint) = store.meta("fingerprint")? {
        println!("built with: {fingerprint}");
    }
    println!(
        "this digup: llama.cpp {}, vectors {}",
        digup_runtime::build_tag(),
        digup_runtime::VECTOR_VERSION
    );
    // `IndexStore.size(of:)`: the database and its write-ahead log.
    let mut size = 0i64;
    for path in [
        store.file.clone(),
        std::path::PathBuf::from(format!("{}-wal", store.file.to_string_lossy())),
    ] {
        size += std::fs::metadata(&path)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
    }
    println!("size: {}", formats::file_size(size));
    Ok(())
}

fn estimate(args: &Arguments) -> Result<(), String> {
    if args.positional.len() < 2 {
        return Err("which folders? e.g. digup estimate ~/Documents".to_string());
    }
    let json = args.flag("--json");
    if json {
        digup_services::parentwatch::start(); // an app's helper goes when the app does
    }
    let options = args.options();
    let index_folder = args.index_folder();
    let measured = MeasuredCosts::load(&index_folder);
    // The model-free speed check that fills a missing `speed` is Metal's (`Speed.measure`
    // returns nil here); the estimate runs on what's kept, whatever it is.
    let costs = measured.costs();
    let calibrated = measured.speed.is_some() || !measured.tallies.is_empty();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut total = 0.0;
    for folder_arg in &args.positional[1..] {
        let folder = folder_url(folder_arg);
        // One folder that isn't there or can't be read mustn't sink the others.
        let readable = std::fs::metadata(&folder)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
            && digup_core::crawler::can_list(&folder.to_string_lossy());
        if !readable {
            if json {
                let mut fields = serde_json::Map::new();
                fields.insert("folder".into(), serde_json::json!(folder));
                fields.insert("readable".into(), serde_json::json!(false));
                fields.insert("seconds".into(), serde_json::json!(0));
                fields.insert("files".into(), serde_json::json!({}));
                fields.insert("pdf_pages".into(), serde_json::json!(0));
                fields.insert("audio_seconds".into(), serde_json::json!(0));
                fields.insert("video_seconds".into(), serde_json::json!(0));
                emit::emit(&serde_json::Value::Object(fields));
            } else {
                println!(
                    "{}    can't read it (not there, or not allowed in macOS privacy settings)",
                    formats::tilde(&folder.to_string_lossy(), &home)
                );
            }
            continue;
        }
        let started = Instant::now();
        let crawl =
            digup_core::crawler::crawl(std::slice::from_ref(&folder), &options, &mut |_| {});
        let parts = digup_core::estimator::estimates_by_subfolder(
            &crawl.candidates,
            &folder.to_string_lossy(),
            &options,
            &costs,
        );
        let mut estimate = digup_core::estimator::Estimate::default();
        for part in parts.values() {
            estimate.add(part);
        }
        // Biggest first; the app lists them under the folder, so a long first pass can be cut down.
        let mut subfolders: Vec<(String, &digup_core::estimator::Estimate)> = parts
            .iter()
            .filter(|(key, _)| key.as_str() != folder.to_string_lossy().as_ref())
            .map(|(key, value)| (key.clone(), value))
            .collect();
        subfolders.sort_by(|a, b| {
            b.1.seconds
                .partial_cmp(&a.1.seconds)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        total += estimate.seconds;
        if json {
            // One line per folder, as soon as it's done (the app's onboarding shows them as they come).
            let mut fields = serde_json::Map::new();
            fields.insert("folder".into(), serde_json::json!(folder));
            fields.insert("seconds".into(), serde_json::json!(estimate.seconds));
            fields.insert("readable".into(), serde_json::json!(true));
            fields.insert("files".into(), files_json(&estimate));
            fields.insert("kind_seconds".into(), kind_seconds_json(&estimate));
            fields.insert("calibrated".into(), serde_json::json!(calibrated));
            fields.insert("pdf_pages".into(), serde_json::json!(estimate.pdf_pages));
            fields.insert(
                "pdf_pages_total".into(),
                serde_json::json!(estimate.pdf_pages_total),
            );
            fields.insert(
                "audio_seconds".into(),
                serde_json::json!(estimate.audio_seconds),
            );
            fields.insert(
                "video_seconds".into(),
                serde_json::json!(estimate.video_seconds),
            );
            fields.insert("long_files".into(), serde_json::json!(estimate.long_files));
            fields.insert(
                "later_seconds".into(),
                serde_json::json!(estimate.later_seconds),
            );
            fields.insert(
                "code_stretches".into(),
                serde_json::json!(estimate.code_stretches),
            );
            fields.insert("bytes".into(), serde_json::json!(estimate.bytes));
            fields.insert(
                "subfolders".into(),
                serde_json::json!(subfolders
                    .iter()
                    .take(500)
                    .map(|(key, value)| {
                        let mut entry = serde_json::Map::new();
                        entry.insert("folder".into(), serde_json::json!(key));
                        entry.insert("seconds".into(), serde_json::json!(value.seconds));
                        entry.insert("files".into(), files_json(value));
                        entry.insert("kind_seconds".into(), kind_seconds_json(value));
                        entry.insert("long_files".into(), serde_json::json!(value.long_files));
                        entry.insert(
                            "later_seconds".into(),
                            serde_json::json!(value.later_seconds),
                        );
                        entry.insert("bytes".into(), serde_json::json!(value.bytes));
                        entry
                    })
                    .collect::<Vec<_>>()),
            );
            fields.insert(
                "skipped".into(),
                serde_json::json!(crawl.skipped.values().sum::<i64>()),
            );
            fields.insert("left_out".into(), serde_json::json!(crawl.skipped));
            fields.insert("datasets".into(), serde_json::json!(crawl.datasets.len()));
            fields.insert(
                "ms".into(),
                serde_json::json!((started.elapsed().as_secs_f64() * 1000.0) as i64),
            );
            emit::emit(&serde_json::Value::Object(fields));
            continue;
        }
        let mut kinds: Vec<String> = Vec::new();
        for kind in [
            digup_core::model::FileKind::Screenshot,
            digup_core::model::FileKind::Image,
            digup_core::model::FileKind::Pdf,
            digup_core::model::FileKind::Doc,
            digup_core::model::FileKind::Audio,
            digup_core::model::FileKind::Video,
            digup_core::model::FileKind::Code,
        ] {
            let Some(count) = estimate.files.get(&kind).copied() else {
                continue;
            };
            if count == 0 {
                continue;
            }
            match kind {
                digup_core::model::FileKind::Pdf => kinds.push(format!(
                    "{} PDFs ({} pages; {} in the first pass)",
                    formats::number(count),
                    formats::number(estimate.pdf_pages_total),
                    formats::number(estimate.pdf_pages)
                )),
                digup_core::model::FileKind::Audio => kinds.push(format!(
                    "{} audio ({})",
                    formats::number(count),
                    formats::duration(estimate.audio_seconds)
                )),
                digup_core::model::FileKind::Video => kinds.push(format!(
                    "{} video ({})",
                    formats::number(count),
                    formats::duration(estimate.video_seconds)
                )),
                digup_core::model::FileKind::Code => kinds.push(format!(
                    "{} code {} ({} stretches)",
                    formats::number(count),
                    if count == 1 { "file" } else { "files" },
                    formats::number(estimate.code_stretches)
                )),
                _ => kinds.push(format!(
                    "{} {}{}",
                    formats::number(count),
                    kind.as_str(),
                    if count == 1 { "" } else { "s" }
                )),
            }
        }
        println!(
            "{}    first index ≈ {}{}",
            formats::tilde(&folder.to_string_lossy(), &home),
            formats::duration(estimate.seconds),
            if options.code {
                format!(", index ≈ {}", byte_count(estimate.bytes as i64))
            } else {
                String::new()
            }
        );
        println!(
            "  {}",
            if kinds.is_empty() {
                "nothing to index".to_string()
            } else {
                kinds.join(" · ")
            }
        );
        if estimate.long_files > 0 {
            println!(
                "  then the rest of {} long {} ≈ {}",
                formats::number(estimate.long_files),
                if estimate.long_files == 1 {
                    "file"
                } else {
                    "files"
                },
                formats::duration(estimate.later_seconds)
            );
        }
        if !subfolders.is_empty() {
            println!(
                "  biggest subfolders: {}{}",
                subfolders
                    .iter()
                    .take(5)
                    .map(|(key, value)| format!(
                        "{} ≈ {}",
                        digup_core::paths::last_component(key),
                        formats::duration(value.seconds)
                    ))
                    .collect::<Vec<_>>()
                    .join(" · "),
                if subfolders.len() > 5 {
                    format!(" · {} more", formats::number((subfolders.len() - 5) as i64))
                } else {
                    String::new()
                }
            );
        }
        if !crawl.skipped.is_empty() {
            println!("  skipped: {}", skipped_line(&crawl.skipped));
        }
        if !crawl.skipped_folders.is_empty() {
            println!(
                "  folders not walked: {}",
                skipped_line(&crawl.skipped_folders)
            );
        }
        let mut datasets: Vec<(String, i64)> = crawl
            .datasets
            .iter()
            .map(|(f, c)| (f.clone(), *c))
            .collect();
        datasets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (dataset_folder, count) in datasets {
            println!(
                "  looks like a dataset (skipped): {} ({} files)",
                formats::tilde(&dataset_folder, &home),
                formats::number(count)
            );
        }
        println!(
            "  (looked at it in {:.2} s)",
            started.elapsed().as_secs_f64()
        );
    }
    if args.positional.len() > 2 && !json {
        println!("all together ≈ {}", formats::duration(total));
    }
    Ok(())
}

/// `Arguments.folderURL`: the URL shape of main.swift:127-128 — the tilde
/// first (`expandingTildeInPath`), then `URL(fileURLWithPath:)`, which
/// makes a RELATIVE argument absolute against the process's CWD, then
/// `standardizedFileURL` (`paths::standardize`: `.`/`..` removed lexically,
/// doubled slashes collapsed, the trailing slash dropped, surplus `..` at
/// the root dropped). The index rows carry the root strings (AGENTS.md's
/// out-of-scope rule): a relative launch must name the folder it resolves
/// to, not the spelling the shell typed.
fn folder_url(path: &str) -> PathBuf {
    url_path(path)
}

/// The shared `URL(fileURLWithPath:)` + `standardizedFileURL` shape (see
/// `folder_url` and `index_folder`).
fn url_path(path: &str) -> PathBuf {
    let expanded = expand_tilde(path);
    let absolute = if expanded.starts_with("/") {
        expanded
    } else {
        std::env::current_dir().unwrap_or_default().join(expanded)
    };
    PathBuf::from(digup_core::paths::standardize(&absolute.to_string_lossy()))
}

/// `files(estimate)`: FileKind raw values → counts (the JSON contract).
fn files_json(estimate: &digup_core::estimator::Estimate) -> serde_json::Value {
    let map = estimate
        .files
        .iter()
        .map(|(kind, count)| (kind.as_str().to_string(), serde_json::json!(count)))
        .collect::<serde_json::Map<_, _>>();
    serde_json::Value::Object(map)
}

fn kind_seconds_json(estimate: &digup_core::estimator::Estimate) -> serde_json::Value {
    let map = estimate
        .kind_seconds
        .iter()
        .map(|(kind, seconds)| (kind.as_str().to_string(), serde_json::json!(seconds)))
        .collect::<serde_json::Map<_, _>>();
    serde_json::Value::Object(map)
}

/// `skippedLine`: reason → count, biggest first. Ties are the engine's dictionary order, which
/// Swift seeds per process (the engine's own two runs of the same folder list the same counts
/// in different orders), so they are ordered by name here; the counts are what is pinned.
fn skipped_line(skipped: &std::collections::HashMap<String, i64>) -> String {
    let mut lines: Vec<(i64, &String)> = skipped
        .iter()
        .map(|(reason, count)| (*count, reason))
        .collect();
    lines.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    lines
        .iter()
        .map(|(count, reason)| format!("{} {}", formats::number(*count), reason))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `ByteCountFormatter.string(fromByteCount:, countStyle: .file)`, pinned by probing the
/// formatter on this engine (Swift 6.4 / corelibs): the unit is chosen from the byte count
/// (base 1000, 999 999 B is "1,000 KB" — the unit never promotes after rounding), then the
/// mantissa keeps 0 decimals for bytes and KB, 1 for MB, 2 from GB up, with trailing zeros
/// trimmed ("1049999999999 → 1.05 TB", "1000000000000 → 1 TB") and thousands groups in the
/// integer part ("999999 → 1,000 KB"). 1 byte is singular.
fn byte_count(bytes: i64) -> String {
    const UNITS: [&str; 7] = ["byte", "KB", "MB", "GB", "TB", "PB", "EB"];
    // The formatter rounds to nearest with ties to even: 10 450 B is "10 KB", 11 500 B is "12 KB".
    const DECIMALS: [usize; 7] = [0, 0, 1, 2, 2, 2, 2];
    // The unit follows the byte count (base 1000); 999 999 B stays KB and reads "1,000 KB".
    let unit = if bytes < 1000 {
        0
    } else {
        ((bytes as f64).log10() / 3.0).floor().max(0.0) as usize
    };
    let value = if unit == 0 {
        bytes as f64
    } else {
        bytes as f64 / 1000f64.powi(unit as i32)
    };
    let text = if DECIMALS[unit] == 0 {
        formats::number(value.round_ties_even() as i64)
    } else {
        let rounded = format!("{:.*}", DECIMALS[unit], value);
        let rounded = rounded
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string();
        let (head, tail) = match rounded.find('.') {
            Some(at) => (rounded[..at].to_string(), rounded[at..].to_string()),
            None => (rounded.clone(), String::new()),
        };
        format!(
            "{}{}",
            formats::number(head.parse::<i64>().unwrap_or(0)),
            tail
        )
    };
    let unit_name = match unit {
        0 if bytes == 1 => "byte",
        0 => "bytes",
        _ => UNITS[unit],
    };
    format!("{text} {unit_name}")
}

/// The Phase-1 self-test: open a v6 index and report it.
/// `CodeQuery` (Model.swift:104): the "code:" prefix that routes a query to the
/// code index. `text(of:)`: what's typed after it ("" for "code:" alone), nil for
/// anything that isn't a code query.
fn code_query_text(query: &str) -> Option<String> {
    const PREFIX: &str = "code:";
    let trimmed = query.trim_start();
    let head: String = trimmed.chars().take(PREFIX.chars().count()).collect();
    if head.to_ascii_lowercase() != PREFIX {
        return None;
    }
    Some(
        trimmed
            .chars()
            .skip(PREFIX.chars().count())
            .collect::<String>()
            .trim()
            .to_string(),
    )
}

/// `Arguments.kinds()` (main.swift:145): the `--kind` list, the refusal worded verbatim.
fn search_kinds(
    args: &Arguments,
) -> Result<Option<std::collections::HashSet<digup_core::model::FileKind>>, String> {
    use digup_core::model::FileKind;
    match args.value("--kind") {
        None => Ok(None),
        Some(list) => {
            let mut set = std::collections::HashSet::new();
            for item in list.split(',') {
                match FileKind::parse(item.trim()) {
                    Some(kind) => {
                        set.insert(kind);
                    }
                    None => {
                        return Err(format!(
                            "unknown kind {item}; use screenshot, image, pdf, doc, audio, video"
                        ))
                    }
                }
            }
            Ok(Some(set))
        }
    }
}

/// `queryVectors` (main.swift:351): the queries through a text-only worker whose
/// log rides beside the index; nil when the index has no vectors (keyword-only
/// search), an error when two pipelines' vectors would meet.
fn query_vectors(
    queries: &[String],
    store: &digup_core::IndexStore,
    searcher: &digup_core::searcher::Searcher<'_>,
    prompt: fn(&str) -> String,
) -> Result<Option<Vec<Vec<f32>>>, String> {
    use digup_runtime::worker::Embedder;
    use digup_runtime::{LlamaEmbedder, LlamaModels};
    if searcher.vector_count() == 0 || queries.is_empty() {
        return Ok(None);
    }
    let models = LlamaModels::locate().map_err(|e| e.to_string())?;
    let log = store.directory.join(digup_core::paths::WORKER_QUERY_LOG);
    let embedder =
        LlamaEmbedder::new(&models, true, None, Some(&log)).map_err(|e| e.to_string())?;
    if let Some(source) = &searcher.vector_source {
        let mine = embedder.info().vector_source();
        if *source != mine {
            return Err(format!(
                "index vectors come from {source} but the query encoder is {mine}"
            ));
        }
    }
    let prompted: Vec<String> = queries.iter().map(|q| prompt(q)).collect();
    Ok(Some(
        embedder.embed_texts(&prompted).map_err(|e| e.to_string())?,
    ))
}

/// `location(_:)` (main.swift:178): the place a hit points at, in the CLI's words.
fn location(hit: &digup_core::searcher::SearchHit) -> String {
    use digup_core::model::SegmentKind;
    match hit.segment {
        SegmentKind::Page | SegmentKind::PageImage => hit
            .loc
            .map(|loc| format!("p. {}", loc as i64))
            .unwrap_or_default(),
        SegmentKind::Audio => hit
            .loc
            .map(|loc| format!("at {} (sound)", formats::clock(loc)))
            .unwrap_or_default(),
        SegmentKind::Frame => hit
            .loc
            .map(|loc| format!("at {}", formats::clock(loc)))
            .unwrap_or_default(),
        SegmentKind::Chunk => hit
            .loc
            .map(|loc| format!("part {}", loc as i64))
            .unwrap_or_default(),
        SegmentKind::Lines => hit
            .loc
            .map(|loc| {
                format!("lines {}–{}", loc as i64, hit.loc_end.unwrap_or(loc) as i64)
                    + &hit
                        .focus
                        .map(|focus| format!(", opens at {}", focus as i64))
                        .unwrap_or_default()
            })
            .unwrap_or_default(),
        SegmentKind::Cells => hit
            .loc
            .map(|loc| {
                if hit.loc_end == Some(loc) {
                    format!("cell {}", loc as i64)
                } else {
                    format!("cells {}–{}", loc as i64, hit.loc_end.unwrap_or(loc) as i64)
                }
            })
            .unwrap_or_default(),
        SegmentKind::Ocr => "text in image".to_string(),
        _ => String::new(),
    }
}

/// Foundation's number shape for `JSONSerialization` (probed on this box against
/// Swift 6.4's writer, digit for digit): shortest round-trip digits — like Rust's
/// `{:e}` — written in exponent form when the leading digit's power is < -4 or
/// ≥ 16 ("1e-05", "1e+16"), decimal form otherwise, integral values bare ("3"),
/// negative zero "-0", exponents signed with at least two digits.
fn swift_number(value: &serde_json::Number) -> String {
    if let Some(int) = value.as_i64() {
        return int.to_string();
    }
    if let Some(int) = value.as_u64() {
        return int.to_string();
    }
    let Some(number) = value.as_f64() else {
        return value.to_string();
    };
    if number == 0.0 {
        return if number.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }
    let sign = if number.is_sign_negative() { "-" } else { "" };
    // The shortest decimal Rust's LowerExp prints: "d[.ddd]eE", digits + the power
    // of the leading digit.
    let text = format!("{:e}", number.abs());
    let (digits_part, exp_part) = text.split_once('e').expect("LowerExp prints e");
    let exponent: i32 = exp_part
        .parse()
        .expect("LowerExp prints an integer exponent");
    let digits: String = digits_part.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    // The leading digit's power: 1.23e15 → 15; 0.0001234 → 1.234e-4 → -4.
    let leading = exponent;
    if !(-4..16).contains(&leading) {
        // "1e+16", "9.9e-05", "-1.5e-10": a signed, two-at-least-digit exponent.
        let head = &digits[..1];
        let tail = &digits[1..];
        let mantissa = if tail.is_empty() {
            head.to_string()
        } else {
            format!("{head}.{tail}")
        };
        return format!("{sign}{mantissa}e{:+03}", leading);
    }
    // Fixed form: the point sits after the digit at the `leading` power.
    let len = digits.len() as i32;
    let out = if leading >= len - 1 {
        format!("{}{}", digits, "0".repeat((leading - (len - 1)) as usize))
    } else if leading >= 0 {
        let split = (leading + 1) as usize; // digits up to and including the point's left side
        format!("{}.{}", &digits[..split], &digits[split..])
    } else {
        format!("0.{}{}", "0".repeat((-leading - 1) as usize), digits)
    };
    format!("{sign}{out}")
}

/// JSONSerialization `[.prettyPrinted, .sortedKeys]` (main.swift:379-381), probed
/// byte-for-byte against Swift 6.4 on this box: 2-space indents, `"key" : value`
/// separators, `/` escaped as `\/`, controls as the short escapes with uppercase
/// `\uXXXX` for the rest, empty containers printed as `[`, a blank line, the
/// closing indent — serde's Map is a BTreeMap, so `hit_fields` rows arrive sorted.
fn swift_json(rows: &[serde_json::Value]) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '/' => out.push_str("\\/"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }
    fn write(value: &serde_json::Value, indent: usize, out: &mut String) {
        use serde_json::Value;
        match value {
            Value::Object(map) => {
                let open = " ".repeat(indent);
                if map.is_empty() {
                    out.push_str("{\n\n");
                    out.push_str(&open);
                    out.push('}');
                    return;
                }
                out.push_str("{\n");
                let inner = " ".repeat(indent + 2);
                // `.sortedKeys`: lexicographic key order (the Map is insertion-ordered
                // under serde_json's preserve_order — Foundation sorts on writing).
                let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
                entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
                for (index, (key, item)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&inner);
                    out.push('"');
                    out.push_str(&esc(key));
                    out.push_str("\" : ");
                    write(item, indent + 2, out);
                }
                out.push('\n');
                out.push_str(&open);
                out.push('}');
            }
            Value::Array(items) => {
                let open = " ".repeat(indent);
                if items.is_empty() {
                    out.push_str("[\n\n");
                    out.push_str(&open);
                    out.push(']');
                    return;
                }
                out.push_str("[\n");
                let inner = " ".repeat(indent + 2);
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&inner);
                    write(item, indent + 2, out);
                }
                out.push('\n');
                out.push_str(&open);
                out.push(']');
            }
            Value::String(text) => {
                out.push('"');
                out.push_str(&esc(text));
                out.push('"');
            }
            Value::Number(number) => out.push_str(&swift_number(number)),
            Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Value::Null => out.push_str("null"),
        }
    }
    let mut out = String::new();
    write(&serde_json::Value::Array(rows.to_vec()), 0, &mut out);
    out
}

/// `search(_:)` (main.swift:362): one query, the store's ranking relayed verbatim.
/// The human lines and the `--json` rows are the CLI's own contract; the hits'
/// numbers come from the engine (the daemon's `hit_fields` — the same rows).
fn search(args: &Arguments) -> Result<(), String> {
    use digup_core::searcher::{SearchOptions, Searcher};
    use digup_core::{IndexStore, CODE_FILE, MAIN_FILE};
    let mut query = args.positional[1..].join(" ");
    if query.is_empty() {
        return Err("search for what?".to_string());
    }
    let code = code_query_text(&query);
    if let Some(text) = code.clone() {
        query = text;
    }
    if query.is_empty() {
        return Err("search the code for what?".to_string());
    }
    let store = IndexStore::open(
        &args.index_folder(),
        if code.is_none() { MAIN_FILE } else { CODE_FILE },
    )
    .map_err(|e| e.to_string())?;
    let mut searcher = Searcher::new(&store, true).map_err(|e| e.to_string())?;
    let prompt: fn(&str) -> String = if code.is_none() {
        digup_runtime::prompts::query
    } else {
        digup_runtime::prompts::code_query
    };
    let started = Instant::now();
    let vector = query_vectors(&[query.clone()], &store, &searcher, prompt)?
        .and_then(|mut groups| groups.pop());
    let encoded = started.elapsed().as_secs_f64();
    let kinds = search_kinds(args)?;
    let opts = SearchOptions {
        kinds: kinds.as_ref(),
        limit: args
            .value("-n")
            .and_then(|v| v.parse::<i64>().ok())
            .map(|n| n.max(0) as usize)
            .unwrap_or(10),
        z_gate: args
            .value("--zgate")
            .and_then(|v| v.parse().ok())
            .unwrap_or(digup_core::scoring::Z_GATE),
        word_weight: args.value("--word-weight").and_then(|v| v.parse().ok()),
        prefix_last: false,
    };
    let hits = searcher
        .search(&query, vector.as_deref(), &opts)
        .map_err(|e| e.to_string())?;
    let searched_ms = (started.elapsed().as_secs_f64() - encoded) * 1000.0;
    if args.flag("--json") {
        let rows: Vec<serde_json::Value> = hits
            .iter()
            .map(digup_services::search_service::hit_fields)
            .collect();
        println!("{}", swift_json(&rows));
        return Ok(());
    }
    if hits.is_empty() {
        println!("no good matches");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for (rank, hit) in hits.iter().enumerate() {
        let mut why: Vec<String> = Vec::new();
        if let Some(z) = hit.z {
            why.push(format!("z {:.1}", z));
        }
        if let Some(words) = hit.keyword_rank {
            why.push(format!("words #{words}"));
        }
        let place = location(hit);
        let name = Path::new(&hit.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| hit.path.clone());
        println!(
            "{}. {} {}{}   ({})",
            formats::lpad(&(rank + 1).to_string(), 2),
            formats::pad(hit.kind.as_str(), 10),
            name,
            if place.is_empty() {
                String::new()
            } else {
                format!("  [{place}]")
            },
            why.join(", ")
        );
        let folder = Path::new(&hit.path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("      {}", formats::tilde(&folder, &home));
        if let Some(excerpt) = &hit.excerpt {
            if !excerpt.is_empty() {
                let cut: String = excerpt.graphemes(true).take(160).collect();
                println!("      “{cut}”");
            }
        }
    }
    println!(
        "({} vectors · query encoder {:.2} s incl. start-up · search {:.0} ms)",
        searcher.vector_count(),
        encoded,
        searched_ms
    );
    Ok(())
}

/// `evaluate(_:)` (main.swift:611): recall of known answers (evals/*.json). Ranks,
/// R@1/R@5/R@10, MRR and the weaker-matches fold are the engine's own search, run
/// through the same knobs (`--zgate`, `--word-weight`, `-n`).
fn evaluate(args: &Arguments) -> Result<(), String> {
    use digup_core::model::{FileKind, SegmentKind};
    use digup_core::searcher::{SearchOptions, Searcher};
    use digup_core::{IndexStore, CODE_FILE, MAIN_FILE};

    #[derive(Clone, Copy, Default)]
    struct Row {
        n: i64,
        at1: i64,
        at5: i64,
        at10: i64,
        mrr: f64,
    }

    let file = args.positional.get(1).ok_or("which eval file?")?;
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(expand_tilde(file)).map_err(|e| format!("{file}: {e}"))?,
    )
    .map_err(|e| format!("{file}: {e}"))?;
    let base_raw = doc
        .get("base")
        .and_then(|b| b.as_str())
        .ok_or("eval file has no base")?;
    let queries = doc
        .get("queries")
        .and_then(|q| q.as_array())
        .ok_or("eval file has no queries")?;
    // The testbed kept elsewhere (another drive on Windows, wherever CI puts it).
    const TESTBED: &str = "~/DigUpTestbed";
    let base = match std::env::var("DIGUP_TESTBED") {
        Ok(testbed) if base_raw.starts_with(TESTBED) => {
            format!("{}{}", testbed, &base_raw[TESTBED.len()..])
        }
        _ => expand_tilde(base_raw).to_string_lossy().into_owned(),
    };
    // "code: …" queries search the code index (with its prompt), the others the main one.
    let texts: Vec<String> = queries
        .iter()
        .map(|q| {
            let text = q.get("q").and_then(|q| q.as_str()).unwrap_or("");
            code_query_text(text).unwrap_or_else(|| text.to_string())
        })
        .collect();
    let is_code: Vec<bool> = queries
        .iter()
        .map(|q| code_query_text(q.get("q").and_then(|q| q.as_str()).unwrap_or("")).is_some())
        .collect();
    let depth = args
        .value("-n")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|n| n.max(0) as usize)
        .unwrap_or(10);
    let z_gate = args
        .value("--zgate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(digup_core::scoring::Z_GATE);
    let word_weight = args.value("--word-weight").and_then(|v| v.parse().ok());
    let index = args.index_folder();
    let main_store = if is_code.iter().any(|code| !*code) {
        Some(IndexStore::open(&index, MAIN_FILE).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let code_store = if is_code.iter().any(|code| *code) {
        Some(IndexStore::open(&index, CODE_FILE).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let mut main_searcher: Option<Searcher<'_>> = main_store
        .as_ref()
        .map(|store| Searcher::new(store, true))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut code_searcher: Option<Searcher<'_>> = code_store
        .as_ref()
        .map(|store| Searcher::new(store, true))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; queries.len()];
    for group in [false, true] {
        let slots: Vec<usize> = (0..queries.len())
            .filter(|i| is_code[*i] == group)
            .collect();
        if slots.is_empty() {
            continue;
        }
        let (searcher, store, prompt) = if group {
            (
                code_searcher.as_mut().expect("the code searcher"),
                code_store.as_ref().expect("the code store"),
                digup_runtime::prompts::code_query as fn(&str) -> String,
            )
        } else {
            (
                main_searcher.as_mut().expect("the main searcher"),
                main_store.as_ref().expect("the main store"),
                digup_runtime::prompts::query as fn(&str) -> String,
            )
        };
        let group_texts: Vec<String> = slots.iter().map(|i| texts[*i].clone()).collect();
        if let Some(embedded) = query_vectors(&group_texts, store, &*searcher, prompt)? {
            for (slot, vector) in slots.iter().zip(embedded) {
                vectors[*slot] = Some(vector);
            }
        }
    }
    let mut by_tag: Vec<(String, Row)> = Vec::new();
    let mut shown = 0i64;
    let mut listed = 0i64;
    let mut folded = 0i64; // the app's weaker-matches fold (Searcher.strongCount)
    for (i, query) in queries.iter().enumerate() {
        let kinds: Option<std::collections::HashSet<FileKind>> = query
            .get("kind")
            .and_then(|k| k.as_str())
            .and_then(FileKind::parse)
            .map(|kind| std::collections::HashSet::from([kind]));
        let searcher: &mut Searcher<'_> = if is_code[i] {
            code_searcher.as_mut().expect("the code searcher")
        } else {
            main_searcher.as_mut().expect("the main searcher")
        };
        let opts = SearchOptions {
            kinds: kinds.as_ref(),
            limit: depth,
            z_gate,
            word_weight,
            prefix_last: false,
        };
        let hits = searcher
            .search(&texts[i], vectors[i].as_deref(), &opts)
            .map_err(|e| e.to_string())?;
        let scores: Vec<f64> = hits.iter().map(|hit| hit.score).collect();
        let strong = digup_core::scoring::strong_count(&scores, 0.5) as i64;
        shown += strong;
        listed += hits.len() as i64;
        // Paths relative to `base`; "path@6-12" also requires the hit to start in [6, 12).
        let targets: Vec<(String, Option<(f64, f64)>)> = query
            .get("expect")
            .and_then(|e| e.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        let entry = entry.as_str().unwrap_or("");
                        let mut parts = entry.splitn(2, '@');
                        let head = parts.next().unwrap_or("");
                        let range = parts.next().and_then(|tail| {
                            let numbers: Vec<f64> =
                                tail.split('-').filter_map(|s| s.parse().ok()).collect();
                            if numbers.len() == 2 {
                                Some((numbers[0], numbers[1]))
                            } else {
                                None
                            }
                        });
                        (format!("{base}/{head}"), range)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // A place to be at: a page or moment starts in the range; a stretch of
        // code overlaps it (its lines or cells).
        let rank = hits
            .iter()
            .position(|hit| {
                targets.iter().any(|(path, range)| {
                    hit.path == *path
                        && match range {
                            Some((low, high)) => match hit.segment {
                                SegmentKind::Lines | SegmentKind::Cells => hit
                                    .loc
                                    .map(|loc| loc <= *high && hit.loc_end.unwrap_or(loc) >= *low)
                                    .unwrap_or(false),
                                _ => hit
                                    .loc
                                    .map(|loc| loc >= *low && loc <= *high)
                                    .unwrap_or(false),
                            },
                            None => true,
                        }
                })
            })
            .map(|found| found + 1);
        if let Some(rank) = rank {
            if (rank as i64) > strong {
                folded += 1;
            }
        }
        let tag = query
            .get("tag")
            .and_then(|t| t.as_str())
            .unwrap_or("all")
            .to_string();
        for key in [tag.clone(), "ALL".to_string()] {
            let existing = by_tag.iter_mut().find(|(k, _)| *k == key);
            match existing {
                Some((_, row)) => {
                    row.n += 1;
                    if let Some(rank) = rank {
                        if rank <= 1 {
                            row.at1 += 1;
                        }
                        if rank <= 5 {
                            row.at5 += 1;
                        }
                        if rank <= 10 {
                            row.at10 += 1;
                        }
                        row.mrr += 1.0 / rank as f64;
                    }
                }
                None => {
                    let mut row = Row {
                        n: 1,
                        ..Default::default()
                    };
                    if let Some(rank) = rank {
                        if rank <= 1 {
                            row.at1 = 1;
                        }
                        if rank <= 5 {
                            row.at5 = 1;
                        }
                        if rank <= 10 {
                            row.at10 = 1;
                        }
                        row.mrr = 1.0 / rank as f64;
                    }
                    by_tag.push((key, row));
                }
            }
        }
        let top = hits
            .first()
            .map(|hit| {
                let name = Path::new(&hit.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| hit.path.clone());
                let place = location(hit);
                // main.swift:674 — `"\(($0.path as NSString).lastPathComponent)"`: the
                // parens around Swift's interpolation are syntax, not characters, so the
                // name prints bare (measured on this box: `top: seg_6s.mp4 [at 0:00]`).
                format!(
                    "{}{}",
                    name,
                    if place.is_empty() {
                        String::new()
                    } else {
                        format!(" [{place}]")
                    }
                )
            })
            .unwrap_or_else(|| "-".to_string());
        let rank_field = rank
            .map(|rank| formats::lpad(&format!("#{rank}"), 4))
            .unwrap_or_else(|| "miss".to_string());
        let qtext = query.get("q").and_then(|q| q.as_str()).unwrap_or("");
        println!(
            "{}  {} {} top: {}",
            rank_field,
            formats::pad(&tag, 11),
            formats::pad(&format!("“{qtext}”"), 42),
            top
        );
    }
    println!();
    println!(
        "{} {}  {}  {}  {}  {}",
        formats::pad("tag", 12),
        formats::lpad("n", 3),
        formats::lpad("R@1", 5),
        formats::lpad("R@5", 5),
        formats::lpad("R@10", 5),
        formats::lpad("MRR", 5)
    );
    by_tag.sort_by_key(|(key, _)| if key == "ALL" { "~" } else { key.as_str() }.to_string());
    for (tag, row) in &by_tag {
        let n = row.n as f64;
        println!(
            "{} {}  {:5.2}  {:5.2}  {:5.2}  {:5.2}",
            formats::pad(tag, 12),
            formats::lpad(&row.n.to_string(), 3),
            row.at1 as f64 / n,
            row.at5 as f64 / n,
            row.at10 as f64 / n,
            row.mrr / n
        );
    }
    let count = queries.len().max(1) as f64;
    println!(
        "weaker-matches fold (below 0.5× the top score): {:.1} of {:.1} results shown per query; {} expected {} folded",
        shown as f64 / count,
        listed as f64 / count,
        folded,
        if folded == 1 { "answer" } else { "answers" }
    );
    Ok(())
}

/// `emit` of DownloadModel.swift: the JSON lines, keys in the order each event lists them.
/// The line itself is services' `emit` (the one writer: sorted keys straight to fd 1).
fn emit_event(event: &str, fields: serde_json::Map<String, serde_json::Value>) {
    let mut row = serde_json::Map::new();
    row.insert(
        "event".to_string(),
        serde_json::Value::String(event.to_string()),
    );
    for (key, value) in fields {
        row.insert(key, value);
    }
    emit::emit(&serde_json::Value::Object(row));
}

/// `json_object` / `json_str` / `json_int`: the `download-model` event rows' builders —
/// they assemble the `Map` (the JSON values), they never spell a line. `core/jsonline.rs`
/// is the authoritative emitter (Foundation's `.sortedKeys` bytes) and `services::emit` is
/// the one writer; these three are used only by `download_model`'s progress/finish events.
fn json_object<I: IntoIterator<Item = (String, serde_json::Value)>>(
    items: I,
) -> serde_json::Map<String, serde_json::Value> {
    items.into_iter().collect()
}

fn json_str(value: &str) -> serde_json::Value {
    serde_json::Value::String(value.to_string())
}

fn json_int(value: i64) -> serde_json::Value {
    serde_json::json!(value)
}

/// `downloadModel(_:)` (DownloadModel.swift) + its `FileFetcher` (FileFetcher.swift),
/// with `curl` as the transport (every OS ships it, so the port adds no HTTP stack):
/// the events, the resume (curl's `-C -` sends the Range and keeps it across the
/// redirect, as FileFetcher does), the ≤5-a-second progress with one final event
/// when the transfer ends, and the sha256 gate before a file gets its name.
fn download_model(args: &Arguments) -> i32 {
    use digup_runtime::models::ModelFiles;
    use std::time::Duration;

    let folder = match args.value("--to") {
        Some(dir) => folder_url(dir),
        None => ModelFiles::installed(),
    };
    if let Err(e) = std::fs::create_dir_all(&folder) {
        eprintln!("error: {e}");
        return 1;
    }
    let missing = ModelFiles::missing(&folder);
    let mut done = ModelFiles::total_bytes() - missing.iter().map(|file| file.bytes).sum::<i64>();
    for file in missing {
        let final_path = folder.join(file.name);
        // A checksum-matched file in the Hugging Face cache is copied, not fetched.
        let cached_ok = ModelFiles::cached_copy(file)
            .map(|cached| {
                ModelFiles::sha256(&cached)
                    .map(|hash| hash == file.sha256)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if cached_ok {
            emit_event(
                "file",
                json_object([
                    ("name".to_string(), json_str(file.name)),
                    ("bytes".to_string(), json_int(file.bytes)),
                    ("from".to_string(), json_str("cache")),
                ]),
            );
            let partial = folder.join(format!("{}.partial", file.name));
            let _ = std::fs::remove_file(&partial);
            let cached = ModelFiles::cached_copy(file).expect("the cache copy, checked just now");
            if let Err(e) = std::fs::copy(&cached, &partial)
                .and_then(|_| std::fs::rename(&partial, &final_path))
            {
                eprintln!("error: {e}");
                return 1;
            }
            done += file.bytes;
            continue;
        }
        emit_event(
            "file",
            json_object([
                ("name".to_string(), json_str(file.name)),
                ("bytes".to_string(), json_int(file.bytes)),
                ("from".to_string(), json_str("download")),
            ]),
        );
        let mut failures = 0i32;
        loop {
            match fetch(file, &folder, done) {
                Outcome::Downloaded(partial) => {
                    emit_event(
                        "verifying",
                        json_object([("name".to_string(), json_str(file.name))]),
                    );
                    let hash = match ModelFiles::sha256(&partial) {
                        Ok(hash) => hash,
                        Err(e) => {
                            eprintln!("error: {e}");
                            return 1;
                        }
                    };
                    if hash != file.sha256 {
                        let _ = std::fs::remove_file(&partial);
                        failures += 1;
                        if failures >= 3 {
                            emit_event(
                                "error",
                                json_object([(
                                    "message".to_string(),
                                    json_str(&format!(
                                        "{} arrived damaged, three times",
                                        file.name
                                    )),
                                )]),
                            );
                            return 1;
                        }
                        emit_event(
                            "retry",
                            json_object([
                                (
                                    "message".to_string(),
                                    json_str(&format!("{} arrived damaged", file.name)),
                                ),
                                ("seconds".to_string(), json_int(0)),
                            ]),
                        );
                        continue;
                    }
                    if let Err(e) = std::fs::rename(&partial, &final_path) {
                        eprintln!("error: {e}");
                        return 1;
                    }
                    done += file.bytes;
                }
                Outcome::Failed(message, retry) => {
                    failures += 1;
                    if !retry || failures > 20 {
                        emit_event(
                            "error",
                            json_object([("message".to_string(), json_str(&message))]),
                        );
                        return 1;
                    }
                    // A flaky link resumes where it stopped: 2, 4, 8 … seconds apart, a minute at most.
                    let seconds = 60i32.min(1 << failures.min(6));
                    emit_event(
                        "retry",
                        json_object([
                            ("message".to_string(), json_str(&message)),
                            ("seconds".to_string(), json_int(seconds as i64)),
                        ]),
                    );
                    std::thread::sleep(Duration::from_secs(seconds as u64));
                }
            }
            break;
        }
    }
    emit_event(
        "done",
        json_object([("folder".to_string(), json_str(&folder.to_string_lossy()))]),
    );
    0
}

enum Outcome {
    Downloaded(PathBuf),
    Failed(String, bool),
}

/// One try at one file (FileFetcher.swift's `start` + delegate): the file lands on
/// disk as `<name>.partial`, curl resumes it with a Range, the progress events are
/// read off the file at FileFetcher's pace (≥0.2 s apart), and one last event tells
/// how far it got when the transfer ends — as Swift's `didCompleteWithError` does.
fn fetch(file: &digup_runtime::models::ModelFile, folder: &Path, before: i64) -> Outcome {
    use std::time::{Duration, Instant};

    let partial = folder.join(format!("{}.partial", file.name));
    let mut written = std::fs::metadata(&partial)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    if written > file.bytes {
        let _ = std::fs::remove_file(&partial);
        written = 0;
    }
    if written == file.bytes {
        // The file is already complete on disk (a previous run stopped after the last
        // byte): FileFetcher reports it downloaded without making a request.
        return Outcome::Downloaded(partial);
    }
    // curl writes into the partial; `-C -` resumes from its size (the Range survives
    // Hugging Face's redirect to its CDN, which `-L` follows); `-f` fails on 4xx/5xx,
    // and `--speed-limit/--speed-time` mirror the 60 s "no bytes" request timeout.
    let mut child = match Command::new("curl")
        .arg("-sS")
        .arg("-L")
        .arg("-f")
        .arg("-C")
        .arg("-")
        .arg("--speed-limit")
        .arg("1")
        .arg("--speed-time")
        .arg("60")
        .arg("-o")
        .arg(&partial)
        .arg("-w")
        .arg("%{http_code}")
        .arg(digup_runtime::models::ModelFiles::url(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Outcome::Failed(format!("curl did not start: {e}"), true),
    };
    let mut last_report = Instant::now() - Duration::from_millis(200);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let written = std::fs::metadata(&partial)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        if last_report.elapsed() >= Duration::from_millis(200) {
            last_report = Instant::now();
            emit_event(
                "progress",
                json_object([
                    ("name".to_string(), json_str(file.name)),
                    ("bytes".to_string(), json_int(written)),
                    ("total".to_string(), json_int(file.bytes)),
                    ("all_bytes".to_string(), json_int(before + written)),
                    (
                        "all_total".to_string(),
                        json_int(digup_runtime::models::ModelFiles::total_bytes()),
                    ),
                ]),
            );
        }
        if child
            .try_wait()
            .expect("curl is here, it can be waited for")
            .is_some()
        {
            break;
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(e) => return Outcome::Failed(format!("curl did not end: {e}"), true),
    };
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let written = std::fs::metadata(&partial)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    // The one event that always lands at the end (FileFetcher's final `progress`).
    emit_event(
        "progress",
        json_object([
            ("name".to_string(), json_str(file.name)),
            ("bytes".to_string(), json_int(written)),
            ("total".to_string(), json_int(file.bytes)),
            ("all_bytes".to_string(), json_int(before + written)),
            (
                "all_total".to_string(),
                json_int(digup_runtime::models::ModelFiles::total_bytes()),
            ),
        ]),
    );
    match output.status.code() {
        Some(0) => {
            if written == file.bytes {
                Outcome::Downloaded(partial)
            } else {
                Outcome::Failed("The download stopped early".to_string(), true)
            }
        }
        Some(22) => {
            // `-f` answered a 4xx/5xx: the status decides retry, as FileFetcher's
            // response handler does (429 and the 5xx family are flaky links).
            let code: i64 = status.parse().unwrap_or(0);
            Outcome::Failed(
                format!("Hugging Face answered {code}"),
                code == 429 || code >= 500,
            )
        }
        Some(23) => Outcome::Failed(
            "Can't save the model: the disk write stopped".to_string(),
            false,
        ),
        Some(28) => Outcome::Failed(
            "The download stopped: no bytes for a minute".to_string(),
            true,
        ),
        Some(code) => Outcome::Failed(
            format!(
                "curl exited {code}{tail}",
                tail = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            ),
            true,
        ),
        None => Outcome::Failed("the download stopped".to_string(), true),
    }
}

#[cfg(test)]
mod tests {
    use super::byte_count;
    use super::parse;

    /// `Arguments.store()` (main.swift:101-103): `--code` names `code.sqlite` — the store a
    /// code run answers its fingerprint to. The port opened `index.sqlite` for `index --code`
    /// and `status --code`, so a code run into a folder that already holds a media index
    /// refused its own index on the media fingerprint ("index: … photo140 shot280 … now: …
    /// code1 lines1800", exit 1) where the reference answers exit 0 with its rows in
    /// `code.sqlite`. Measured 2026-11-13 by `scripts/rust/twin-gate.sh`, which indexes main
    /// then code into one store; the twin corpus is small, so this verb is not otherwise
    /// covered by the pinned suites (they pass each store explicitly through the library).
    #[test]
    fn the_code_flag_names_the_code_store() {
        let store_of = |argv: &[&str]| {
            let owned: Vec<String> = argv.iter().map(|arg| arg.to_string()).collect();
            parse(&owned).map(|args| args.store_file())
        };
        assert_eq!(
            store_of(&["index", "some-folder"]),
            Ok(digup_core::MAIN_FILE)
        );
        assert_eq!(
            store_of(&["index", "some-folder", "--code"]),
            Ok(digup_core::CODE_FILE)
        );
        assert_eq!(
            store_of(&["status", "--index", "/tmp/idx", "--code"]),
            Ok(digup_core::CODE_FILE)
        );
        assert_eq!(
            store_of(&["status", "--index", "/tmp/idx"]),
            Ok(digup_core::MAIN_FILE)
        );
    }

    /// The number shapes are pinned from probing Swift 6.4's `JSONSerialization`
    /// writer on this box (jsonprobe3/4.out): shortest digits, exponent form when
    /// the leading power is < -4 or ≥ 16, fixed form bare-integral, "-0" for the
    /// signed zero.
    #[test]
    fn the_cli_paths_are_the_url_standardized_shape() {
        // F-13 residual (a): main.swift:94-95 (`--index`) and 127-128
        // (the roots) run `URL(fileURLWithPath:
        // expandingTildeInPath).standardizedFileURL`. The standardize is
        // the port's measured `paths::standardize`; `URL(fileURLWithPath:)`
        // absolutizes a relative argument against the CWD. The audit-base
        // ran `expand_tilde` alone on `--index` — the relayed `index` string
        // carried the caller's `..`, doubled slashes and trailing slash.
        use super::{expand_tilde, home, url_path, PathBuf};
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(url_path("/data/idx/"), PathBuf::from("/data/idx"));
        assert_eq!(url_path("/data//idx"), PathBuf::from("/data/idx"));
        assert_eq!(url_path("/data/x/../idx/."), PathBuf::from("/data/idx"));
        assert_eq!(url_path("/.."), PathBuf::from("/")); // surplus .. at the root drops
        assert_eq!(url_path("/a/b/../.."), PathBuf::from("/"));
        assert_eq!(url_path("rel"), cwd.join("rel")); // URL(fileURLWithPath:) absolutizes
        assert_eq!(url_path("./rel"), cwd.join("rel"));
        // The pops are lexical and climb ABOVE the CWD: standardizedFileURL
        // removes the pair a/.. then the pair cli/.., so a/../../rel from
        // crates/cli is crates/rel — what URL.standardizedFileURL answers.
        assert_eq!(url_path("a/../../rel"), cwd.parent().unwrap().join("rel"));
        assert_eq!(url_path("~/x/"), home().join("x")); // the tilde runs FIRST
        assert_eq!(url_path("~"), home());
        // The audit-base shape, recorded inline (the fails-before): the
        // `--index` string passed expand_tilde untouched — verbatim garbage.
        assert_eq!(
            expand_tilde("/data/idx/../idx/").to_string_lossy(),
            "/data/idx/../idx/"
        );
        assert_eq!(expand_tilde("rel").to_string_lossy(), "rel");
        // And the relayed fact rides index_folder:
        let owned: Vec<String> = ["status", "--index", "/data/idx/../idx/"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        let args = parse(&owned).unwrap();
        assert_eq!(args.index_folder(), PathBuf::from("/data/idx"));
    }

    #[test]
    fn swift_number_shapes() {
        let cases: [(f64, &str); 16] = [
            (0.0, "0"),
            (-0.0, "-0"),
            (3.0, "3"),
            (2.5, "2.5"),
            (0.1, "0.1"),
            (1760000000.5, "1760000000.5"),
            (0.9708202862739563, "0.9708202862739563"),
            (1e15, "1000000000000000"),
            (1e16, "1e+16"),
            (1e17, "1e+17"),
            (1e18, "1e+18"),
            (1e21, "1e+21"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (9.9e-5, "9.9e-05"),
            (2.5e-8, "2.5e-08"),
        ];
        for (value, want) in cases {
            let number = serde_json::Number::from_f64(value).expect("every case is a finite f64");
            let got = super::swift_number(&number);
            assert_eq!(got, want, "{value}");
        }
        // Swift's writer's string escapes and container shapes (jsonprobe.out / 2.out).
        let rows = vec![serde_json::json!({
            "path": "/media/a/b.jpg",
            "info": {},
            "excerpt": "“café” — বাংলা \n tab\\ end\u{01}\u{1c}\u{7f}",
            "n": 1.0,
            "neg": -0.0,
        })];
        let want = "[\n  {\n    \"excerpt\" : \"“café” — বাংলা \\n tab\\\\ end\\u0001\\u001C\u{7f}\",\n    \"info\" : {\n\n    },\n    \"n\" : 1,\n    \"neg\" : -0,\n    \"path\" : \"\\/media\\/a\\/b.jpg\"\n  }\n]";
        assert_eq!(super::swift_json(&rows), want);
        // Empty top-level answers print "[", a blank line, "]" (jsonprobe.out).
        assert_eq!(super::swift_json(&[]), "[\n\n]");
    }

    /// `ByteCountFormatter(countStyle: .file)`, pinned by probing the formatter (Swift 6.4,
    /// corelibs Foundation) on these exact byte counts.
    #[test]
    fn byte_counts_match_the_formatter() {
        let cases: [(i64, &str); 34] = [
            (1, "1 byte"),
            (999, "999 bytes"),
            (1000, "1 KB"),
            (1049, "1 KB"),
            (1050, "1 KB"),
            (1150, "1 KB"),
            (1449, "1 KB"),
            (1450, "1 KB"),
            (1949, "2 KB"),
            (1950, "2 KB"),
            (4100, "4 KB"),
            (9999, "10 KB"),
            (10450, "10 KB"),
            (10500, "10 KB"),
            (10999, "11 KB"),
            (11000, "11 KB"),
            (11500, "12 KB"),
            (12300, "12 KB"),
            (41000, "41 KB"),
            (99999, "100 KB"),
            (105000, "105 KB"),
            (123000, "123 KB"),
            (999999, "1,000 KB"),
            (1000000, "1 MB"),
            (1234567, "1.2 MB"),
            (4100000, "4.1 MB"),
            (99999999, "100 MB"),
            (104999999, "105 MB"),
            (123456789, "123.5 MB"),
            (1049999999, "1.05 GB"),
            (1234567890, "1.23 GB"),
            (999999999999, "1,000 GB"),
            (1049999999999, "1.05 TB"),
            (1234567890123, "1.23 TB"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(byte_count(bytes), expected, "{bytes}");
        }
    }
}
