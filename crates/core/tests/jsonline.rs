//! The JSON-lines format, pinned against Foundation's `JSONSerialization` (`.sortedKeys`),
//! measured on this box with Swift 6.4 (probe in `/tmp/opencode/jsonfmt`). Every line the engine's
//! helpers write is bytes on a pipe: the port reproduces them.

use serde_json::{json, Map, Value};

fn line(fields: Value) -> String {
    digup_core::jsonline::to_line(&fields).expect("finite numbers write a line")
}

#[test]
fn numbers_are_the_shortest_decimal_foundation_writes() {
    // swift=<measured bytes> for each value; the table is the probe's own output.
    let cases: &[(&str, f64, &str)] = &[
        ("0.0", 0.0, "0"),
        ("neg zero", -0.0, "-0"),
        ("2.0", 2.0, "2"),
        ("100.0", 100.0, "100"),
        ("-10.0", -10.0, "-10"),
        ("0.5", 0.5, "0.5"),
        ("1e-4", 1e-4, "0.0001"),
        ("1e-3", 1e-3, "0.001"),
        ("1e-5", 1e-5, "1e-05"),
        ("1.23e-5", 0.0000123, "1.23e-05"),
        ("9.9999e-6", 9.9999e-6, "9.9999e-06"),
        ("1e-7", 1e-7, "1e-07"),
        ("1e-16", 1e-16, "1e-16"),
        ("1e-24", 1e-24, "1e-24"),
        ("5e-324", 5e-324, "5e-324"),
        ("1e14", 1e14, "100000000000000"),
        ("1e15", 1e15, "1000000000000000"),
        ("1234567890123456.0", 1234567890123456.0, "1234567890123456"),
        ("1e16", 1e16, "1e+16"),
        (
            "1.2345678901234567e16",
            12345678901234567.0,
            "1.2345678901234568e+16",
        ),
        ("1e20", 99999999999999999999.0, "1e+20"),
        ("1e21", 1e21, "1e+21"),
        (
            "f64::MAX",
            1.7976931348623157e308,
            "1.7976931348623157e+308",
        ),
        ("11.045714", 11.045714, "11.045714"),
        ("1/3", 1.0 / 3.0, "0.3333333333333333"),
        ("nextafter(1,2)", 1.0000000000000002, "1.0000000000000002"),
        ("0.1", 0.1, "0.1"),
        (
            "123456789.123456789",
            "123456789.123456789".parse::<f64>().unwrap(),
            "123456789.12345679",
        ),
        ("pi*2", 2.0 * std::f64::consts::PI, "6.283185307179586"),
        ("seconds", 23.200155291527505, "23.200155291527505"),
        ("video_seconds", 81.77799999999999, "81.77799999999999"),
        ("kind_seconds", 0.027826137800414517, "0.027826137800414517"),
    ];
    for (label, value, want) in cases {
        let got = line(json!({ "v": value }));
        assert_eq!(got, format!("{{\"v\":{want}}}"), "{label}");
    }
}

