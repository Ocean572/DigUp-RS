//! `digup worker` (`workerService`, WorkerService.swift:8): EmbeddingGemma 2 on
//! llama.cpp as a process, speaking the JSON-lines protocol of
//! `worker/digup_worker.py` (see `EmbeddingWorker`). The app runs one as its query
//! encoder, so the model's memory goes back to the system when it exits. llama.cpp's
//! warnings go to stderr.
//!
//! Protocol, one JSON object per line in, one out carrying the request's `id`:
//!   out: {"ready":true,"info":{model,revision,runtime,dtype,dim,text_only,load_seconds}}
//!   in:  {"op":"text|image|audio","inputs":[…],"budget":n} → {"id":…,"ok":true,"dim":n,
//!        "count":n,"vectors":<base64 float32 LE, one row per input>,"errors":[…|null]}
//!   in:  {"op":"info"} → {"id":…,"ok":true,"info":{…}};  {"op":"quit"} → ok, then exit.

use base64::Engine as _;
use serde_json::{Map, Value};

use digup_runtime::models::LlamaModels;
use digup_runtime::worker::{Embedder, Embedding};
use digup_runtime::LlamaEmbedder;

use crate::emit::emit;

/// Swift's `request["inputs"] as? [String] ?? []` (WorkerService.swift:26) —
/// the cast is ALL-OR-NOTHING over the array: a string array is kept as it
/// stands; ANY non-string element (a number, a bool, a null, a nested array)
/// fails the whole cast and the request answers with `[]`; a non-array
/// (a string, an object, a null, a missing key) is the same refusal.
/// Measured on swift-corelibs 6.4 (2026-11-15, f7cast.swift):
/// `["a",7,"b"]`->nil, `["a",true]`->nil, `["a",null]`->nil, `"a"`->nil,
/// `{"a":"b"}`->nil; `["a","b"]`->kept, `[]`->kept. The empty answer runs
/// the op on zero inputs: `ok:true` with `count:0`, `vectors:""`, `errors:[]`
/// (Swift's `embedTexts` ends `if sequences > 0` — no batch for no inputs).
/// The audit-base ran `filter_map` — per-element: `["a",7,"b"]` embedded
/// `a` and `b` (count:2) where the engine embeds NOTHING (count:0).
fn swift_cast_inputs(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| row.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Swift's `request["budget"] as? Int` (WorkerService.swift:40) — the
/// NSNumber bridge of swift-corelibs, measured 2026-11-15
/// (f7cast.swift + f7cast2.swift): integers pass; WHOLE-VALUED doubles
/// bridge (`280.0`->280, `140.0`->140, `-3.0`->-3, `1e18`->1e18);
/// fractional doubles refuse (`2.5`, `140.5`); booleans bridge (`true`->1,
/// `false`->0); strings and nulls refuse; values outside the Int64 range
/// refuse (`1e19`, `2^63`; `2^63-1` passes). `?? 280` supplies the default
/// for a refusal. The audit-base ran serde's `as_i64`, which refuses every
/// JSON float and every bool: `140.0` defaulted to 280 where the engine
/// embeds at 140 (the vectors differ — `ok:true` both sides, different
/// bytes), and `true` defaulted to a silent 280-embed where the engine
/// bridges 1, `budget must be one of [70, 140, 280, 560, 1120]` refuses
/// and the reply is `ok:false`.
fn swift_cast_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                return Some(int);
            }
            let float = number.as_f64()?;
            if float.fract() == 0.0
                && (-9_223_372_036_854_775_808_f64..9_223_372_036_854_775_808_f64).contains(&float)
            {
                Some(float as i64)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `workerService(_:)`. `text_only`: `--text-only` — queries only, no media encoders.
pub fn worker_service(text_only: bool) -> i32 {
    let embedder = match LlamaModels::locate()
        .and_then(|models| LlamaEmbedder::new(&models, text_only, None, None))
    {
        Ok(embedder) => embedder,
        Err(error) => {
            emit(&serde_json::json!({"ready": false, "error": error.to_string()}));
            return 1;
        }
    };
    let info_fields = info_fields(embedder.info());
    emit(&serde_json::json!({"ready": true, "info": info_fields}));
    let stdin = std::io::stdin();
    for line in stdin.lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue; // `try? JSONSerialization … as? [String: Any]`
        };
        let Value::Object(request) = request else {
            continue;
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let inputs = swift_cast_inputs(request.get("inputs"));
        let reply = handle(&embedder, &request, &inputs, &id, &info_fields);
        match reply {
            Ok(reply) => emit(&reply),
            Err(error) => {
                let mut out = Map::new();
                out.insert("id".into(), id.clone());
                out.insert("ok".into(), Value::Bool(false));
                out.insert(
                    "error".into(),
                    Value::String(error.chars().take(500).collect()),
                );
                emit(&Value::Object(out));
            }
        }
        if request.get("op").and_then(Value::as_str) == Some("quit") {
            let mut embedder = embedder;
            embedder.close();
            return 0;
        }
    }
    let mut embedder = embedder;
    embedder.close();
    0
}

