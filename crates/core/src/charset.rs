//! Issue #169: the legacy-encoding sniff.
//!
//! The pinned Swift rule (`readText` / `DocExtractor`, and `code.rs`'s read) takes strict
//! UTF-8 and falls back to ISO-Latin-1 — every byte a character, measured byte-for-byte in
//! `tests/doc.rs`. That fallback is what turns a GBK/CP936 file into mojibake: the bytes ARE
//! text in a legacy family, Latin-1 just reads them as the wrong characters.
//!
//! The sniff is a DECLINE-BY-DEFAULT gate, not a guess. A family wins only when ALL of:
//!   • its bytes decode CLEANLY in it — no replacement characters; a legacy table that cannot
//!     read a byte pair says so, which is the strongest filter there is;
//!   • the decoded text is plausibly text of that family's OWN script: a share of the script's
//!     letters, ASCII-mixed (legacy text is always ASCII-mixed), and no stray control
//!     characters outside tab/newline/CR;
//!   • (GB18030) the file's byte shape is the GB2312 core grid — the trail-shape gate below.
//! When no family clears the gate the caller keeps its pinned Latin-1, so every
//! Foundation-parity row the suite pins stays byte-for-byte the answer. Windows-1252 is
//! deliberately NOT a candidate: the pinned CP1252 rows read the 0x80–0x9F bytes as the C1
//! controls they are (the euro stays a control) — measured, and the law says the measured
//! answer wins.
//!
//! THE SUPERSET LAW (measured on this box, digit-for-digit). GB18030's two-byte form covers
//! Big5's and Shift-JIS's byte ranges and maps their pairs to CLEAN CJK ideographs — the
//! superset reads a Japanese file as "clean Chinese" with a HIGHER own-script share than the
//! true family (own 18/32 vs 17/32 on the Shift-JIS fixture), so clean-decode + script
//! evidence alone can rank the garbage ABOVE the answer. The discriminator is the BYTE SHAPE:
//! real GB2312/GBK/CP936 text (the #169 corpus) lives almost entirely in the GB2312 core
//! grid — trail bytes 0xA1–0xFE — while Big5 puts half its trails in 0x40–0x7E and Shift-JIS
//! uses the 0x80–0xA0 trails GB18030 cannot read at all. GB18030 therefore wins only with
//! the core-grid trail shape; the files its superset reading covets fall to their own tables.
//!
//! THE DECLINED FAMILIES (each pinned by a test, each an honest decline, not a miss):
//!   • Western single-byte (Latin-1 pins stay — module comment).
//!   • windows-1251 / KOI8-R: Cyrillic high-high pairs land in the GB2312 core grid (trail
//!     shape 1.0) and map to clean CJK ideographs — telling Cyrillic from Chinese needs a
//!     letter-frequency model, its own round. The measured shape: real Cyrillic prose is
//!     full of odd-length word runs, and a lone high byte before a space is an illegal GB
//!     trail, so the clean-decode gate declines real Cyrillic files — the short pinned line
//!     AND a 2 320-character one, in windows-1251 and KOI8-R alike (measured digit-for-digit,
//!     pinned below). And a Cyrillic PROBE cannot close the gap: windows-1251/KOI8-R spell
//!     Cyrillic letters with exactly the high bytes the GB2312 grid uses for Chinese, so a
//!     GBK file decoded as windows-1251 reads ~half Cyrillic — the probe fires on the
//!     Chinese corpus itself.
//!   • EUC-KR / EUC-JP: their grid IS the GB2312 grid (pairs 0xA1–0xFE × 0xA1–0xFE), so no
//!     table-free test separates them from GBK — the Chinese corpus and Korean/Japanese-EUC
//!     files share one shape. Undeclared files of those families ride the CJK reading the
//!     share contest picks (measured and pinned: EUC-KR rides gb18030, EUC-JP rides Big5);
//!     telling them apart is the same frequency-model round.
//! Mail is NOT a client of the sniff: `mail::decoded` is the pinned Swift relay — only
//! iso-8859-1/windows-1252 get their tables and every OTHER declared charset (and none)
//! reads as UTF-8-lossy (MailMessage.swift:217). The cross-engine gate pinned this law on
//! the receipt fixture (`charset=us-ascii` with a UTF-8 em dash: ONE character, never
//! three — the WHATWG label table redirects ascii labels to windows-1252, and reading the
//! em dash "in" it emits the mojibake the gate refused). Declared-charset decoding for the
//! legacy families is a separate, measured round, not #169's.
//!
//! Determinism: a pure function over the file's bytes — fixed candidate order, fixed
//! thresholds, no statistics across files, no per-process state, nothing that changes between
//! runs (the Swift `modalityMeans` noise floor is not invited in here).
//!
//! Tie law: when two families both clear every gate, the higher share of the family's own
//! script's letters wins, and an exact tie falls to the earlier candidate — GB18030, the
//! family the issue's corpus names.

