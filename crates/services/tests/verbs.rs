//! Issue #7a — the scoped sync verb. The stdin thread parses one line at a time; these
//! pins ride the seam `read_commands` uses (`apply_verb` + `Flags::take_sync`): a scoped
//! `sync ["<dir>", …]` carries its dirs (merged, deduplicated) to the next sync; a plain
//! `sync` outranks a pending scope (the whole set of folders is the engine's pinned full
//! behavior); unknown/malformed verbs pass over, as the engine's `readCommands` passed over
//! everything it did not know; the old verbs keep answering exactly as pinned.

use std::path::PathBuf;

use digup_services::index_service::Flags;

#[test]
fn a_scoped_sync_verb_carries_its_dirs_and_a_plain_sync_outranks_them() {
    let flags = Flags::new(true);
    // A fresh helper asks for the whole set the moment it starts.
    assert_eq!(flags.take_sync().unwrap(), Vec::<PathBuf>::new());
    // Scoped verb: dirs merged, deduplicated, kept for the next sync.
    digup_services::index_service::apply_verb(&flags, r#"sync ["/a/b", "/c"]"#);
    digup_services::index_service::apply_verb(&flags, r#"sync ["/c", "/d/e"]"#);
    assert_eq!(
        flags.take_sync().unwrap(),
        vec![
            PathBuf::from("/a/b"),
            PathBuf::from("/c"),
            PathBuf::from("/d/e")
        ]
    );
    // A scoped request not taken yet, then a plain sync: the whole set wins.
    digup_services::index_service::apply_verb(&flags, r#"sync ["/only"]"#);
    digup_services::index_service::apply_verb(&flags, "sync");
    assert_eq!(flags.take_sync().unwrap(), Vec::<PathBuf>::new());
    // No request: nothing to take.
    assert!(flags.take_sync().is_none());
    // Unknown and malformed verbs are passed over.
    digup_services::index_service::apply_verb(&flags, "sync not-json");
    digup_services::index_service::apply_verb(&flags, "sync [1, 2]");
    digup_services::index_service::apply_verb(&flags, "nonsense");
    assert!(flags.take_sync().is_none());
    // The old verbs keep answering as pinned.
    digup_services::index_service::apply_verb(&flags, "backfill off");
    assert!(!flags.backfill());
    digup_services::index_service::apply_verb(&flags, "backfill on");
    assert!(flags.backfill());
    digup_services::index_service::apply_verb(&flags, "sync code");
    assert!(flags.take_code_sync());
    digup_services::index_service::apply_verb(&flags, "stop");
    assert!(flags.stop());
}
