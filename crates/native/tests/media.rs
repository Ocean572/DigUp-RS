//! The media seam pinned to the engine: `crates/native/src/media.rs` vs
//! `Platform/Portable/Media+Portable.swift` (`MediaInfo.duration/streams`,
//! `AudioExtractor.windows`, `VideoExtractor.keyframes`, `VideoExtractor.frame`), measured by
//! the cross-check in `/tmp/opencode/mediacheck` — Swift in the Ubuntu 22.04 container running
//! `/usr/bin/ffmpeg` (4.4.2), Rust on the host running that same build copied out
//! (`/tmp/opencode/ffmpeg44`), both over the SAME synthetic fixture bytes, 143 lines
//! (13 fixtures × 11 scenarios) with zero field differences.
//!
//! What the fixtures cover: 44.1 kHz stereo → ffmpeg's 16 kHz mono downmix straddling a
//! silence edge (`tone.wav`), 8 kHz up-sample of pure digital silence (`quiet.wav`), an AAC
//! tail shorter than the hop minimum (`voice.m4a`), the 8×8-hash shot dedup over three scene
//! cuts (`scenes.mp4`), hasAudio on video that outlasts its soundtrack (`scenesaudio.mp4`),
//! the MPEG-TS no-index seek with the keyframe lead (`scenes.ts`), no-audio /
//! audio-only containers, a 1.2 s clip with slots past the end, a file cut before its `moov`
//! (duration unknown), a faststart half-cut (duration known, decode dies mid-gop), junk, and
//! the empty file. Refusal texts compare verbatim, and every WAV copy / frame JPEG the seam
//! writes is compared by SHA-256 — the model must hear and see the engine's exact bytes.
//!
//! Run with the pinned tools; a missing ffmpeg is an environment failure, not a skip:
//! `DIGUP_FFMPEG=/tmp/opencode/ffmpeg44` (the 4.4.2 build the container's `ffmpeg` is).

use std::path::{Path, PathBuf};

use digup_core::jsonline;
use digup_native::media;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const ANSWERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media-answers.jsonl"
));

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn out_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("digup-media-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_of(path: &Path) -> String {
    hex_of(&Sha256::digest(
        std::fs::read(path).expect("the file the seam just wrote"),
    ))
}

/// The 36-character UUID stem → "U" (the same normalization the Swift probe applies, its
/// regex `[0-9A-F]{8}-…` over Foundation's uppercase `NSUUID` spelling).
fn is_uuid(bytes: &[u8]) -> bool {
    let hex = |byte: u8| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte);
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes.iter().all(|byte| *byte == b'-' || hex(*byte))
}