use encoding_rs::Encoding;
use encoding_rs::BIG5;
use encoding_rs::GB18030;
use encoding_rs::SHIFT_JIS;

/// The families the sniff can win for, in priority order: the two whose byte shape is
/// separable from GB18030's superset reading (Big5's 0x40–0x7E trails, Shift-JIS's
/// 0x80–0xA0 trails) behind the corpus family itself. The ranking is by own-script share
/// (the module's tie law below); this is the fixed order a tie falls through, GB18030 first.
const CANDIDATES: &[&Encoding] = &[GB18030, BIG5, SHIFT_JIS];

/// The share of the family's own script letters a candidate needs (of all decoded
/// characters). Kanji-only Japanese text clears Shift-JIS's bar through the kanji slot; the
/// bars exist to decline high-entropy bytes that happen to decode cleanly.
fn min_own_share(name: &str) -> f64 {
    match name {
        // Japanese: kana first (Shift-JIS's own sign), kanji-only text clears the same slot.
        "Shift_JIS" => 0.03,
        // Chinese / Traditional Chinese: the ideograph share.
        _ => 0.05,
    }
}

/// The character-class counts of a decoded candidate.
#[derive(Default)]
struct Tally {
    ascii: usize,   // printable ASCII (space..~)
    ws: usize,      // tab, newline, CR
    control: usize, // any other control/format character
    own: usize,     // the family's own script letters
    total: usize,
}

fn tally(text: &str, encoding: &'static Encoding) -> Tally {
    let mut t = Tally::default();
    for ch in text.chars() {
        t.total += 1;
        let code = ch as u32;
        match ch {
            '\t' | '\n' | '\r' => t.ws += 1,
            _ if (0x20..=0x7E).contains(&code) => t.ascii += 1,
            _ if (code <= 0x1F || code == 0x7F || (0x80..=0x9F).contains(&code)) => t.control += 1,
            _ => {
                if is_own_script(code, encoding) {
                    t.own += 1
                }
            }
        }
    }
    t
}

/// "The family's own script": the letters the family's readers actually type. A family also
/// counts the scripts its text legitimately mixes with (Chinese files mix ASCII; Japanese
/// files mix kana and kanji), so Japanese counts kana + kanji and the Chinese families count
/// the ideographs. The arms for the declined families stay spelled out with them: the ranges
/// document WHICH script each family's evidence would name, for the round that adds the
/// frequency model.
fn is_own_script(code: u32, encoding: &'static Encoding) -> bool {
    const CJK_IDEO: (u32, u32) = (0x4E00, 0x9FFF);
    const CJK_EXT_A: (u32, u32) = (0x3400, 0x4DBF);
    const KANA: (u32, u32) = (0x3040, 0x30FF);
    const CYRILLIC: (u32, u32) = (0x0400, 0x04FF);
    const HANGUL_SYLL: (u32, u32) = (0xAC00, 0xD7AF);
    fn in_range(code: u32, (lo, hi): (u32, u32)) -> bool {
        (lo..=hi).contains(&code)
    }
    // `Encoding::name()` is encoding_rs's exact IANA spelling — "Big5", "Shift_JIS",
    // "gb18030". Matching the wrong case silently zeroes the family's evidence (measured on
    // this box: a "big5" arm fell through to the Cyrillic default and Big5 declined).
    match encoding.name() {
        "gb18030" | "Big5" => in_range(code, CJK_IDEO) || in_range(code, CJK_EXT_A),
        "Shift_JIS" | "EUC-JP" => {
            in_range(code, KANA) || in_range(code, CJK_IDEO) || in_range(code, CJK_EXT_A)
        }
        "EUC-KR" => in_range(code, HANGUL_SYLL),
        _ => in_range(code, CYRILLIC),
    }
}

