//! The request models of `api.py` (`SearchRequest`, `EmbedAheadRequest`)
//! validated the way FastAPI answers them: 422 with a `detail` list whose items
//! carry `type` / `loc` / `msg` (the suite pins that shape, `test_api.py:119`).
//!
//! The rules are pydantic v2's lax mode, in field-declaration order, collecting
//! every error (FastAPI reports all of them, not the first). Coercions mirror
//! pydantic-core: numeric strings parse for ints, `"true"/"false"`-style words
//! parse for bools, ints with a fractional part do not; `extra` is pydantic's
//! default "ignore", so unknown fields are dropped, not refused.

use serde_json::{json, Value};

/// `Kind = Literal[...]` (`api.py:56`) — the engine's FileKind rawValues; the API
/// refuses anything else with a 422 because the engine would silently drop it.
pub const KINDS: [&str; 7] = ["screenshot", "image", "pdf", "doc", "audio", "video", "code"];

/// pydantic's enum msg joins the values with commas and an "or" before the last.
fn enum_msg() -> String {
    let quoted: Vec<String> = KINDS.iter().map(|kind| format!("'{kind}'")).collect();
    let (head, last) = quoted.split_at(quoted.len() - 1);
    format!("Input should be {} or {}", head.join(", "), last[0])
}

fn item(kind: &str, loc: Vec<Value>, msg: &str) -> Value {
    json!({"type": kind, "loc": loc, "msg": msg})
}

fn body_is_object(body: &Value, errors: &mut Vec<Value>) -> bool {
    if !body.is_object() {
        errors.push(item(
            "model_attributes_type",
            vec![json!("body")],
            "Input should be a valid dictionary or object to extract fields from",
        ));
        return false;
    }
    true
}

pub struct SearchReq {
    pub q: String,
    pub kinds: Option<Vec<String>>,
    pub limit: i64,
    pub code: bool,
    pub meaning: bool,
    pub prefix_last: bool,
}

pub struct EmbedAheadReq {
    pub q: String,
    pub code: bool,
}

/// `q: str = Field(max_length=512)` + `code: bool` — shared by both models.
fn check_q(body: &Value, errors: &mut Vec<Value>) -> Option<String> {
    match body.get("q") {
        None => {
            errors.push(item("missing", vec![json!("body"), json!("q")], "Field required"));
            None
        }
        Some(Value::String(text)) => {
            if text.chars().count() > 512 {
                errors.push(item(
                    "string_too_long",
                    vec![json!("body"), json!("q")],
                    "String should have at most 512 characters",
                ));
                None
            } else {
                Some(text.clone())
            }
        }
        Some(_) => {
            errors.push(item(
                "string_type",
                vec![json!("body"), json!("q")],
                "Input should be a valid string",
            ));
            None
        }
    }
}

/// `code: bool = False` (and its peers): lax bools — bools, numbers 0/1, and the
/// words pydantic-core accepts; anything else is `bool_parsing`.
fn check_bool(
    body: &Value,
    field: &str,
    default: bool,
    errors: &mut Vec<Value>,
) -> bool {
    match body.get(field) {
        None => default,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => match number.as_i64() {
            Some(0) => false,
            Some(1) => true,
            _ => {
                errors.push(item(
                    "bool_parsing",
                    vec![json!("body"), json!(field)],
                    "Input should be a valid boolean, unable to interpret input",
                ));
                default
            }
        },
        Some(Value::String(text)) => match text.to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" | "on" | "1" => true,
            "false" | "f" | "no" | "n" | "off" | "0" => false,
            _ => {
                errors.push(item(
                    "bool_parsing",
                    vec![json!("body"), json!(field)],
                    "Input should be a valid boolean, unable to interpret input",
                ));
                default
            }
        },
        Some(_) => {
            errors.push(item(
                "bool_parsing",
                vec![json!("body"), json!(field)],
                "Input should be a valid boolean, unable to interpret input",
            ));
            default
        }
    }
}

