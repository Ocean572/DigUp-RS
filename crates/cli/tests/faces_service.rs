//! The faces-service gate (FACES.md Phase 2 §2): a live daemon over JSON-lines,
//! the seam the server and the app speak. Spawns `digup faces-service` on the
//! five-photo public Wikimedia gate fixtures (one face each, three persons),
//! drives every verb, reads the store back through the daemon's own replies.
//! Self-skips when the face models are absent — the rule every model-pinned
//! suite shares (`crates/cli/tests/cli.rs`, the mcp daemon pattern).

#![cfg(feature = "faces")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

fn face_models_dir() -> Option<PathBuf> {
    // The pinned face models, the server's rule: DIGUP_FACE_MODELS, else the
    // standard models folder. Absent → self-skip (ci.yml ships no face models).
    let env = std::env::var("DIGUP_FACE_MODELS").ok().map(PathBuf::from);
    let home = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join("digup-data/models"));
    env.into_iter().chain(home).find(|dir| {
        dir.join("scrfd_10g_bnkps.onnx").is_file() && dir.join("w600k_mbf.onnx").is_file()
    })
}

/// The daemon as the thin clients see it: write a line, read lines, a reply
/// is the line carrying your `id`; everything before it is an event.
struct Daemon {
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    child: Option<Child>,
    events: Vec<Value>,
    next_id: i64,
}