/// One request's reply, or the error that answers `ok:false` — the Swift `do/catch`
/// of the loop body.
fn handle(
    embedder: &LlamaEmbedder,
    request: &Map<String, Value>,
    inputs: &[String],
    id: &Value,
    info_fields: &Value,
) -> Result<Value, String> {
    let mut out = Map::new();
    match request.get("op").and_then(Value::as_str) {
        Some("quit") => {
            out.insert("id".into(), id.clone());
            out.insert("ok".into(), Value::Bool(true));
            Ok(Value::Object(out))
        }
        Some("info") => {
            out.insert("id".into(), id.clone());
            out.insert("ok".into(), Value::Bool(true));
            out.insert("info".into(), info_fields.clone());
            Ok(Value::Object(out))
        }
        Some(op @ ("text" | "image" | "audio")) => {
            let embeddings: Vec<Embedding> = match op {
                "text" => embedder
                    .embed_texts(inputs)
                    .map(|rows| rows.into_iter().map(Embedding::made).collect())
                    .map_err(|e| e.to_string())?,
                "image" => {
                    let budget = request
                        .get("budget")
                        .and_then(swift_cast_int)
                        .unwrap_or(280) as i32;
                    embedder
                        .embed_images(inputs, budget)
                        .map_err(|e| e.to_string())?
                }
                _ => embedder.embed_audio(inputs).map_err(|e| e.to_string())?,
            };
            // Float32 little-endian, one row per input; an input that failed gets
            // zeros and a message.
            let dim = embedder.info().dim;
            let mut floats: Vec<u8> = Vec::with_capacity(embeddings.len() * dim * 4);
            let mut errors: Vec<Value> = Vec::with_capacity(embeddings.len());
            for embedding in &embeddings {
                match &embedding.vector {
                    Some(vector) => {
                        for value in vector {
                            floats.extend_from_slice(&value.to_le_bytes());
                        }
                    }
                    None => floats.extend_from_slice(&[0u8; 4].repeat(dim)),
                }
                errors.push(
                    embedding
                        .error
                        .as_ref()
                        .map(|e| Value::String(e.clone()))
                        .unwrap_or(Value::Null),
                );
            }
            // Swift fills `$0.vector ?? zeros(dim)`: a vector-less row is zeros; the
            // engine's `Embedding` carries both, and the `text` map above never
            // leaves one vector-less with the error unset.
            out.insert("id".into(), id.clone());
            out.insert("ok".into(), Value::Bool(true));
            out.insert("dim".into(), serde_json::json!(dim));
            out.insert("count".into(), serde_json::json!(inputs.len()));
            out.insert(
                "vectors".into(),
                Value::String(base64::engine::general_purpose::STANDARD.encode(&floats)),
            );
            out.insert("errors".into(), Value::Array(errors));
            Ok(Value::Object(out))
        }
        op => Err(format!("unknown op {}", op.unwrap_or("(none)"))),
    }
}