/// The GB18030 byte-shape gate (module law): the share of the file's two-byte trail bytes
/// that live in the GB2312 core grid (0xA1–0xFE). Real GB2312/GBK/CP936 text is nearly all
/// core; Big5 puts about half its trails in 0x40–0x7E, and Shift-JIS's 0x80–0xA0 trails GB18030
/// cannot read at all (the clean-decode gate declines those files outright). Four-byte forms
/// (0x81–0xFE 0x30–0x39 0x81–0xFE 0x30–0x39 — rare CJK/emoji extensions) count as non-core;
/// real corpus text has almost none, and the bar leaves them room.
fn gb_trail_core_share(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut core = 0usize;
    let mut trails = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let lead = bytes[i];
        // ASCII rides free; GB18030's single-byte euro (0x80) is a one-byte character.
        if lead <= 0x80 {
            i += 1;
            continue;
        }
        let trail = match bytes.get(i + 1) {
            Some(&trail) => trail,
            None => break, // a lead with no byte after it: unpaired, the caller's decode says so
        };
        if (0x40..=0x7E).contains(&trail) || (0xA1..=0xFE).contains(&trail) {
            trails += 1;
            if trail >= 0xA1 {
                core += 1;
            }
            i += 2;
        } else if (0x30..=0x39).contains(&trail) && i + 4 <= bytes.len() {
            trails += 2; // the four-byte form's two digit trails: non-core
            i += 4;
        } else {
            i += 1; // lead without a legal trail: counts against the core shape
        }
    }
    if trails == 0 {
        0.0
    } else {
        core as f64 / trails as f64
    }
}

/// GB18030 wins only above this core-grid share: the corpus shape. Big5's ~50 % and
/// Shift-JIS's 0 % fall below it; clean corpus text sits at ~1.0.
const GB_TRAIL_CORE_MIN: f64 = 0.85;

/// Every byte a character, pinned Swift's way. Public so the callers share ONE spelling.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// What the sniff decided: the family's IANA name (encoding_rs's own spelling, so a log line
/// names the family the file was read in) and the decoded text. Constructed only by `sniff`,
/// which decodes exactly one of its candidate families.
pub struct Sniffed {
    pub encoding: &'static str,
    pub text: String,
}