impl Daemon {
    fn start(index: &Path, models: &Path, consent: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_digup"))
            .args([
                "faces-service".to_string(),
                "--index".to_string(),
                index.to_string_lossy().into_owned(),
                "--faces-models".to_string(),
                models.to_string_lossy().into_owned(),
                "--faces-consent".to_string(),
                consent.to_string_lossy().into_owned(),
                "--emb-model".to_string(),
                "w600k_mbf.onnx".to_string(),
                "--merge-threshold".to_string(),
                "0.45".to_string(),
                "--model-idle".to_string(),
                "3600".to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("faces-service spawns");
        let stdin = child.stdin.take().expect("the daemon's stdin");
        let stdout = child.stdout.take().expect("the daemon's stdout");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut daemon = Daemon {
            stdin,
            lines: rx,
            child: Some(child),
            events: Vec::new(),
            next_id: 0,
        };
        let ready = daemon.line(30).expect("the daemon says ready");
        assert_eq!(ready["event"], "ready", "the daemon's first line is ready");
        daemon.events.push(ready);
        daemon
    }

    /// The next daemon line (an event or a reply), bounded.
    fn line(&self, seconds: u64) -> Option<Value> {
        match self.lines.recv_timeout(Duration::from_secs(seconds)) {
            Ok(line) => Some(serde_json::from_str(&line).expect("one JSON object per line")),
            Err(RecvTimeoutError::Timeout) => None,
            Err(error) => panic!("the daemon's stream closed: {error}"),
        }
    }

    /// Send a request, return its reply; events along the way are recorded.
    fn request(&mut self, op: &str, fields: Value, seconds: u64) -> Value {
        let mut request = fields.as_object().expect("a request object").clone();
        self.next_id += 1;
        let id = self.next_id;
        request.insert("id".into(), json!(id));
        request.insert("op".into(), json!(op));
        writeln!(self.stdin, "{}", Value::Object(request)).expect("the request line lands");
        self.stdin.flush().expect("flush");
        loop {
            let line = self.line(seconds).expect("the daemon answers in time");
            if line.get("id").and_then(Value::as_i64) == Some(id) {
                return line;
            }
            self.events.push(line);
        }
    }

    fn quit(&mut self) {
        let reply = self.request("quit", json!({}), 30);
        assert_eq!(reply["ok"], true, "quit is answered");
        if let Some(child) = self.child.as_mut() {
            let status = child.wait().expect("the daemon exits");
            assert_eq!(status.code(), Some(0), "faces-service exit code");
        }
    }
}

/// The deployment's consent record (FACES.md Phase 1 §5): the daemon refuses
/// every verb without it — the engine gates what the UI gates.
fn consent_file(dir: &Path) -> PathBuf {
    let path = dir.join("faces-consent.json");
    std::fs::write(&path, r#"{"consented": true, "accepted_at": 0.0}"#)
        .expect("the consent record");
    path
}

#[test]
fn faces_service_verbs_round_trip_on_the_public_fixtures() {
    let Some(models) = face_models_dir() else {
        eprintln!("SKIP: the pinned face models are absent (faces_service gate)");
        return;
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/faces");
    assert!(fixtures.is_dir(), "the face fixtures ride in the repo");
    let work = tempfile::tempdir().expect("a work folder");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    let consent = consent_file(work.path());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/faces");

    let mut daemon = Daemon::start(&index, &models, &consent);
    let roots = json!({"roots": [fixtures.to_string_lossy()]});

    // 1. scan: five photos, one face each; the clustering pass runs on the
    //    scan's tail; one event per scanned file.
    let reply = daemon.request("scan", roots.clone(), 600);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["files"], 5, "five photos scanned: {reply}");
    assert_eq!(reply["faces"], 5, "one face each: {reply}");
    assert_eq!(reply["skipped"], 0, "every fixture decodes: {reply}");
    assert_eq!(reply["assigned"], 5, "every face joins a person: {reply}");
    let scan_events = daemon
        .events
        .iter()
        .filter(|e| e["event"] == "face-scan")
        .count();
    assert_eq!(scan_events, 5, "one face-scan event per file");

    // 1b. piggyback freshness (FACES.md §3, the scans table): a rescan of the same
    //     roots decodes 0 files and skips all 5 as fresh — the faceless-photo case
    //     included, which is the whole point of schema 2 (measured 2026-11-22:
    //     under schema 1 a faceless file re-decoded at ~4 s/photo on every scan).
    let rescan = daemon.request("scan", roots.clone(), 600);
    assert_eq!(rescan["ok"], true, "{rescan}");
    assert_eq!(rescan["files"], 0, "nothing new to decode: {rescan}");
    assert_eq!(
        rescan["fresh"], 5,
        "every fixture is recorded at (path,size,mtime): {rescan}"
    );
    assert_eq!(rescan["faces"], 0, "no face is paid for twice: {rescan}");
    assert_eq!(
        rescan["assigned"], 0,
        "clustering has nothing new: {rescan}"
    );

    // 2. counts + the persons the clustering opened. Measured fixture facts
    //    (rows pinned in FACES.md): the 2005 and 2021 files are the SAME
    //    source photograph (cos 1.0000 — they merge exactly), and the real
    //    2008↔2013 pair measures 0.4761 with w600k_mbf — it joins at the
    //    0.45 threshold this daemon runs, not at 0.5. Three people, one of
    //    them the three-file Merkel cluster.
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(counts["ok"], true, "{counts}");
    assert_eq!(counts["faces"], 5);
    assert_eq!(counts["persons"], 3, "three people: {counts}");
    assert_eq!(counts["named"], 0);
    assert_eq!(counts["pending"], 0);
    assert_eq!(counts["models"]["emb"], "w600k_mbf.onnx");
    let persons = daemon.request("persons", json!({}), 60);
    let rows = persons["persons"].as_array().expect("a persons array");
    assert_eq!(rows.len(), 3);
    let merkel = rows
        .iter()
        .find(|p| p["count"] == json!(3))
        .expect("the three-photo person is one cluster");
    let singletons: Vec<i64> = rows
        .iter()
        .filter(|p| p["count"] == json!(1))
        .map(|p| p["id"].as_i64().expect("a person id"))
        .collect();
    assert_eq!(singletons.len(), 2);

    // 3. name-person rides the store and shows in the list.
    let merkel_id = merkel["id"].as_i64().expect("a person id");
    let named = daemon.request("name", json!({"person": merkel_id, "name": "Angela"}), 60);
    assert_eq!(named["ok"], true, "{named}");
    let persons = daemon.request("persons", json!({}), 60);
    let row = persons["persons"]
        .as_array()
        .expect("a persons array")
        .iter()
        .find(|p| p["id"] == json!(merkel_id))
        .expect("the named person");
    assert_eq!(row["name"], "Angela");
    assert_eq!(daemon.request("counts", json!({}), 60)["named"], 1);

    // 4. person-photos: the three Merkel files, each with its box and landmarks.
    let photos = daemon.request("person", json!({"person": merkel_id}), 60);
    let faces = photos["faces"].as_array().expect("a faces array");
    assert_eq!(faces.len(), 3);
    let paths: Vec<&str> = faces
        .iter()
        .map(|f| f["path"].as_str().expect("a path"))
        .collect();
    assert_eq!(
        paths.len(),
        paths.iter().collect::<std::collections::HashSet<_>>().len()
    );
    for face in faces {
        assert_eq!(face["box"].as_array().expect("a box").len(), 4);
        assert_eq!(face["kps"].as_array().expect("kps").len(), 5);
        assert!(
            face["score"].as_f64().expect("a score") >= 0.5,
            "the score gate held"
        );
    }
    let anchor = faces
        .iter()
        .find(|f| f["path"].as_str().unwrap().contains("2005"))
        .expect("the 2005 photo");
    let anchor_face = anchor["id"].as_i64().expect("a face id");

    // 5. find-similar-faces: the same person's photos rank above the others.
    let similar = daemon.request("similar", json!({"face": anchor_face, "limit": 4}), 60);
    let hits = similar["faces"].as_array().expect("a hits array");
    assert_eq!(hits.len(), 4, "the other four faces rank: {similar}");
    assert!(hits[0]["path"].as_str().unwrap().contains("Angela_Merkel"));
    assert!(hits[1]["path"].as_str().unwrap().contains("Angela_Merkel"));
    assert!(
        hits[0]["cos"].as_f64().expect("a cos") > hits[3]["cos"].as_f64().expect("a cos"),
        "cosine ranks: {similar}"
    );

    // 6. merge-persons, then delete-person: the faces are evidence, they queue.
    let merged = daemon.request(
        "merge",
        json!({"into": singletons[1], "from": singletons[0]}),
        60,
    );
    assert_eq!(merged["ok"], true, "{merged}");
    assert_eq!(daemon.request("counts", json!({}), 60)["persons"], 2);
    let deleted = daemon.request("delete", json!({"person": singletons[1]}), 60);
    assert_eq!(deleted["ok"], true, "{deleted}");
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (counts["persons"].as_i64(), counts["pending"].as_i64()),
        (Some(1), Some(2)),
        "{counts}"
    );
    let reclustered = daemon.request("cluster", json!({}), 60);
    assert_eq!(
        reclustered["assigned"], 2,
        "the two queue and open again: {reclustered}"
    );
    assert_eq!(daemon.request("counts", json!({}), 60)["persons"], 3);

    // 7. pending-clusters is empty here (the fixtures are cleanly separated) —
    //    the review queue answers its shape.
    let pending = daemon.request("pending", json!({}), 60);
    assert_eq!(
        pending["count"], 0,
        "no collisions on the fixtures: {pending}"
    );

    // 8. the ghost-row cascade: no roots → every face row is outside scope.
    let pruned = daemon.request("prune", json!({"roots": []}), 60);
    assert_eq!(pruned["ok"], true, "{pruned}");
    assert_eq!(pruned["pruned"], 5, "{pruned}");
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (
            counts["faces"].as_i64(),
            counts["persons"].as_i64(),
            counts["pending"].as_i64()
        ),
        (Some(0), Some(0), Some(0)),
        "the store emptied: {counts}"
    );

    // 9. quit answers, then the process exits clean.
    daemon.quit();
}

#[test]
fn faces_service_refuses_the_second_embedder_pin_on_the_same_store() {
    // Store-scoped model law (FACES.md tiers): one face.sqlite, one embedder.
    // A daemon under a different --emb-model than the store's stamp refuses.
    let Some(models) = face_models_dir() else {
        eprintln!("SKIP: the pinned face models are absent (faces_service gate)");
        return;
    };
    // Only if a second embedder pin is present (w600k_r50 rides the same pin set).
    if !models.join("w600k_r50.onnx").is_file() {
        eprintln!("SKIP: w600k_r50.onnx absent (tier-switch gate)");
        return;
    }
    let work = tempfile::tempdir().expect("a work folder");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    let consent = consent_file(work.path());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/faces");
    let mut daemon = Daemon::start(&index, &models, &consent);
    let reply = daemon.request(
        "scan",
        json!({"roots": [fixtures.to_string_lossy()], "limit": 1}),
        600,
    );
    assert_eq!(reply["ok"], true, "{reply}");
    daemon.quit();
    // A second daemon under the r50 pin meets the store's w600k_mbf stamp.
    let mut second = Command::new(env!("CARGO_BIN_EXE_digup"))
        .args([
            "faces-service".to_string(),
            "--index".to_string(),
            index.to_string_lossy().into_owned(),
            "--faces-models".to_string(),
            models.to_string_lossy().into_owned(),
            "--faces-consent".to_string(),
            consent.to_string_lossy().into_owned(),
            "--emb-model".to_string(),
            "w600k_r50.onnx".to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the second daemon spawns");
    // The refusal is its OWN: the ready line is absent, the process dies on
    // the model law (its stderr carries the reason); EOF makes it exit.
    drop(second.stdin.take());
    let status = second.wait().expect("the second daemon exits");
    assert_ne!(status.code(), Some(0), "the tier switch is a hard refusal");
}

#[test]
fn faces_service_without_a_consent_record_refuses_to_serve() {
    // The consent gate is the ENGINE's (FACES.md Phase 1 §5), not a UI
    // decoration: no accepted record at --faces-consent → the daemon dies
    // before `ready`, no store opened, no model loaded, exit non-zero.
    // No pinned models needed — the refusal happens above every of their
    // seams, so this gate runs on every CI box.
    let work = tempfile::tempdir().expect("a work folder");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    let mut child = Command::new(env!("CARGO_BIN_EXE_digup"))
        .args([
            "faces-service".to_string(),
            "--index".to_string(),
            index.to_string_lossy().into_owned(),
            "--faces-models".to_string(),
            work.path().to_string_lossy().into_owned(),
            "--emb-model".to_string(),
            "w600k_mbf.onnx".to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("faces-service spawns");
    drop(child.stdin.take()); // EOF: the daemon has nothing to answer
    let status = child.wait().expect("the daemon exits");
    assert_ne!(status.code(), Some(0), "no consent, no service");
    assert!(
        !index.join("face.sqlite").exists(),
        "the refusal happens BEFORE the store opens"
    );
    // A record that does not consent is the same refusal.
    let lying = work.path().join("faces-consent.json");
    std::fs::write(&lying, r#"{"consented": false}"#).expect("the record");
    let mut child = Command::new(env!("CARGO_BIN_EXE_digup"))
        .args([
            "faces-service".to_string(),
            "--index".to_string(),
            index.to_string_lossy().into_owned(),
            "--faces-models".to_string(),
            work.path().to_string_lossy().into_owned(),
            "--faces-consent".to_string(),
            lying.to_string_lossy().into_owned(),
            "--emb-model".to_string(),
            "w600k_mbf.onnx".to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("faces-service spawns");
    drop(child.stdin.take());
    assert_ne!(
        child.wait().expect("the daemon exits").code(),
        Some(0),
        "a record with consented:false is no consent"
    );
}
