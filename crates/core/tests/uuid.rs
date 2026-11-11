//! `UUID().uuidString`, the name the engine gives the pictures it writes. Measured on this box with
//! swift-corelibs Foundation (`/tmp/opencode/imagecheck/uuidprobe.swift`): a fresh `UUID()` is
//! CANONICAL form in UPPERCASE hex, version nibble 4, variant nibble 8, 9, A or B.

use digup_core::uuid::uuid_string;

#[test]
fn uuid_is_canonical_uppercase_form() {
    let name = uuid_string();
    assert_eq!(name.len(), 36, "{name}");
    let characters: Vec<char> = name.chars().collect();
    for (index, character) in characters.iter().enumerate() {
        let dash = [8, 13, 18, 23].contains(&index);
        assert!((*character == '-') == dash, "{name}: dash at {index}");
        if !dash {
            assert!(
                character.is_ascii_hexdigit() && !character.is_ascii_lowercase(),
                "{name}: hex is uppercase"
            );
        }
    }
    assert_eq!(characters[14], '4', "{name}: version 4");
    assert!(
        "89AB".contains(characters[19]),
        "{name}: RFC 4122 variant, {name}"
    );
}

#[test]
fn two_names_are_not_the_same_name() {
    // The engine names each preview this way, in the folder the index reads back.
    assert_ne!(uuid_string(), uuid_string());
}
