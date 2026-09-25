//! `main.rs` must build every memory repo and event-log write sink behind the redactor.
//! A text scan via `include_str!`: pond-server's tests don't run in CI, pond-infra's do.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

/// Whether `wrapper` is on the `site` line or up to six above (rustfmt splits `Arc::new(`),
/// stopping after the previous `construction` so a bare site can't borrow an earlier wrapper.
fn wrapped_within(lines: &[&str], site: usize, wrapper: &str, construction: &str) -> bool {
    let floor = site.saturating_sub(6);
    let mut start = floor;
    for i in (floor..site).rev() {
        if lines[i].contains(construction) {
            start = i + 1;
            break;
        }
    }
    lines[start..=site].iter().any(|l| l.contains(wrapper))
}

fn sites(lines: &[&str], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn every_memory_repository_construction_goes_through_the_redactor() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteMemoryRepository::new(");
    // Pinned, not a floor: `>= 4` would let a fifth bypass site slip in unnoticed.
    assert_eq!(
        found.len(),
        4,
        "found {} SqliteMemoryRepository::new( sites in main.rs; there were 4 \
         (run_server, run_chat, run_agent_cmd, run_memories_cmd). A new one is \
         a new write path and needs wrapping; a missing one means this guard \
         has stopped matching and is asserting nothing.",
        found.len()
    );
    for site in found {
        assert!(
            wrapped_within(
                &lines,
                site,
                "RedactingMemoryRepository::new(",
                "SqliteMemoryRepository::new("
            ),
            "main.rs:{} constructs SqliteMemoryRepository outside \
             RedactingMemoryRepository. Chokepoint 1 is bypassed on that path: \
             whatever writes through this repo -- extraction, the giap-memory \
             MCP tool, POST /memories, consolidation -- stores credentials \
             verbatim in pond_system.db. Wrap it, or if this really is a \
             read-only handle, say so here.",
            site + 1
        );
    }
}

#[test]
fn every_event_log_write_sink_goes_through_the_redactor() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteEventLog::new(");
    assert!(
        found.len() >= 2,
        "found {} SqliteEventLog::new( sites in main.rs; there were 2. This \
         guard has stopped matching and is asserting nothing. It was 5 until the \
         2026-09-10 group deletions took `giap-audit` and with it the three \
         `.into_dyn()` read handles `init_audit_deps` consumed.",
        found.len()
    );

    let mut write_sinks = 0;
    for site in found {
        // `.into_dyn()` is a read handle; redacting one would double-scrub on the way out.
        if lines[site].contains(".into_dyn()") {
            continue;
        }
        write_sinks += 1;
        assert!(
            wrapped_within(
                &lines,
                site,
                "RedactingEventLog::new(",
                "SqliteEventLog::new("
            ),
            "main.rs:{} builds a write-path SqliteEventLog outside \
             RedactingEventLog. Chokepoint 2 is bypassed: event attributes -- \
             egress URLs with their query strings, policy audit rows carrying \
             a principal label and a remote address -- reach pond_logs.db \
             unredacted.",
            site + 1
        );
    }
    assert_eq!(
        write_sinks, 2,
        "expected exactly 2 write-path event logs (the shared binding and the \
         SqliteSecurityPolicy audit sink); found {write_sinks}. A new one needs \
         its own wrapper and a line in this count."
    );
}

/// Egress must use the wrapped log: outbound URLs are the richest query-string PII source.
#[test]
fn the_egress_sink_reads_the_redacting_binding() {
    assert!(
        MAIN.contains("set_egress_sink(event_log.clone())"),
        "set_egress_sink no longer takes the wrapped `event_log` binding. If it \
         was rebound to a fresh SqliteEventLog, every recorded outbound call \
         bypasses the redactor -- and it compiles either way."
    );
}

#[test]
fn the_window_never_reaches_back_over_an_earlier_construction() {
    let lines = vec![
        "let memory_repo = Arc::new(RedactingMemoryRepository::new(",
        "    SqliteMemoryRepository::new(db.system.clone()),",
        "    redactor.clone(),",
        "));",
        "let leaky = Arc::new(SqliteMemoryRepository::new(db.system.clone()));",
    ];
    let sites: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("SqliteMemoryRepository::new("))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(sites, vec![1, 4], "fixture must contain both constructions");

    assert!(
        wrapped_within(
            &lines,
            1,
            "RedactingMemoryRepository::new(",
            "SqliteMemoryRepository::new("
        ),
        "the genuinely wrapped construction must still read as wrapped -- \
         without this the guard could pass by rejecting everything"
    );
    assert!(
        !wrapped_within(
            &lines,
            4,
            "RedactingMemoryRepository::new(",
            "SqliteMemoryRepository::new("
        ),
        "a bare construction below a wrapped one must NOT inherit its wrapper"
    );
}