fn normalise(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 36 <= bytes.len() && is_uuid(&bytes[i..i + 36]) {
            out.push('U');
            i += 36;
        } else {
            out.push(bytes[i] as char); // fixture and file names here are ASCII
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The scenario runner: the eleven reads the Swift probe ran per fixture, answering in the
// probe's JSON shape so the pinned lines compare field for field.

fn row(fixture: &str, scenario: &str, ok: bool) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("fixture".into(), json!(fixture));
    row.insert("scenario".into(), json!(scenario));
    row.insert("ok".into(), json!(ok));
    row
}

fn refused(fixture: &str, scenario: &str, error: &str) -> Value {
    let mut row = row(fixture, scenario, false);
    row.insert("why".into(), json!(error));
    Value::Object(row)
}

/// One `MediaPiece`: its normalized file name, its stretch, and the SHA-256 of the file the
/// seam wrote there (the cross-check hashed the files off disk; this measures the same bytes).
fn piece_row(piece: &media::MediaPiece) -> Value {
    json!({
        "end": piece.end,
        "file": normalise(piece.path.file_name().unwrap().to_string_lossy().as_ref()),
        "sha": sha256_of(&piece.path),
        "start": piece.start,
    })
}

fn run_scenario(fixtures: &Path, out: &Path, fixture: &str, scenario: &str) -> Value {
    let url = fixtures.join(fixture);
    let folder = out.join(fixture);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).expect("scenario folder");

    let (window, hop) = match scenario {
        "w10_10" => (10.0, 10.0),
        "w5_3" => (5.0, 3.0),
        "w1_1" => (1.0, 1.0),
        _ => (0.0, 0.0),
    };
    if window > 0.0 {
        let sub = folder.join(scenario);
        std::fs::create_dir_all(&sub).expect("window folder");
        let mut row = row(fixture, scenario, true);
        return match media::windows(&url, window, hop, 2.0, &sub) {
            Ok((pieces, duration)) => {
                row.insert(
                    "pieces".into(),
                    Value::Array(pieces.iter().map(piece_row).collect()),
                );
                row.insert("duration".into(), json!(duration));
                Value::Object(row)
            }
            Err(error) => refused(fixture, scenario, &error.to_string()),
        };
    }

    let (every, pixels, max_frames) = match scenario {
        "k2_240" => (2.0f64, 240usize, 1200usize),
        "k05_96" => (0.5, 96, 1200),
        "k01_96m8" => (0.1, 96, 8),
        _ => (0.0, 0, 0),
    };
    if every > 0.0 {
        let sub = folder.join(scenario);
        std::fs::create_dir_all(&sub).expect("frame folder");
        let mut row = row(fixture, scenario, true);
        return match media::keyframes(&url, every, pixels, max_frames, &sub) {
            Ok((frames, duration, has_audio)) => {
                row.insert(
                    "frames".into(),
                    Value::Array(frames.iter().map(piece_row).collect()),
                );
                row.insert("duration".into(), json!(duration));
                row.insert("hasAudio".into(), json!(has_audio));
                Value::Object(row)
            }
            Err(error) => refused(fixture, scenario, &error.to_string()),
        };
    }

    let seconds = match scenario {
        "f0" => Some(0.0f64),
        "f5_5" => Some(5.5),
        "f600" => Some(600.0),
        _ => None,
    };
    match scenario {
        "dur" => {
            let mut row = row(fixture, scenario, true);
            // Swift stores the wrapped nil (`duration(of:) as Any`) and JSONSerialization
            // spells it `null` — measured on broken/junk/empty.
            row.insert(
                "seconds".into(),
                match media::duration_of(&url) {
                    Some(value) => json!(value),
                    None => Value::Null,
                },
            );
            Value::Object(row)
        }
        "streams" => {
            let mut row = row(fixture, scenario, true);
            let (audio, video) = media::streams_of(&url);
            row.insert("audio".into(), json!(audio));
            row.insert("video".into(), json!(video));
            Value::Object(row)
        }
        _ if seconds.is_some() => {
            let preview = folder.join(format!("{scenario}.jpg"));
            let mut row = row(fixture, scenario, true);
            match media::frame_at(seconds.expect("seconds"), &url, 480, &preview) {
                Ok(()) => {
                    row.insert("wrote".into(), json!(true));
                    row.insert("sha".into(), json!(sha256_of(&preview)));
                    Value::Object(row)
                }
                Err(error) => refused(fixture, scenario, &error.to_string()),
            }
        }
        _ => panic!("unknown scenario {scenario}"),
    }
}

// ---------------------------------------------------------------------------
// The pinned suite.

