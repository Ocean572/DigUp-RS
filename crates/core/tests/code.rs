//! `CodeExtractor` (CodeExtractor.swift) in Rust, pinned against the engine's own answers.
//!
//! The wide gate is the fixture cross-check (`/tmp/opencode/codecheck`: 19 files — blocks, shell
//! closers, CRLF, a minified bundle, generator marks, a private key, accents and ZWJ emoji,
//! notebooks in both formats, a git repo, latin-1 — whose JSON lines the engine and the port wrote
//! identically). These tests keep the shapes that gate proved inside the workspace.

use std::path::{Path, PathBuf};

use digup_core::code;
use digup_core::extract::ExtractError;
use digup_core::options::IndexOptions;
use unicode_segmentation::UnicodeSegmentation;

fn graphemes(text: &str) -> usize {
    UnicodeSegmentation::graphemes(text, true).count()
}

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-code-{name}"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn write(folder: &Path, name: &str, body: &[u8]) -> PathBuf {
    let path = folder.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn line_count_counts_the_last_line_when_the_file_ends_without_one() {
    assert_eq!(code::line_count(""), 0);
    assert_eq!(code::line_count("one\n"), 1);
    assert_eq!(code::line_count("one\ntwo"), 2);
    assert_eq!(code::line_count("one\ntwo\n"), 2);
    // CR-LF is one Character in Swift, so a CRLF file's last Character is not "\n": one line more.
    assert_eq!(code::line_count("one\r\ntwo\r\n"), 3); // 2 newlines + the CRLF suffix's +1
    assert_eq!(code::line_count("one\r\ntwo\r\nthree"), 3); // 2 newlines + the final line
                                                            // A lone carriage return is ordinary text.
    assert_eq!(code::line_count("one\rtwo\n"), 1);
}

#[test]
fn stretches_end_where_the_code_does() {
    // Six functions, each followed by a blank line, in a budget that needs several cuts.
    let mut text = String::new();
    for group in 0..6 {
        text.push_str(&format!(
            "static int group_{group}(int *out) {{\n  int value = {group};\n  out[0] = value;\n}}\n\n"
        ));
    }
    let pieces = code::lines(&text, 120);
    assert!(
        pieces.len() >= 2,
        "the file is longer than one stretch: {pieces:?}"
    );
    let lines: Vec<&str> = text.split('\n').collect();
    for piece in &pieces {
        // A piece is whole lines, from its first to its last, and fits the budget.
        let body = lines[piece.first as usize - 1..piece.last as usize].join("\n");
        assert_eq!(
            body, piece.text,
            "piece {}-{} is whole lines",
            piece.first, piece.last
        );
        assert!(
            graphemes(&piece.text) <= 120,
            "the piece fits the budget: {}",
            piece.text.len()
        );
    }
    // Every cut lands where a new block starts at the left margin.
    for piece in pieces.iter().skip(1) {
        assert!(
            lines[piece.first as usize - 1].starts_with("static int group_"),
            "the cut is a block boundary: {:?}",
            pieces
        );
    }
}

#[test]
fn a_stretch_cut_mid_block_hands_its_last_two_lines_to_the_next() {
    // One block with no blank lines and no left-margin starts: the cut must overlap.
    let body = (0..40)
        .map(|index| format!("  value[{index}] = {index} * 2;"))
        .collect::<Vec<String>>()
        .join("\n");
    let text = format!("void fill(int *value) {{\n{body}\n}}\n");
    let pieces = code::lines(&text, 200);
    assert!(pieces.len() >= 2, "{pieces:?}");
    assert!(
        pieces.windows(2).any(|pair| pair[1].first < pair[0].last),
        "the mid-block cut hands lines on: {pieces:?}"
    );
    for piece in &pieces {
        assert!(!piece.text.is_empty());
        assert!(piece.first >= 1 && piece.first <= piece.last);
    }
}

#[test]
fn a_line_past_the_budget_is_a_stretch_of_its_own_and_the_piece_caps_at_4000() {
    let long = "x".repeat(5000);
    let text = format!("int a = 1;\n{long}\nint b = 2;\n");
    let pieces = code::lines(&text, 300);
    assert!(
        pieces
            .iter()
            .any(|piece| graphemes(&piece.text) == code::LONGEST_PIECE),
        "{pieces:?}"
    );
    assert!(pieces
        .iter()
        .all(|piece| graphemes(&piece.text) <= code::LONGEST_PIECE));
}

#[test]
fn shell_closers_belong_to_the_block_above_them() {
    let text = "for name in \"$@\"; do\n  make \"$name\"\n  echo done\ndone\nfi\n";
    let pieces = code::lines(text, 30);
    assert!(pieces.len() >= 2, "{pieces:?}");
    for piece in pieces.iter().skip(1) {
        let first = piece
            .text
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(!["done", "fi"].contains(&first.as_str()), "{piece:?}");
    }
}