/// The sniff. `None` = decline: the caller keeps its pinned fallback.
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    if bytes.is_empty() {
        return None;
    }
    // A file with no high bytes is ASCII, which is UTF-8 — nothing to sniff.
    if !bytes.iter().any(|byte| *byte >= 0x80) {
        return None;
    }
    // NULs are the "not text" gate's territory (code.rs) and the pinned UTF-16-no-BOM row's
    // (doc.rs: the NULs ride as the characters they are). The sniff stays out of both.
    if bytes.contains(&0) {
        return None;
    }
    let total_min = 8usize; // a handful of bytes is not evidence of anything
    let mut best: Option<(f64, &'static Encoding, String)> = None;
    for &family in CANDIDATES {
        // A family that needed replacement characters cannot be the answer — a legacy table
        // that cannot read a byte pair says so, and that is the strongest filter there is.
        let (decoded, had_errors) = family.decode_without_bom_handling(bytes);
        if had_errors {
            continue;
        }
        // The superset law: GB18030 takes only the GB2312 core shape.
        if family.name() == "gb18030" && gb_trail_core_share(bytes) < GB_TRAIL_CORE_MIN {
            continue;
        }
        let text = decoded.into_owned();
        let t = tally(&text, family);
        if t.total < total_min || t.control > 0 {
            continue;
        }
        let ascii_share = (t.ascii + t.ws) as f64 / t.total as f64;
        // Legacy text is ASCII-mixed: paths, numbers, newlines, punctuation. A file with no
        // ASCII at all is a binary that happened to decode cleanly — not a text.
        if ascii_share < 0.30 {
            continue;
        }
        let own_share = t.own as f64 / t.total as f64;
        if own_share < min_own_share(family.name()) {
            continue;
        }
        match &best {
            Some((previous, _, _)) if *previous >= own_share => {}
            _ => best = Some((own_share, family, text)),
        }
    }
    best.map(|(_, family, text)| Sniffed {
        encoding: family.name(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // The declined families: pinned as DECLINED/AMBIGUOUS below, so they are fixtures here,
    // not candidates.
    use encoding_rs::EUC_JP;
    use encoding_rs::EUC_KR;
    use encoding_rs::KOI8_R;
    use encoding_rs::WINDOWS_1251;

    fn gbk(text: &str) -> Vec<u8> {
        GB18030.encode(text).0.into_owned()
    }

    #[test]
    fn a_gbk_file_decodes_as_gbk_not_as_latin1() {
        // The #169 shape: a Chinese note, ASCII-mixed the way a real file is.
        let bytes = gbk("会议纪要：今天讨论中文搜索的编码问题 meeting notes 2024\n");
        assert!(gb_trail_core_share(&bytes) > GB_TRAIL_CORE_MIN);
        let sniffed = sniff(&bytes).expect("the GBK family clears the gate");
        assert_eq!(sniffed.encoding, "gb18030");
        assert!(sniffed.text.contains("会议纪要"));
        assert!(sniffed.text.contains("meeting notes 2024"));
        assert!(!sniffed.text.contains('\u{0}'));
    }

    #[test]
    fn pure_ascii_and_nul_files_are_declined_the_pinned_way() {
        assert!(sniff(b"plain ascii text, every byte below 0x80").is_none());
        assert!(sniff(&[b'a', 0, b'b', 0xC7, 0xF1]).is_none());
        assert!(sniff(&[]).is_none());
    }

    #[test]
    fn clean_bytes_that_are_not_text_of_any_family_are_declined() {
        // High-entropy noise: 0xC0..0xD9 pairs are legal GB lead/trail bytes, so a table can
        // "read" them cleanly — the ASCII-mix bar is what declines it.
        let mut bytes: Vec<u8> = Vec::new();
        for i in 0..400u32 {
            bytes.push(0xC0 + (i % 26) as u8);
            bytes.push(0xA1 + ((i * 7) % 90) as u8);
        }
        assert!(sniff(&bytes).is_none());
    }

    #[test]
    fn big5_and_shift_jis_files_survive_the_superset_gate() {
        // The superset law, pinned. Big5 is a TRADITIONAL-Chinese table: the fixture is 繁體.
        let big5 = BIG5
            .encode("會議紀要：今日討論中文搜尋的編碼問題 meeting 2024\n")
            .0
            .into_owned();
        assert!(
            gb_trail_core_share(&big5) < GB_TRAIL_CORE_MIN,
            "the GB18030 trail-shape gate must decline a Big5 file"
        );
        let sniffed = sniff(&big5).expect("Big5 clears the gate");
        assert_eq!(sniffed.encoding, "Big5");
        assert!(sniffed.text.contains("會議紀要"));

        let sjis = SHIFT_JIS
            .encode("会議議事録：今日中文検索について議論 meeting 2024\n")
            .0
            .into_owned();
        let sniffed = sniff(&sjis).expect("Shift-JIS clears the gate");
        assert_eq!(sniffed.encoding, "Shift_JIS");
        assert!(sniffed.text.contains("会議"));
        assert!(sniffed.text.contains("について"));
    }

    #[test]
    fn euc_kr_and_euc_jp_bytes_ride_the_cjk_family_the_grid_is_shared() {
        // The documented ambiguity (module law): the EUC-KR/EUC-JP grid IS the GB2312 grid,
        // so an undeclared Korean/Japanese-EUC file is shape-identical to Chinese — the CJK
        // family the share contest picks takes it (measured: EUC-KR rides gb18030, EUC-JP
        // rides Big5 — the deterministic result of the pure function, pinned verbatim). An
        // improvement over the Latin-1 they used to get, not a full cure; the frequency-model
        // round is the cure. (Mail is the pinned Swift relay, module comment.)
        let euc = EUC_KR
            .encode("회의록: 오늘 한국어 검색 인코딩 논의 meeting 2024\n")
            .0
            .into_owned();
        let sniffed = sniff(&euc).expect("the shared grid decodes cleanly as CJK");
        assert_eq!(sniffed.encoding, "gb18030");
        // The rides are CLEAN CJK readings, not replacement-character garbage that passed a
        // lenient gate: the Hangul letters map to real ideographs (zero control characters
        // in the decoded text, so every one is a letter, a digit, punctuation or whitespace).
        let (decoded, had) = GB18030.decode_without_bom_handling(&euc);
        assert!(!had, "the GB reading is clean");
        assert_eq!(
            tally(&decoded, GB18030).control,
            0,
            "no control characters in the ride"
        );
        let eucjp = EUC_JP
            .encode("会議議事録：今日中文検索について議論 meeting 2024\n")
            .0
            .into_owned();
        let sniffed = sniff(&eucjp).expect("the shared grid decodes cleanly as CJK");
        assert_eq!(sniffed.encoding, "Big5");
        let (decoded, had) = BIG5.decode_without_bom_handling(&eucjp);
        assert!(!had, "the Big5 reading is clean");
        assert_eq!(
            tally(&decoded, BIG5).control,
            0,
            "no control characters in the ride"
        );
    }

    #[test]
    fn cyrillic_bytes_are_declined_to_the_pinned_latin1() {
        // The scoped law, pinned: real Cyrillic runs break the GB18030 clean-decode gate on
        // odd-length word runs (a lone high byte before a space is an illegal GB trail), and
        // even runs that read as clean CJK are indistinguishable from Chinese without a
        // letter-frequency model. The decline is the honest result. (Mail is the pinned
        // Swift relay — `mail::decoded` reads every non-Latin-1/CP1252 declaration as
        // UTF-8, module comment.)
        let koi = KOI8_R
            .encode("Привет, это русский текст report 2024\n")
            .0
            .into_owned();
        assert!(sniff(&koi).is_none());
        let cp = WINDOWS_1251
            .encode("Привет, это русский текст report 2024\n")
            .0
            .into_owned();
        assert!(sniff(&cp).is_none());
        // The long case, pinned as measured (2026-11-28): 2 320 characters of real Cyrillic,
        // windows-1251 and KOI8-R alike, decline too — real prose is full of odd-length word
        // runs, and every lone high byte breaks the clean-decode gate on all three candidates
        // (measured digit-for-digit: cp1251 → gb18030 had, Big5 had, Shift_JIS had; KOI8-R →
        // same). The review round's "long clean Cyrillic rides gb18030" claim did NOT
        // reproduce on this box, and its probe cannot exist anyway (module comment): the
        // decline is the pinned answer for Cyrillic, short or long.
        let long = "Привет, это длинный русский текст для проверки кодировки. ".repeat(40);
        assert!(sniff(&WINDOWS_1251.encode(&long).0).is_none());
        assert!(sniff(&KOI8_R.encode(&long).0).is_none());
    }

    #[test]
    fn the_latin1_helper_is_the_pinned_every_byte_a_character() {
        assert_eq!(latin1(&[0x41, 0xE9, 0x80]), "A\u{00E9}\u{0080}");
    }
}
