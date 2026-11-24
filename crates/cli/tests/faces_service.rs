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
        Self::start_with(index, models, consent, &[])
    }

    fn start_with(index: &Path, models: &Path, consent: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_digup"))
            .args(
                [
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
                ]
                .into_iter()
                .chain(extra.iter().map(|s| s.to_string())),
            )
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
    assert_eq!(
        reply["assigned"], 3,
        "the core law: the trio joins, the singletons stay queued: {reply}"
    );
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

    // 2. counts + the persons the clustering opened (core law, FACES.md
    //    2026-11-23: a person exists from 3 faces up — Immich's `minFaces` law,
    //    the measured cure for the bench fragmentation). Measured fixture facts
    //    (rows pinned in FACES.md): the 2005 and 2021 files are the SAME source
    //    photograph (cos 1.0000 — they merge exactly), and the real cross-age
    //    pair 2005/2021 ↔ 2013 measures 0.4761 with w600k_mbf — it joins at the
    //    0.45 threshold this daemon runs, not at 0.5. One person: the three-file
    //    Merkel cluster; the Obama and Trump singletons are queued, not cards.
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(counts["ok"], true, "{counts}");
    assert_eq!(counts["faces"], 5);
    assert_eq!(
        counts["persons"], 1,
        "the core law keeps singletons out of the cards: {counts}"
    );
    assert_eq!(counts["named"], 0);
    assert_eq!(
        counts["pending"], 2,
        "the singletons are review material: {counts}"
    );
    assert_eq!(counts["models"]["emb"], "w600k_mbf.onnx");
    let persons = daemon.request("persons", json!({}), 60);
    let rows = persons["persons"].as_array().expect("a persons array");
    assert_eq!(rows.len(), 1);
    let merkel = rows
        .iter()
        .find(|p| p["count"] == json!(3))
        .expect("the three-photo person is one cluster");

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

    // 6. the review queue, BEST MATCH FIRST (the user's law 2026-11-23: "faces
    //    with close matches should be at the top — I can't scroll through that
    //    many"). The two queued singletons answer with the engine's real cosines
    //    against the one person — all under the merge line, "no close person"
    //    is the honest shape — sorted by their best candidate descending, ties
    //    broken by face identity (the pool determinism law).
    let pending = daemon.request("pending", json!({}), 60);
    let queue = pending["pending"].as_array().expect("a queue");
    assert_eq!(pending["count"], 2, "{pending}");
    assert_eq!(queue.len(), 2);
    let best = |row: &serde_json::Value| row["candidates"][0]["cos"].as_f64().expect("a cos");
    assert!(
        best(&queue[0]) >= best(&queue[1]),
        "the faces with the closest candidates sort to the top: {pending}"
    );
    for row in queue {
        assert!(
            best(row) < 0.45,
            "the engine does not guess: these faces have no close person: {pending}"
        );
        assert_eq!(
            row["candidates"][0]["person"], merkel["id"],
            "the only candidate is the one person: {pending}"
        );
        assert_eq!(
            row["candidates"][0]["name"], "Angela",
            "the candidate carries the person's name: {pending}"
        );
    }

    // 7. confirm-person is the human's override of the engine's refusal: the
    //    Obama face joins the named person at a cosine the clustering refused
    //    (below the merge line — the human decides, the engine relays). And the
    //    cluster pass keeps the REMAINING singleton queued: a face with no
    //    close person never opens a card and never disappears (the core law's
    //    review shape — the exact screen the user saw on the bench library).
    let face_of = |row: &serde_json::Value| row["face"]["id"].as_i64().expect("a face id");
    let obama = queue
        .iter()
        .find(|row| {
            row["face"]["path"]
                .as_str()
                .unwrap_or_default()
                .contains("Obama")
        })
        .expect("the queued Obama face");
    let confirmed = daemon.request(
        "confirm",
        json!({"face": face_of(obama), "person": merkel_id}),
        60,
    );
    assert_eq!(confirmed["ok"], true, "{confirmed}");
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (counts["persons"].as_i64(), counts["pending"].as_i64()),
        (Some(1), Some(1)),
        "{counts}"
    );
    let reclustered = daemon.request("cluster", json!({}), 60);
    assert_eq!(
        reclustered["assigned"], 0,
        "the last singleton has no close person: it stays queued, it opens no card: {reclustered}"
    );
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (counts["persons"].as_i64(), counts["pending"].as_i64()),
        (Some(1), Some(1)),
        "{counts}"
    );
    let trump_queue = daemon.request("pending", json!({}), 60);
    let trump = &trump_queue["pending"].as_array().expect("a queue")[0];
    assert!(
        trump["face"]["path"]
            .as_str()
            .unwrap_or_default()
            .contains("Trump"),
        "the queued face after the confirm is the Trump photo: {trump_queue}"
    );
    let confirmed = daemon.request(
        "confirm",
        json!({"face": face_of(trump), "person": merkel_id}),
        60,
    );
    assert_eq!(confirmed["ok"], true, "{confirmed}");
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (counts["assigned"].as_i64(), counts["pending"].as_i64()),
        (Some(5), Some(0)),
        "{counts}"
    );

    // 8. delete-person: the human dissolves the card — every face is evidence
    //    again — and one cluster press rebuilds exactly the core-law shape from
    //    an EMPTY persons table: the Merkel trio becomes a person, the
    //    singletons stay queued (the cluster verb from zero, convergence pinned).
    let deleted = daemon.request("delete", json!({"person": merkel_id}), 60);
    assert_eq!(deleted["ok"], true, "{deleted}");
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (counts["persons"].as_i64(), counts["pending"].as_i64()),
        (Some(0), Some(5)),
        "{counts}"
    );
    let reclustered = daemon.request("cluster", json!({}), 60);
    assert_eq!(
        reclustered["assigned"], 3,
        "the trio re-forms, the singletons stay queued: {reclustered}"
    );
    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        (counts["persons"].as_i64(), counts["pending"].as_i64()),
        (Some(1), Some(2)),
        "{counts}"
    );

    // 9. the ghost-row cascade: no roots → every face row is outside scope.
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

    // 10. quit answers, then the process exits clean.
    daemon.quit();
}