#[test]
fn code_a_program_made_or_holding_a_key_is_left_out() {
    let options = IndexOptions::default();
    let folder = temp_folder("refusals");
    let generated_late = "// pad\n// @generated\nfunc g() {}\n".to_string();
    let minified = format!("// bundle\n{}", "a=b;".repeat(4000));
    let minified_crlf = format!("// bundle\r\n{}", "a=b;".repeat(4000));
    let cases: Vec<(String, Vec<u8>, &str)> = vec![
        (
            "gen.go".into(),
            b"// Code generated by protoc-gen-go. DO NOT EDIT.\npackage x\n".to_vec(),
            "made by a program",
        ),
        (
            "late.go".into(),
            generated_late.into_bytes(),
            "made by a program",
        ),
        (
            "key.pem".into(),
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n".to_vec(),
            "holds a private key",
        ),
        (
            "bundle.min.js".into(),
            minified.clone().into_bytes(),
            "minified",
        ),
        // CRLF: Swift's `split(separator: "\n")` does not cut at a CR-LF Character, so the bundle is one line.
        (
            "crlf.min.js".into(),
            minified_crlf.into_bytes(),
            "minified (",
        ),
        (
            "blob.c".into(),
            {
                let mut bytes = b"int main() {}\n".to_vec();
                bytes.extend(vec![0u8; 40]);
                bytes
            },
            "not text",
        ),
    ];
    for (name, body, why) in cases {
        let path = write(&folder, &name, &body);
        let outcome = match code::read(&path, &options) {
            Err(ExtractError::LeftOut { why }) => why,
            other => panic!(
                "{name}: read answered {:?}",
                other.map(|file| file.pieces.len())
            ),
        };
        assert!(outcome.contains(why), "{name}: {outcome}");
    }
    // Marks past the first 1000 characters are not marks to the engine.
    let late = format!(
        "// {}\n// @generated by hand\nfunc h() {{}}\n",
        "pad ".repeat(400)
    );
    let path = write(&folder, "handwritten.go", late.as_bytes());
    assert!(
        code::read(&path, &options).is_ok(),
        "a mark past 1,000 characters is not read"
    );
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn a_repo_doc_is_titled_by_its_path_in_the_repo() {
    let folder = temp_folder("place");
    let repo = folder.join("billing");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let path = write(&repo, "payments/retry.py", b"def retry():\n    pass\n");
    let (repo_name, title) = code::place(&path.to_string_lossy());
    assert_eq!(repo_name.as_deref(), Some(repo.to_string_lossy().as_ref()));
    assert_eq!(title, "billing/payments/retry.py");

    let loose = write(&folder, "loose.c", b"int loose(void) { return 0; }\n");
    let (repo_name, title) = code::place(&loose.to_string_lossy());
    assert_eq!(repo_name, None);
    assert_eq!(
        title,
        format!("{}/loose.c", folder.file_name().unwrap().to_string_lossy())
    );
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn notebooks_read_their_cells_and_ignore_their_outputs() {
    // `r##` because the JSON carries `"#` (the markdown cell): one `#` would end the raw string.
    let nb4 = r##"{"cells":[
        {"cell_type":"code","source":["a = 1\n","b = 2\n"],"outputs":[{"text/plain":["3"]}]},
        {"cell_type":"markdown","source":"# notes"},
        {"cell_type":"code","source":[]},
        {"cell_type":"code","source":["c = a + b\n"]}
    ],"nbformat":4}"##;
    assert_eq!(
        code::notebook_cells(nb4.as_bytes()).unwrap(),
        vec!["a = 1\nb = 2\n", "# notes", "", "c = a + b\n"]
    );

    // nbformat 3 keeps its cells in worksheets, with the code under "input".
    let nb3 =
        r#"{"worksheets":[{"cells":[{"cell_type":"code","input":["print(1)\n"]}]}],"nbformat":3}"#;
    assert_eq!(
        code::notebook_cells(nb3.as_bytes()).unwrap(),
        vec!["print(1)\n"]
    );

    assert!(matches!(
        code::notebook_cells(b"{ not json ]"),
        Err(ExtractError::Unreadable { why }) if why == "not a notebook"
    ));

    // Whole cells to a stretch: empty cells passed over, a long cell cut into line stretches that
    // all point at the same cell.
    let cells = vec![
        "one".to_string(),
        String::new(),
        "two".to_string(),
        "x".repeat(500),
        "tail".to_string(),
    ];
    let pieces = code::notebook_pieces(&cells, 200);
    assert_eq!((pieces[0].first, pieces[0].last), (1, 3));
    assert_eq!(pieces[0].text, "one\n\ntwo");
    assert!(
        pieces
            .iter()
            .skip(1)
            .take_while(|piece| piece.first == 4)
            .all(|piece| piece.last == 4),
        "{pieces:?}"
    );
    assert_eq!(pieces.last().unwrap().first, 5);
}