#[test]
fn whole_doubles_and_integers_share_the_integer_spelling() {
    assert_eq!(line(json!({ "n": 320 })), r#"{"n":320}"#);
    assert_eq!(
        line(json!({ "n": -1234567890123_i64 })),
        r#"{"n":-1234567890123}"#
    );
    assert_eq!(
        line(json!({ "n": i64::MAX })),
        r#"{"n":9223372036854775807}"#
    );
    assert_eq!(
        line(json!({ "n": u64::MAX })),
        r#"{"n":18446744073709551615}"#
    );
}

#[test]
fn foundation_escapes_the_slash_in_every_path() {
    let fields = json!({
        "folder": "/tmp/opencode/media-xcheck",
        "strings": { "a/b/c": "path/with/slash", "quote": "he said \"hi\"", "back": "a\\b",
                     "nl": "a\nb\tb\ru\u{0000}", "uni": "süß — 日本語 ✓", "emoji": "🙂 x" }
    });
    assert_eq!(
        line(fields),
        r#"{"folder":"\/tmp\/opencode\/media-xcheck","strings":{"a\/b\/c":"path\/with\/slash","back":"a\\b","emoji":"🙂 x","nl":"a\nb\tb\ru\u0000","quote":"he said \"hi\"","uni":"süß — 日本語 ✓"}}"#
    );
}

#[test]
fn keys_sort_in_foundations_localized_order() {
    // The order Foundation produced for this exact key set, measured on the same box.
    let want: &[&str] = &[
        "0",
        "9",
        "10",
        "010",
        "A",
        "ä",
        "A 1",
        "a b",
        "a_1",
        "a-1",
        "a.b",
        "a/b",
        "a0b",
        "a1",
        "a01",
        "ab",
        "image",
        "Image",
        "IMAGE",
        "IMG1",
        "img2",
        "img10",
        "p",
        "P",
        "photo",
        "photos",
        "Photos",
        "Photos library",
        "Z",
        "z9",
        "z10",
    ];
    let mut fields = Map::new();
    for (index, key) in want.iter().enumerate() {
        fields.insert((*key).to_string(), json!(index));
    }
    let out = line(Value::Object(fields));
    let keys: Vec<String> = out
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim_matches('{');
            let key = trimmed.split_once(':')?.0;
            Some(key.trim_matches('"').replace("\\/", "/"))
        })
        .collect();
    assert_eq!(keys, want, "key order: {out}");
}

#[test]
fn the_engines_own_dictionary_keys_come_out_in_its_order() {
    // `left_out` as the engine writes it: "Photos library" last, because the comparison ignores case.
    let mut fields = Map::new();
    for (key, count) in [
        ("app or bundle", 1),
        ("code or data file", 13),
        ("copy-protected", 1),
        ("dataset", 320),
        ("inside a code project", 2),
        ("link or special file", 2),
        ("other type", 4),
        ("Photos library", 1),
    ] {
        fields.insert(key.to_string(), json!(count));
    }
    let out = line(Value::Object(fields));
    assert_eq!(
        out,
        r#"{"app or bundle":1,"code or data file":13,"copy-protected":1,"dataset":320,"inside a code project":2,"link or special file":2,"other type":4,"Photos library":1}"#,
        "{out}"
    );
}

#[test]
fn a_tie_rounds_to_the_even_last_digit() {
    // The 80 exact-tie cases the tie probe found (Foundation's bytes match ties-to-even on all
    // 80; Rust's own `{:e}` spends a digit more and rounds half away, which is what this pins).
    let cases: &[(&str, &str)] = &[
        ("-1323365521330295.25", "-1323365521330295.2"), // kept digit 2, even: stays
        ("1323365521330295.25", "1323365521330295.2"),
        ("2215125484055601.75", "2215125484055601.8"), // kept digit 7, odd: up to the even 8
        ("-229201756668528.375", "-229201756668528.38"),
        ("236004492336982.125", "236004492336982.12"),
        ("117083921059173.625", "117083921059173.62"),
        ("2148270316112.031494140625", "2148270316112.0315"), // the exact expansion, rounded
        ("0.15625", "0.15625"),                               // already the shortest: no rounding
    ];
    for (input, want) in cases {
        let value: f64 = input.parse().unwrap();
        let out = line(json!({ "v": value }));
        assert_eq!(out, format!("{{\"v\":{want}}}"), "input {input}");
    }
}

#[test]
fn a_non_finite_number_never_writes_invalid_json() {
    // A raw `NaN` token would be invalid JSON; `JSONSerialization` refuses those numbers and
    // `emit` writes nothing (`guard let data = try?`). serde_json drops them to `null` on the way
    // in, so the port never spells one.
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        match digup_core::jsonline::to_line(&json!({ "v": value })) {
            None => {}
            Some(out) => assert!(out.contains("null"), "{out}"),
        }
    }
}

#[test]
fn empty_containers_and_booleans() {
    assert_eq!(line(json!({ "a": [], "b": {} })), r#"{"a":[],"b":{}}"#);
    assert_eq!(
        line(json!({ "t": true, "f": false, "n": null })),
        r#"{"f":false,"n":null,"t":true}"#
    );
}
