//! Tripwire: `main.rs` still spawns the proactive reviewer with the right arguments.
//! CI only `cargo check`s pond-server, so deleting that wiring would otherwise stay green.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

#[test]
fn the_file_this_test_reads_is_the_one_it_thinks_it_is() {
    assert!(
        MAIN.contains("async fn run_server"),
        "MAIN is not pond-server/src/main.rs"
    );
    assert!(
        MAIN.contains("async fn run_proactive_reviewer"),
        "the reviewer loop has been renamed or removed from main.rs; if it was moved to another \
         crate this test needs a new path, and if it was deleted so was PAI-7 P4"
    );
}

#[test]
fn the_reviewer_loop_is_actually_spawned() {
    assert!(
        MAIN.contains("tokio::spawn(run_proactive_reviewer("),
        "nothing spawns the proactive reviewer. `run_proactive_reviewer` is `pub`-less but \
         `dead_code` does not fire for a function reachable from a `tokio::spawn` that was \
         deleted along with it — and if it did, CI never compiles this crate's warnings as \
         errors. PAI-7 P4 is off. Fix the wiring or rewrite the stamp in \
         docs/architecture/pai/07-proactive-intelligence.md."
    );
}

/// Raw events would fill the 24-event brief cap with clock ticks and evict the ones that matter.
#[test]
fn only_reviewable_events_reach_the_reviewers_ring() {
    assert!(
        MAIN.contains("reviewable(&bus_event)"),
        "the reviewer's bus subscriber no longer filters through \
         `proactive_review::reviewable`. `Time` and `Session` events would then fill the ring \
         and crowd out the household facts a review is for"
    );
    // `brief_events` is where presence events about another member are dropped.
    assert!(
        MAIN.contains("review::brief_events(&audience, &drained)"),
        "the brief is no longer built through `brief_events`, which is the only place a \
         presence event naming a different household member is removed. The brief is prose \
         handed straight to the model, so PAI-6's scope clamp cannot catch this"
    );
}

/// A second registry makes `GooseOrchestrator::spawn` refuse every review, which looks correct.
#[test]
fn the_reviewer_uses_the_installed_orchestrator_and_not_one_of_its_own() {
    assert!(
        MAIN.contains("pond_mcp_server::installed_orchestrator_deps()"),
        "the reviewer no longer reads the installed orchestrator deps. If it now builds its \
         own `GooseOrchestrator`, every review will be refused for want of a live parent turn"
    );
    assert!(
        !MAIN.contains("TurnAuthorityRegistry::new()"),
        "something in main.rs constructs a second TurnAuthorityRegistry. There must be exactly \
         one — the adapter's — and both the `delegate` tool and the reviewer must read it"
    );
    assert!(
        MAIN.contains("review::review_authority(&audience, &session_id)"),
        "the reviewer publishes an authority it built itself rather than `review_authority`, \
         which `plan_review` also uses. Two constructions are two ceilings, and if their \
         session ids drift every spawn is refused"
    );
}