#[test]
// F9 — the LINE trims ride Foundation's `.whitespaces` set, measured on swift-corelibs
// (the engine's Linux shape): the ⑩a pinned facts f046/f047 (fixtures/search-expected)
// show CR/VT/FF/NEL/LS/PS are NOT in it — a trailing CR SURVIVES
// `trimmingCharacters(in: .whitespaces)`. The audit-base trimmed with `str::trim`
// (Rust's whitespace = White_Space property ⊃ CR/VT/FF/NEL/LS/PS), which
//  - cut the trailing CR off summary lines ("a\r" -> "a", engine: "a\r"), and
//  - flipped closer verdicts: `"end\r"` trimmed to `"end"` counted as the closer, so a
//    stretch cut BEFORE the `end` line where the engine (trim keeps `"end\r"`, not a
//    closer) ends the stretch AFTER it.
// macOS (the Darwin engine) trims the controls and keeps ZWSP — the split
// `search_text::is_whitespace_set` answers that shape there (cfg), pinned below.
fn the_line_trim_is_the_whitespaces_set() {
    // Summary: the CR survives (str::trim fails-before: "b" — with "a" emptied, the
    // code pick lands on the "b" line).
    assert_eq!(code::summary("a\r\r\nb\n"), "a\r");
    // Lines (closer verdict): the engine trims `"end\r"` to `"end\r"` (CR survives
    // `.whitespaces`, f9full + the pinned facts f046/f047) — NOT the closer — so its
    // `startsBlock` of that line FLIPS against the audit-base's str::trim answer.
    // Traced line-for-line against CodeExtractor.swift:82-108 the flip cannot move a
    // piece boundary: a candidate the engine takes via `startsBlock(line)` is re-cut by
    // the fallback search at the same line through its `blank(line - 1) ||` disjunct
    // (search 1 proved that blank when the line qualified), and a candidate the engine
    // rejects (closer flip) is likewise re-cut through `blank`. The closer verdict
    // reaches no other output. Pinned below: the traced engine shape, equal on both
    // trims — fixture "a = 1\nb = 2\r\nend\rc = 3\n", size 12 (offsets 0,6,12,21,26;
    // end=2; floor 4; search 1 cuts at 2 via blank(1); handover start=2; blank-skip
    // skips line 2 (blank on both trims: Cf is whitespace-true in `allSatisfy`), piece
    // 2 = lines 3..3):
    let pieces = code::lines("a = 1\nb = 2\r\nend\rc = 3\n", 12);
    assert_eq!(
        pieces
            .iter()
            .map(|piece| (piece.text.clone(), piece.first, piece.last))
            .collect::<Vec<(String, i64, i64)>>(),
        vec![
            ("a = 1\nb = 2".to_string(), 1, 2),
            ("end\rc = 3".to_string(), 3, 3),
        ],
        "closer verdict flips; boundaries ride the blank rules on both trims (traced)"
    );
    // Leading-character check (`first.isWhitespace` == White_Space + tab, ZWSP true on
    // corelibs, f9probe): a ZWSP-led line is NOT a block start on BOTH platforms.
    let pieces = code::lines("\u{200B}end\nx = 1\n", 18);
    assert_eq!(
        pieces
            .iter()
            .map(|p| (p.first, p.last))
            .collect::<Vec<(i64, i64)>>(),
        vec![(1, 2)],
        "the ZWSP-led `end` line joins the stretch (leading whitespace, both sets)"
    );
}

#[test]
// F9 — the CRLF/grapheme and ZWSP channels, measured on a Mac with the pinned Xcode
// (`wsprobe.swift`/`probe2.swift`, 2026-11-15). Swift merges `\r\n` into ONE `Character`
// (`"\r\n".count == 1`), so `split` drops the whole cluster and a lone `\r` survives as text;
// `.whitespaces` (Apple's Foundation) keeps a CR and trims ZWSP. Both facts hold on every
// Foundation family, so this shape is not platform-specific — the Linux engine answers it the
// same way (the grapheme model is stdlib; the trim set matches corelibs).
fn the_line_trim_on_macos_is_the_darwin_set() {
    // Swift splits `\r\n` as one line-ending and keeps the lone `\r`: lines ["a\r", "b"].
    // `.whitespaces` keeps the CR, so the code pick is the first line, "a\r" (not "b", not
    // "a\r\r"): the earlier macOS-pinned "b" was a hand-guess with no Mac to check it on.
    assert_eq!(code::summary("a\r\r\nb\n"), "a\r");
    let pieces = code::lines("def a():\n    x = 1\nend\r\r\n    y = 2\n", 18);
    // The traced handover shape: "end\r\r\n" splits the `\r\n` cluster so the line is `end\r`,
    // `.whitespaces` keeps its CR, so it is NOT the closer `end` and starts a new block.
    assert_eq!(
        pieces
            .iter()
            .map(|p| (p.first, p.last))
            .collect::<Vec<(i64, i64)>>(),
        vec![(1, 1), (2, 3), (3, 4)],
        "`end\\r` keeps its CR (not the closer); the handover shape the Mac measures"
    );
}