/// `infoFields`: the shape the worker reports, `load_seconds ?? 0` spelled as a
/// number in both cases.
fn info_fields(info: &digup_runtime::worker::WorkerInfo) -> Value {
    serde_json::json!({
        "model": info.model,
        "revision": info.revision,
        "runtime": info.runtime,
        "dtype": info.dtype,
        "dim": info.dim,
        "text_only": info.text_only,
        "load_seconds": info.load_seconds.unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::{swift_cast_inputs, swift_cast_int};
    use serde_json::json;

    #[test]
    fn inputs_are_the_all_or_nothing_string_cast() {
        // Every row is a measurement of swift-corelibs 6.4 (2026-11-15).
        assert_eq!(swift_cast_inputs(Some(&json!(["a", "b"]))), vec!["a", "b"]);
        assert!(swift_cast_inputs(Some(&json!([]))).is_empty());
        // The fails-before (recorded by `the_audit_base_casts_inputs_per_element`):
        // the engine answers NOTHING for a mixed array — count:0, vectors:"" —
        // where the audit-base's per-element filter_map embedded "a" and "b".
        assert!(swift_cast_inputs(Some(&json!(["a", 7, "b"]))).is_empty());
        assert!(swift_cast_inputs(Some(&json!(["a", true]))).is_empty());
        assert!(swift_cast_inputs(Some(&json!(["a", null]))).is_empty());
        assert!(swift_cast_inputs(Some(&json!(["a", ["b"]]))).is_empty());
        assert!(swift_cast_inputs(Some(&json!("a"))).is_empty());
        assert!(swift_cast_inputs(Some(&json!({"a": "b"}))).is_empty());
        assert!(swift_cast_inputs(None).is_empty());
    }

    #[test]
    fn budget_is_the_measured_number_bridge_with_the_280_default() {
        assert_eq!(swift_cast_int(&json!(280)), Some(280));
        assert_eq!(swift_cast_int(&json!(280.0)), Some(280));
        assert_eq!(swift_cast_int(&json!(140.0)), Some(140)); // fails-before: 280 (audit-base)
        assert_eq!(swift_cast_int(&json!(-3)), Some(-3));
        assert_eq!(swift_cast_int(&json!(-3.0)), Some(-3));
        assert_eq!(
            swift_cast_int(&json!(1e18)),
            Some(1_000_000_000_000_000_000)
        );
        assert_eq!(swift_cast_int(&json!(true)), Some(1)); // fails-before: 280 (audit-base)
        assert_eq!(swift_cast_int(&json!(false)), Some(0));
        assert_eq!(
            swift_cast_int(&json!(9223372036854775807i64)),
            Some(9223372036854775807)
        );
        assert_eq!(swift_cast_int(&json!(2.5)), None);
        assert_eq!(swift_cast_int(&json!(140.5)), None);
        assert_eq!(swift_cast_int(&json!(1e19)), None);
        assert_eq!(swift_cast_int(&json!(9223372036854775808u64)), None);
        assert_eq!(swift_cast_int(&json!("280")), None);
        assert_eq!(swift_cast_int(&json!(null)), None);
    }

    #[test]
    fn the_audit_base_casts_inputs_per_element_and_defaults_the_bridge() {
        // F-07's fails-before, recorded at the fixed tip: the audit-base
        // expressions verbatim. `filter_map` KEEPS the string elements of a
        // mixed array (a 2-input embed where the engine embeds nothing), and
        // serde's `as_i64` refuses whole-valued doubles and bools, so
        // `unwrap_or(280)` silently embeds at 280 where the engine embeds at
        // 140 (`140.0`) or refuses the whole request (`true` -> 1 ->
        // `budget must be one of [70, 140, 280, 560, 1120]` -> ok:false).
        let rows = json!(["a", 7, "b"]);
        let audit_inputs: Vec<String> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(audit_inputs, vec!["a", "b"]); // the engine's answer is []
        for budget in [json!(140.0), json!(true)] {
            assert_eq!(
                budget.as_i64().unwrap_or(280),
                280, // the audit-base: a silent 280-embed on BOTH
            );
        }
        assert_eq!(swift_cast_int(&json!(140.0)), Some(140)); // the engine: embed at 140
        assert_eq!(swift_cast_int(&json!(true)), Some(1)); // the engine: refuse (1 is no budget)
    }
}
