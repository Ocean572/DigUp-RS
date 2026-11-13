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
        let inputs: Vec<String> = request
            .get("inputs")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
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
            return Ok(Value::Object(out));
        }
        Some("info") => {
            out.insert("id".into(), id.clone());
            out.insert("ok".into(), Value::Bool(true));
            out.insert("info".into(), info_fields.clone());
            return Ok(Value::Object(out));
        }
        Some(op @ ("text" | "image" | "audio")) => {
            let embeddings: Vec<Embedding> = match op {
                "text" => embedder
                    .embed_texts(inputs)
                    .map(|rows| rows.into_iter().map(Embedding::made).collect())
                    .map_err(|e| e.to_string())?,
                "image" => {
                    let budget =
                        request.get("budget").and_then(Value::as_i64).unwrap_or(280) as i32;
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