#[test]
// F9 — ZWSP (U+200B) is IN `.whitespaces` on BOTH Foundation families: swift-corelibs
// (f9full.swift, 2026-11-14: 19 scalars — tab, space, the Zs separators, and 200B; ZWJ 200D
// NOT) and Apple's Foundation (measured on a Mac, `wsprobe.swift` 2026-11-15:
// `"\u{200B}a\u{200B}".trimmingCharacters(in: .whitespaces)` is `"a"`). So a summary line led
// by ZWSP trims to its `def` line on every platform; the engine picks it as the definition.
// The audit-base's `str::trim` (Rust's White_Space excludes ZWSP) kept the ZWSP, the definition
// test failed on the glued token "\u{200B}def", and the summary returned the ZWSP-led line.
fn the_summary_trim_takes_a_leading_zero_width_space() {
    let s = code::summary("\u{200B}def f():\n");
    assert_eq!(
        s, "def f():",
        "ZWSP trims on every platform (200B IS in .whitespaces on corelibs and Apple)"
    );
}

#[test]
fn the_summary_is_the_first_line_that_defines_something() {
    let stretch = "import time\n\n\ndef retry_with_backoff(fn, tries=3):\n    delay = 1\n";
    assert_eq!(
        code::summary(stretch),
        "def retry_with_backoff(fn, tries=3):"
    );
    assert_eq!(code::summary("# a comment\nx = 1\n"), "x = 1");
    // A file of nothing but excluded lines reads as its first line.
    assert_eq!(code::summary("}\n)\n@decorated\n"), "}");
    assert_eq!(graphemes(&code::summary(&"y".repeat(200))), 161); // 160 characters + the ellipsis
    assert!(code::defines_something("def") && code::defines_something("#define"));
    assert!(!code::defines_something("let") && !code::defines_something("val"));
}

#[test]
fn the_language_table_is_the_engines() {
    for (extension, language) in [
        ("py", "Python"),
        ("tsx", "TypeScript"),
        ("hpp", "C++ header"),
        ("ipynb", "Notebook"),
        ("toml", "TOML"),
        ("mk", "Makefile"),
        ("pyi", "Python"),
        ("s", "Assembly"),
    ] {
        assert_eq!(
            code::known_language(extension),
            Some(language),
            "{extension}"
        );
    }
    assert_eq!(code::language_of("/a/b/retry.py"), "Python");
    assert_eq!(code::language_of("/a/b/Makefile"), "Makefile");
    assert_eq!(code::language_of("/a/b/no_extension"), "Text");
    assert_eq!(code::language_of("/a/b/odd.lang"), "LANG");
}

#[test]
fn a_notebook_past_the_byte_limit_is_left_out() {
    let folder = temp_folder("notebook-limit");
    let path = write(
        &folder,
        "big.ipynb",
        br#"{"cells":[{"cell_type":"code","source":"x = 1\nx = 2\n"}],"nbformat":4}"#,
    );
    let options = IndexOptions {
        max_code_bytes: 4,
        ..IndexOptions::default()
    };
    let outcome = match code::read(&path, &options) {
        Err(error) => error,
        Ok(_) => panic!("left out"),
    };
    assert!(
        format!("{outcome}").contains("cells over 0 MB"),
        "{outcome}"
    );
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn latin1_bytes_read_as_latin1() {
    // Swift's read: strict UTF-8, else ISO-Latin-1 (every byte a character).
    let folder = temp_folder("latin1");
    let path = write(&folder, "notes.md", b"na\xEFve caf\xE9\n");
    let file = code::read(&path, &IndexOptions::default()).expect("readable");
    let text: String = file
        .pieces
        .iter()
        .map(|piece| piece.text.clone())
        .collect::<Vec<String>>()
        .join("\n");
    assert!(text.contains("caf"), "{text:?}");
    let _ = std::fs::remove_dir_all(&folder);
}