#[test]
fn faces_service_scan_with_named_workers_answers_the_same_fields() {
    // The worker pool (FACES.md 2026-11-22): the scan replies under a NAMED
    // `--faces-workers 4` answer exactly what the default daemon answers —
    // same files/faces/skipped/assigned, one face-scan event per fixture.
    // The events are pinned as a SET of paths: parallel workers make event
    // ORDER a timing artifact (the tie-order law of AGENTS.md), the identity
    // of the recorded files is the contract.
    let Some(models) = face_models_dir() else {
        eprintln!("SKIP: the pinned face models are absent (faces_service gate)");
        return;
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/faces");
    let work = tempfile::tempdir().expect("a work folder");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    let consent = consent_file(work.path());
    let mut daemon = Daemon::start_with(&index, &models, &consent, &["--faces-workers", "4"]);
    let reply = daemon.request("scan", json!({"roots": [fixtures.to_string_lossy()]}), 600);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["files"], 5, "five photos scanned: {reply}");
    assert_eq!(reply["faces"], 5, "one face each: {reply}");
    assert_eq!(reply["skipped"], 0, "every fixture decodes: {reply}");
    assert_eq!(
        reply["assigned"], 3,
        "the core law: the trio joins, the singletons stay queued: {reply}"
    );
    let mut paths: Vec<String> = daemon
        .events
        .iter()
        .filter(|e| e["event"] == "face-scan")
        .map(|e| e["path"].as_str().unwrap_or_default().to_string())
        .collect();
    paths.sort();
    let mut want: Vec<String> = std::fs::read_dir(&fixtures)
        .expect("the fixtures folder")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .map(|x| x.to_string_lossy() == "jpg")
                .unwrap_or(false)
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    want.sort();
    assert_eq!(paths, want, "one face-scan event per fixture, as a set");
    daemon.quit();
}

