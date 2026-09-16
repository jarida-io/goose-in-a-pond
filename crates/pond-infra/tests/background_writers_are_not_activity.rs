//! No background writer may stamp `sessions.updated_at`.
//!
//! That column is one of the two activity sources the idle gate reads
//! (`consolidation_schedule::saw_activity_since_start` takes the newest
//! `sessions.updated_at` across the store). Anything the pond writes to a
//! session row on its own initiative therefore has a second, invisible effect:
//! it tells every background job that somebody just came back.
//!
//! For the two title writers that meant a rename would cancel the sweep that
//! was doing the renaming. For the batch extraction cursor it is worse, because
//! that writer runs once per WINDOW of every conversation in the backlog: a
//! pass that stamped `updated_at` would trip its own activity watcher within
//! seconds of starting, cancel itself, push the idle clock forward by fifteen
//! minutes, and do that on every pass for the life of the process. The engine
//! would look enabled, log nothing wrong, and never finish a pass.
//!
//! # Why a source grep on top of the behavioural tests
//!
//! `sqlite_session_storage.rs` already carries
//! `background_title_writes_are_not_mistaken_for_user_activity` and
//! `extraction_cursor_writes_are_not_mistaken_for_user_activity`, and those are
//! the stronger instrument: they run the SQL and read the column back. This
//! exists because of how the mistake is actually made. Nobody deletes a passing
//! test; what happens is that a fifth writer gets added next to the four below,
//! copied from `update_title` rather than from `set_derived_title`, and it has
//! no behavioural test of its own because nobody thought to write one. A grep
//! over the statements themselves fails on the copy, not on the omission.
//!
//! `include_str!` creates no dependency edge and needs no link, so this reads
//! the adapter as text in the crate that owns it.

const STORAGE: &str = include_str!("../src/sqlite_session_storage.rs");

/// The methods that write a session row on the pond's OWN initiative, and must
/// therefore leave the activity clock alone.
///
/// Each entry is the method's `fn` signature line as it appears in the source.
/// A method renamed out from under this list fails
/// `every_named_background_writer_still_exists` below rather than silently
/// passing, which is the whole reason the guard against the guard is here.
const BACKGROUND_WRITERS: &[&str] = &[
    "async fn set_derived_title(",
    "async fn set_generated_title(",
    "async fn set_extraction_cursor(",
    "async fn note_extraction_attempt(",
];

/// Methods that SHOULD stamp the clock, because a person did the thing.
///
/// The vacuity control. Without it, this whole file passes against an adapter
/// that never writes `updated_at` anywhere -- in which case the idle gate has no
/// activity source at all and every background job runs while the household is
/// mid-conversation.
const REAL_ACTIVITY: &[&str] = &["async fn update_title(", "async fn add_message("];

/// Return the body of the method whose signature line contains `signature`,
/// from that line to the start of the next method at the same indentation.
///
/// Crude on purpose: the alternative is parsing Rust, and what this needs to
/// know is only "does the word `updated_at` appear between here and the next
/// `async fn`". A body that ends early makes the guard weaker, never wrong,
/// and `every_named_background_writer_still_exists` catches the case where it
/// finds nothing at all.
fn method_body<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
    let start = source.find(signature)?;
    let rest = &source[start + signature.len()..];
    let end = rest.find("\n    async fn ").unwrap_or(rest.len());
    Some(&rest[..end])
}

#[test]
fn no_background_session_writer_stamps_the_activity_clock() {
    for signature in BACKGROUND_WRITERS {
        let body = method_body(STORAGE, signature)
            .unwrap_or_else(|| panic!("{signature} is not in sqlite_session_storage.rs"));
        assert!(
            !body.contains("updated_at = datetime('now')"),
            "{signature} stamps `sessions.updated_at`. That column is an activity source: \
             a background writer touching it reads to every lane job as a person coming \
             back, so the pass cancels itself and pushes the idle clock forward. Write the \
             row without it, as `set_derived_title` does."
        );
    }
}

#[test]
fn every_named_background_writer_still_exists() {
    for signature in BACKGROUND_WRITERS {
        assert!(
            STORAGE.contains(signature),
            "{signature} is no longer in sqlite_session_storage.rs -- this guard is now \
             checking nothing. Update the list in the same change that renamed it."
        );
    }
}

/// Vacuity control: the adapter really does stamp the clock when a PERSON acts.
///
/// Renaming a conversation by hand and sending a message are both activity, and
/// if neither wrote `updated_at` the assertions above would hold for the wrong
/// reason -- against a store the idle gate could read no activity from at all.
#[test]
fn a_persons_own_writes_still_stamp_the_activity_clock() {
    for signature in REAL_ACTIVITY {
        let body = method_body(STORAGE, signature)
            .unwrap_or_else(|| panic!("{signature} is not in sqlite_session_storage.rs"));
        assert!(
            body.contains("updated_at = datetime('now')"),
            "{signature} no longer stamps `sessions.updated_at`. If that is deliberate the \
             idle gate has lost an activity source and background jobs will run while \
             somebody is mid-conversation; if it is not, this guard has gone vacuous."
        );
    }
}
