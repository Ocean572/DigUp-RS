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
use digup_services::speed as modelspeed;
use digup_services::system_load;

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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    // Where this machine's speed check found its processor faster than its GPU, the model
    // runs on the processor (main.swift:725).
    if args.positional.first().map(String::as_str) != Some("speed") {
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
    match args.positional.first().map(String::as_str) {
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
        _ => selftest(&args),
    }
}

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

    /// `Arguments.indexFolder`: `--index` (with `~`), else the platform's default
    /// (`AppPaths.defaultIndex`: `$XDG_DATA_HOME/digup/index`, or `~/.local/share/digup/index`).
    fn index_folder(&self) -> PathBuf {
        if let Some(chosen) = self.value("--index") {
            return expand_tilde(chosen);
        }
        let base = match std::env::var("XDG_DATA_HOME") {
            Ok(value) if !value.is_empty() => PathBuf::from(value),
            _ => std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".local/share"),
        };
        base.join("digup").join("index")
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

/// `tilde(_:)`: a path under the home shown as `~/…`.
fn tilde(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    let home = home().to_string_lossy().into_owned();
    if text.starts_with(&home) {
        "~".to_string() + &text[home.len()..]
    } else {
        text
    }
}

/// Compact JSON on one line (`emit`, IndexService.swift:519): Foundation's `JSONSerialization`
/// with `.sortedKeys`, written straight to the descriptor. `jsonline` reproduces its bytes.
fn emit(fields: serde_json::Map<String, serde_json::Value>) {
    if let Some(line) = digup_core::jsonline::to_line(&serde_json::Value::Object(fields)) {
        println!("{line}");
    }
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
    let line = format!(
        "measured by indexing ({}):",
        tilde(&MeasuredCosts::file(&folder))
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
        .env("DIGUP_DEVICE", "cpu")
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
    let log = folder.join("worker.log");
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
        emit(fields);
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
                    emit(fields);
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
        emit(fields);
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
    println!(
        "kept in {}: {}",
        tilde(&MeasuredCosts::file(&folder)),
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
                emit(fields);
            } else {
                println!(
                    "{}    can't read it (not there, or not allowed in macOS privacy settings)",
                    tilde(&folder)
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
            emit(fields);
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
                    number(count),
                    number(estimate.pdf_pages_total),
                    number(estimate.pdf_pages)
                )),
                digup_core::model::FileKind::Audio => kinds.push(format!(
                    "{} audio ({})",
                    number(count),
                    duration(estimate.audio_seconds)
                )),
                digup_core::model::FileKind::Video => kinds.push(format!(
                    "{} video ({})",
                    number(count),
                    duration(estimate.video_seconds)
                )),
                digup_core::model::FileKind::Code => kinds.push(format!(
                    "{} code {} ({} stretches)",
                    number(count),
                    if count == 1 { "file" } else { "files" },
                    number(estimate.code_stretches)
                )),
                _ => kinds.push(format!(
                    "{} {}{}",
                    number(count),
                    kind.as_str(),
                    if count == 1 { "" } else { "s" }
                )),
            }
        }
        println!(
            "{}    first index ≈ {}{}",
            tilde(&folder),
            duration(estimate.seconds),
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
                number(estimate.long_files),
                if estimate.long_files == 1 {
                    "file"
                } else {
                    "files"
                },
                duration(estimate.later_seconds)
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
                        duration(value.seconds)
                    ))
                    .collect::<Vec<_>>()
                    .join(" · "),
                if subfolders.len() > 5 {
                    format!(" · {} more", number((subfolders.len() - 5) as i64))
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
                tilde(std::path::Path::new(&dataset_folder)),
                number(count)
            );
        }
        println!(
            "  (looked at it in {:.2} s)",
            started.elapsed().as_secs_f64()
        );
    }
    if args.positional.len() > 2 && !json {
        println!("all together ≈ {}", duration(total));
    }
    Ok(())
}

/// `Arguments.folderURL`: `~` expanded, standardized.
fn folder_url(path: &str) -> PathBuf {
    PathBuf::from(digup_core::paths::standardize(
        &expand_tilde(path).to_string_lossy(),
    ))
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

/// `number(_:)`: `Int.formatted(.number)` — thousands groups.
fn number(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut grouped = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// `duration(_:)`: seconds shown the way the engine's lines do.
fn duration(seconds: f64) -> String {
    if seconds < 90.0 {
        format!("{} s", seconds.round() as i64)
    } else if seconds < 5400.0 {
        format!("{} min", (seconds / 60.0).round() as i64)
    } else {
        format!("{:.1} h", seconds / 3600.0)
    }
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
        .map(|(count, reason)| format!("{} {}", number(*count), reason))
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
        number(value.round_ties_even() as i64)
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
        format!("{}{}", number(head.parse::<i64>().unwrap_or(0)), tail)
    };
    let unit_name = match unit {
        0 if bytes == 1 => "byte",
        0 => "bytes",
        _ => UNITS[unit],
    };
    format!("{text} {unit_name}")
}

/// The Phase-1 self-test: open a v6 index and report it.
fn selftest(args: &Arguments) {
    use digup_core::{IndexStore, MAIN_FILE, SCHEMA_VERSION};
    let dir = args
        .positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("digup-cli-selftest"));
    match IndexStore::open(Path::new(&dir), MAIN_FILE) {
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

#[cfg(test)]
mod tests {
    use super::byte_count;

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