#[test]
fn faces_service_scan_never_opens_excluded_folders() {
    // The Folders-tab skip list (2026-11-22): `--exclude` folders are OFF LIMITS to the
    // face scan's crawl — the main index's own exclusion law, including its override
    // rule: a skip applies INSIDE a chosen root, and a folder CHOSEN whole is walked
    // whole (FolderSelection). So the test chooses the fixtures' PARENT and excludes
    // the faces folder inside it — the exclusion's own shape.
    let Some(models) = face_models_dir() else {
        eprintln!("SKIP: the pinned face models are absent (faces_service gate)");
        return;
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/faces");
    let parent = fixtures
        .parent()
        .expect("the fixtures have a parent folder");
    let work = tempfile::tempdir().expect("a work folder");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    let consent = consent_file(work.path());
    let mut daemon = Daemon::start_with(
        &index,
        &models,
        &consent,
        &["--exclude", &fixtures.to_string_lossy()],
    );
    let reply = daemon.request("scan", json!({"roots": [parent.to_string_lossy()]}), 60);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(
        reply["files"], 0,
        "the excluded faces folder yields no scanned file: {reply}"
    );
    assert_eq!(
        reply["faces"], 0,
        "nothing is decoded, so nothing is recorded: {reply}"
    );
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

/// The paintable window (the People tab's shape, 2026-11-23): `covers: true` pays one
/// cover query per person — the live store measured 34 s cold for 8,868 people, over the
/// thin client's 120 s read clock, for a panel that paints at most GRID_CAP cards. The
/// `limit` field answers exactly that window: the most-photo people first, covers paid
/// for the returned rows only. No `limit` keeps the reply's exact shape (every person,
/// id order), so every thin client reading the verb today is untouched.
#[test]
fn faces_service_persons_answers_the_paintable_window() {
    let Some(models) = face_models_dir() else {
        eprintln!("SKIP: the pinned face models are absent (faces_service gate)");
        return;
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/faces");
    let work = tempfile::tempdir().expect("a work folder");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    let consent = consent_file(work.path());

    // The trio merges at this daemon's 0.45 pin; min_faces:1 lets the two singletons
    // be cards too — the fixture's three persons carry counts [3, 1, 1].
    let mut daemon = Daemon::start_with(&index, &models, &consent, &["--faces-min-faces", "1"]);
    let roots = json!({"roots": [fixtures.to_string_lossy()]});
    assert_eq!(
        daemon.request("scan", roots, 600)["ok"],
        true,
        "scan answers"
    );

    let counts = daemon.request("counts", json!({}), 60);
    assert_eq!(
        counts["persons"], 3,
        "min_faces:1 makes three cards: {counts}"
    );

    // 1. No limit: every person, id order, no covers asked — no cover fields.
    let persons = daemon.request("persons", json!({}), 60);
    let rows = persons["persons"].as_array().expect("a persons array");
    assert_eq!(rows.len(), 3);
    let ids: Vec<i64> = rows
        .iter()
        .map(|p| p["id"].as_i64().expect("an id"))
        .collect();
    assert_eq!(
        ids,
        {
            let mut sorted = ids.clone();
            sorted.sort();
            sorted
        },
        "the plain list keeps its id order"
    );
    assert!(
        rows.iter().all(|p| p.get("cover_face").is_none()),
        "no covers asked, none paid"
    );

    // 2. Covers without a limit: every row pays its cover face (the old shape).
    let persons = daemon.request("persons", json!({"covers": true}), 60);
    let rows = persons["persons"].as_array().expect("a persons array");
    assert_eq!(rows.len(), 3);
    for row in rows {
        let cover = row["cover_face"]
            .as_object()
            .expect("a cover face per card");
        assert!(
            cover.get("box").is_some() && cover.get("path").is_some(),
            "the cover row: {cover:?}"
        );
    }

    // 3. The window: top N by photo count (the panel's paint order), covers included.
    let persons = daemon.request("persons", json!({"covers": true, "limit": 2}), 60);
    let rows = persons["persons"].as_array().expect("a persons array");
    assert_eq!(rows.len(), 2, "the window is what the client asked for");
    let (first, second) = (rows[0].clone(), rows[1].clone());
    assert_eq!(
        rows[0]["count"], 3,
        "the biggest cluster leads the window: {first}"
    );
    assert_eq!(
        rows[1]["count"], 1,
        "then the singletons, id order on ties: {second}"
    );
    assert!(
        first["id"].as_i64() < second["id"].as_i64(),
        "id order breaks the count tie"
    );
    for row in rows {
        assert!(
            row["cover_face"].as_object().is_some(),
            "window rows carry their cover"
        );
    }

    // 4. A window wider than the store answers every person; limit 0 answers none
    //    (the engine relays the number it is given — it never edits it).
    let persons = daemon.request("persons", json!({"limit": 9}), 60);
    assert_eq!(persons["persons"].as_array().expect("rows").len(), 3);
    assert_eq!(
        persons["persons"].as_array().expect("rows")[0]["count"],
        3,
        "a window over the whole store is still the count-desc window"
    );
    let persons = daemon.request("persons", json!({"limit": 0}), 60);
    assert_eq!(persons["persons"].as_array().expect("rows").len(), 0);

    // 5. The window pays covers for its rows only: count is the row field, never a cover.
    let persons = daemon.request("persons", json!({"limit": 1}), 60);
    let rows = persons["persons"].as_array().expect("a persons array");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["count"], 3,
        "the window leads with the biggest cluster"
    );
    assert!(
        rows[0].get("cover_face").is_none(),
        "covers stay unpaid when not asked"
    );

    daemon.quit();
}
