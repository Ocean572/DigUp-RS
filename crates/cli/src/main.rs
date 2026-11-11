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

/// `Arguments` (main.swift): `--flag value` pairs, bare `--flags`, positionals.
struct Arguments {
    flags: Vec<String>,
    values: BTreeMap<String, String>,
    positional: Vec<String>,
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse(&argv);
    match args.positional.first().map(String::as_str) {
        Some("speed") => {
            if let Err(e) = speed(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        _ => selftest(&args),
    }
}

fn parse(argv: &[String]) -> Arguments {
    let mut flags = Vec::new();
    let mut values = BTreeMap::new();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let argument = &argv[index];
        if argument.starts_with("--")
            && index + 1 < argv.len()
            && !argv[index + 1].starts_with("--")
        {
            values.insert(argument.clone(), argv[index + 1].clone());
            index += 2;
        } else if argument.starts_with("--") || argument == "-h" {
            flags.push(argument.clone());
            index += 1;
        } else {
            positional.push(argument.clone());
            index += 1;
        }
    }
    Arguments {
        flags,
        values,
        positional,
    }
}

impl Arguments {
    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
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

/// Compact JSON on one line (`emit`).
fn emit(fields: serde_json::Map<String, serde_json::Value>) {
    println!("{}", serde_json::to_string(&fields).unwrap());
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