/// `limit: int = Field(default=40, ge=1, le=100)`: ints, numeric strings, and
/// floats with no fractional part; range errors report both bounds.
fn check_limit(body: &Value, errors: &mut Vec<Value>) -> i64 {
    const DEFAULT: i64 = 40;
    let value = match body.get("limit") {
        None => return DEFAULT,
        Some(Value::Number(number)) => match number.as_i64() {
            Some(value) => value,
            None => {
                // pydantic's kind for a NUMBER with a fractional part is
                // `int_from_float` (measured against the FastAPI server);
                // the string form ("5.5") stays `int_parsing`.
                errors.push(item(
                    "int_from_float",
                    vec![json!("body"), json!("limit")],
                    "Input should be a valid integer, got a number with a fractional part",
                ));
                return DEFAULT;
            }
        },
        Some(Value::String(text)) => match text.parse::<i64>() {
            Ok(value) => value,
            Err(_) => {
                errors.push(item(
                    "int_parsing",
                    vec![json!("body"), json!("limit")],
                    "Input should be a valid integer, unable to parse string as an integer",
                ));
                return DEFAULT;
            }
        },
        Some(_) => {
            errors.push(item(
                "int_type",
                vec![json!("body"), json!("limit")],
                "Input should be a valid integer",
            ));
            return DEFAULT;
        }
    };
    if value < 1 {
        errors.push(item(
            "greater_than_equal",
            vec![json!("body"), json!("limit")],
            "Input should be greater than or equal to 1",
        ));
    } else if value > 100 {
        errors.push(item(
            "less_than_equal",
            vec![json!("body"), json!("limit")],
            "Input should be less than or equal to 100",
        ));
    }
    value
}

/// `kinds: list[Kind] | None = None` — null is "no filter". `Kind` is a
/// pydantic **Literal**, so every item that is not one of the seven strings
/// answers `literal_error` with the full set as the message — measured on the
/// FastAPI server: `"not-a-kind"` AND `3` both say `literal_error` (a Literal
/// compares inputs, it does not type-check first). Items are checked in list
/// order, each loc carrying its index (FastAPI's `loc` shape).
fn check_kinds(body: &Value, errors: &mut Vec<Value>) -> Option<Vec<String>> {
    match body.get("kinds") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            let mut kinds = Vec::new();
            for (index, entry) in items.iter().enumerate() {
                match entry {
                    Value::String(text) if KINDS.contains(&text.as_str()) => {
                        kinds.push(text.clone())
                    }
                    _ => errors.push(item(
                        "literal_error",
                        vec![json!("body"), json!("kinds"), json!(index)],
                        &enum_msg(),
                    )),
                }
            }
            Some(kinds)
        }
        Some(_) => {
            errors.push(item(
                "list_type",
                vec![json!("body"), json!("kinds")],
                "Input should be a valid list",
            ));
            None
        }
    }
}

/// POST /search — the field order is `SearchRequest`'s declarations: q, kinds,
/// limit, code, meaning, prefix_last (FastAPI reports errors in that order).
pub fn search(body: &Value) -> Result<SearchReq, Vec<Value>> {
    let mut errors = Vec::new();
    if !body_is_object(body, &mut errors) {
        return Err(errors);
    }
    let q = check_q(body, &mut errors);
    let kinds = check_kinds(body, &mut errors);
    let limit = check_limit(body, &mut errors);
    let code = check_bool(body, "code", false, &mut errors);
    let meaning = check_bool(body, "meaning", true, &mut errors);
    let prefix_last = check_bool(body, "prefix_last", false, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(SearchReq {
        q: q.expect("no errors means q was a string"),
        kinds,
        limit,
        code,
        meaning,
        prefix_last,
    })
}

/// POST /embed_ahead — `EmbedAheadRequest`: q (required, ≤ 512) and code.
pub fn embed_ahead(body: &Value) -> Result<EmbedAheadReq, Vec<Value>> {
    let mut errors = Vec::new();
    if !body_is_object(body, &mut errors) {
        return Err(errors);
    }
    let q = check_q(body, &mut errors);
    let code = check_bool(body, "code", false, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(EmbedAheadReq {
        q: q.expect("no errors means q was a string"),
        code,
    })
}