/// The vendored fixtures are the bytes both engines measured — the digests the cross-check
/// ran on. A fixture that drifts invalidates every answer line beside it.
#[test]
fn fixture_bytes_match_the_cross_check() {
    let pins: &[(&str, &str)] = &[
        (
            "media-audioonly.mp4",
            "a8eeb49dca8ba50d24dc6a867f5cc19e7886312c7d5ebfed9d30953ccb80bd7d",
        ),
        (
            "media-broken.mp4",
            "457a9999642f6858cbfce38d95a36ae225dd1e441665dcf740e22f39e34b94a9",
        ),
        (
            "media-broken2.mp4",
            "53c0a1ef9b4913d00653f5ba6ce0b72c1cf45542ddb38355aa3268abedf08092",
        ),
        (
            "media-empty.mp4",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "media-junk.mp4",
            "4f80d917a73a62c90768839d2f1c76fac8e34282f77a40d5503d63f84a0a88b1",
        ),
        (
            "media-noaudio.mp4",
            "228b6ae000aa596f89beea4212e8fa092c55b61b369674b1396c771209bdb5ae",
        ),
        (
            "media-quiet.wav",
            "ecce058691178795acf7f5b148d1478adc1d77a67048f2792e11907c5b792d5f",
        ),
        (
            "media-scenes.mp4",
            "27b630c041feeaa12af9e100e28b2d5cc5a7ca6b983bece0aaca4ad08d0a09be",
        ),
        (
            "media-scenes.ts",
            "49cb1c308995045d6c426f3627460c535619a2084f2b46ab76e9e7ef83aa87e9",
        ),
        (
            "media-scenesaudio.mp4",
            "a1292c68ff9872db7b9872365ae440a5c2de28b9a2cb8b794863ab57d01fdee5",
        ),
        (
            "media-short.mp4",
            "2cc45d73f3fc1737834f419c3f070d8a900dc4964e83477ed00bb405a687f068",
        ),
        (
            "media-tone.wav",
            "249a66b287d1ef93f1c385c918321782cb572fe6506a54bd53b92d1bb1a81380",
        ),
        (
            "media-voice.m4a",
            "022fa85ef4360d09714078e90ce688490ae413addb0a028449f0f6740f34547e",
        ),
    ];
    let dir = fixtures_dir();
    assert_eq!(pins.len(), 13, "the thirteen media fixtures");
    for (name, want) in pins {
        let data = std::fs::read(dir.join(name)).expect("the vendored fixture");
        assert_eq!(&hex_of(&Sha256::digest(&data)), want, "the bytes of {name}");
    }
}

/// The engine's answers, every (fixture, scenario): duration, stream kinds, the audio windows
/// (each WAV's SHA-256 and stretch), the keyframes (each JPEG's SHA-256, stretch, hasAudio),
/// the frame_at previews, and the refusal texts.
#[test]
fn engine_answers_match() {
    if digup_native::ffmpeg::ffmpeg().is_none() || digup_native::ffmpeg::ffprobe().is_none() {
        panic!(
            "ffmpeg/ffprobe not found: set DIGUP_FFMPEG=/tmp/opencode/ffmpeg44 \
             (the pinned 4.4.2 build the Swift container's /usr/bin/ffmpeg is). \
             An environment gap is a FAIL here, never a skip."
        );
    }
    let fixtures = fixtures_dir();
    let out = out_dir("answers");
    let mut measured = 0;
    for line in ANSWERS.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let want: Value = serde_json::from_str(line).expect("an answer line");
        let fixture = want["fixture"].as_str().expect("fixture");
        let scenario = want["scenario"].as_str().expect("scenario");
        let got = run_scenario(&fixtures, &out, fixture, scenario);
        // Compare through the port's own JSON emitter: Swift writes whole doubles as `6`,
        // serde_json keeps `6.0`, and the engine's contract is Foundation's spelling. The
        // emitter normalizes both sides the same way it spells them on the wire.
        assert_eq!(
            jsonline::to_line(&got),
            jsonline::to_line(&want),
            "{fixture} / {scenario}"
        );
        measured += 1;
    }
    assert_eq!(measured, 143, "13 fixtures x 11 scenarios");
}

/// The name normalization the answers rely on: Foundation spells `NSUUID` uppercase 36-char
/// with dashes, and the seam's files carry it between fixed affixes.
#[test]
fn uuid_names_normalise() {
    let id = digup_core::uuid::uuid_string();
    assert!(is_uuid(id.as_bytes()), "{id} is the 36-character stem");
    assert_eq!(
        normalise(&format!("frame-{id}-000011.jpg")),
        "frame-U-000011.jpg"
    );
    assert_eq!(normalise(&format!("{id}.wav")), "U.wav");
    assert_eq!(normalise("tone.wav"), "tone.wav"); // no stem, no change
    assert_eq!(normalise(&id.to_lowercase()), id.to_lowercase()); // the engine spells UUIDs uppercase
}
