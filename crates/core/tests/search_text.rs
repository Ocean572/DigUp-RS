//! The letter set keyword search and the embedding gate run on (SearchText.swift).
//!
//! These pins are the `hasWords`/`hasProse`/`inflects`/`PageTextProblem.check` facts the
//! audit verified on 2026-11-13, each chosen so the shape it pins is the one that differs
//! from the plain "letters are L*" reading:
//!
//! * `hasWords` counts marks as letters (SearchText.swift:39, `CharacterSet.letters`, and
//!   the port's own ⑩a `facts-sets` measurement). A Devanagari/Bengali run whose letters are
//!   mostly matras answers "words worth a vector" — and a table of numbers does not.
//! * `hasProse`'s four-words test is the SAME set (SearchText.swift:51,
//!   `word.unicodeScalars.contains(where: CharacterSet.letters.contains)`), so words of marks
//!   count as words with a letter in them.
//! * `PageTextProblem.check`'s lam-ligature test is Swift's `Script.arabic` AND
//!   `properties.isAlphabetic`: the harakat count, the six non-alphabetic marks in the block
//!   do not (measured with Perl's Unicode 16 data, `search_text.rs:is_arabic_alphabetic`).

use digup_core::search_text::{has_prose, has_words, inflects, PageTextProblem};

/// Bengali matras and Devanagari vowel signs: 2 letters, 6 marks, 8 digits. Marks as letters
/// → 8 vs 8 others (32 ≥ 8): words. L* alone → 2 letters vs 14 others: not words, and the
/// chunk would get keyword search only (`indexer.rs` gates embedding on `has_prose`).
#[test]
fn combining_marks_count_as_letters_in_the_words_test() {
    let matras: String = [
        '\u{09C7}', '\u{09BE}', '\u{09CD}', '\u{09BC}', '\u{09C4}', '\u{0951}',
    ]
    .iter()
    .collect();
    let text = format!("কস{matras}12345678");
    assert!(
        has_words(&text),
        "marks count as letters (SearchText.swift:33-41): a Bengali page of matras is text"
    );

    let numbers = "1234 5678 90.12 34.56 7890";
    assert!(
        !has_words(numbers),
        "a table of numbers says nothing in words and gets keyword search only"
    );
}

/// `hasProse`: four words with a letter in them, where a "letter" is the letters-and-marks
/// set. Four words of marks alone pass in Swift (marks are letters there) and fail on L*.
#[test]
fn the_four_words_test_counts_words_made_of_marks() {
    let marks_only = "\u{0981} \u{0982} \u{0983} \u{09CD}";
    let text = format!("ক {marks_only}");
    assert!(
        has_words(&text),
        "the letters test is the marks-inclusive set"
    );
    assert!(
        has_prose(&text),
        "`hasProse` counts a word whose only letter is a mark (SearchText.swift:51)"
    );

    // A stub passes the letters test and is refused by the word count: `DEBUG=true`,
    // `root = true` (SearchText.swift:45-47).
    assert!(!has_prose("DEBUG=true root=true"));
}

/// The scripts that attach endings to words get prefix keyword matching (SearchText.swift:29,
/// `Script.indic.contains(...) || Script.arabic.contains(...)`). The ranges are CoreFoundation's
/// script groupings Swift's `Script` values are generated from: the classic Indic block
/// U+0900-U+0DFF (Devanagari … Sinhala) and the Arabic block U+0600-U+06FF. The UCD
/// `sc=Arabic`/Indic ranges beyond them (Arabic Supplement U+0750-U+077F, Extended-A
/// U+08A0-U+08FF, Saurashtra U+A880-U+A8DF) are NOT part of those groupings — and this box has
/// no Swift toolchain to measure it, so the port keeps the ranges and names the gap.
#[test]
fn inflecting_scripts_are_arabic_and_the_classic_indic_block() {
    assert!(inflects("কুমির")); // Bengali
    assert!(inflects("किताबें")); // Devanagari
    assert!(inflects("مدرسة")); // Arabic
    assert!(
        !inflects("payment gateway"),
        "Latin words are matched whole, not as prefixes"
    );
}

/// The lam-ligature shape Swift's own comment names: "الأردن" read "ا?ردن", "المبيعات"
/// "ا.بيعات" (SearchText.swift:183-186). Two of them trip `lostLetters`.
#[test]
fn a_bare_alef_before_a_dot_or_question_mark_is_lost_letters() {
    let garbled = "ا?ردن ا.بيعات";
    assert_eq!(
        PageTextProblem::check(garbled),
        Some(PageTextProblem::LostLetters),
        "the lam ligature: the article's lam goes with the ligature"
    );
    // A real abbreviation (Prof. Dr.) and a bare alef with its own marks: not lost letters.
    assert_eq!(PageTextProblem::check("أ.د."), None);
}

/// The alphabetic half of the check is the letters AND the harakat (the marks Swift's
/// `properties.isAlphabetic` answers yes for): a mark in the ligature's place is a lost letter
/// exactly as a consonant is. L* alone misses every one of these.
#[test]
fn harakat_are_alphabetic_for_the_lost_letters_test() {
    let harakat = "\u{064B}"; // fathatan: Mn, Alphabetic=yes (measured)
    let text = format!("ا?{harakat} ا.{harakat}");
    assert_eq!(
        PageTextProblem::check(&text),
        Some(PageTextProblem::LostLetters),
        "the harakat count as alphabetic in U+0600-U+06FF (Perl UCD 16 sweep)"
    );

    // And the six marks in the block that are NOT alphabetic do not: U+06DF, U+06E0,
    // U+06EA, U+06EB, U+06EC, U+0658. A letters-and-marks set without the exceptions
    // reports lost letters here, where the engine sees nothing wrong.
    let quiet = "ا?\u{06DF} ا.\u{06DF} ا?\u{06E0} ا.\u{06E0} ا?\u{0658} ا.\u{0658}";
    assert_eq!(
        PageTextProblem::check(quiet),
        None,
        "the non-alphabetic marks of the block are not letters (search_text.rs:is_arabic_alphabetic)"
    );
}

/// Bengali/Hindi read in glyph order: dependent vowel signs with no consonant in front
/// (SearchText.swift:180-182, 202). Ten of them, the first with a space before it.
#[test]
fn vowel_signs_outside_their_consonants_are_glyph_order() {
    let matras: String = ['\u{09C7}'; 10].iter().collect();
    assert_eq!(
        PageTextProblem::check(&matras),
        Some(PageTextProblem::GlyphOrder),
        "each dependent vowel sign must follow a consonant (or its nukta)"
    );

    let wellformed: String = ['\u{0995}', '\u{09C7}'].iter().cycle().take(10).collect();
    assert_eq!(
        PageTextProblem::check(&wellformed),
        None,
        "ক + E repeated is a real word's shape"
    );
}
