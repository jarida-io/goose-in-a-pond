//! Spawns `pond-server chat --json-events --provider mock` (offline echo agent) and checks its
//! stdout is contract NDJSON ending in `exit` on stdin EOF. Not run in CI (`cargo check` only).

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Parsed stdout events up to `exit`, or until `deadline` elapses.
fn read_events_until_exit(stdout: std::process::ChildStdout, deadline: Duration) -> Vec<Value> {
    let start = Instant::now();
    let mut reader = BufReader::new(stdout);
    let mut events = Vec::new();
    let mut line = String::new();
    loop {
        if start.elapsed() > deadline {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if trimmed.is_empty() {
                    continue;
                }
                // EVERY line on stdout must be valid JSON — no banners/prompts.
                let value: Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
                    panic!("non-JSON line on stdout (contract violation): {trimmed:?} ({e})")
                });
                let is_exit = value.get("event").and_then(Value::as_str) == Some("exit");
                events.push(value);
                if is_exit {
                    break;
                }
            }
            Err(e) => panic!("error reading child stdout: {e}"),
        }
    }
    events
}

#[test]
fn json_events_stdin_turn_emits_contract_ndjson() {
    let tmp = std::env::temp_dir().join(format!(
        "giap-json-events-contract-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&tmp).expect("create temp data dir");

    let bin = env!("CARGO_BIN_EXE_pond-server");
    let mut child = Command::new(bin)
        .args([
            "chat",
            "--json-events",
            "--session-id",
            "test-contract",
            "--provider",
            "mock",
        ])
        .env("POND_DATA_DIR", &tmp)
        .env("SQLX_OFFLINE", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pond-server binary");

    // One utterance then EOF: the first listen() runs the turn, the second sees EOF and exits.
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        writeln!(stdin, "what is the capital of france").expect("write to child stdin");
        stdin.flush().ok();
        // `stdin` drops here → pipe closes → child's next read_line() gets EOF.
    }

    let stdout = child.stdout.take().expect("child stdout");
    let events = read_events_until_exit(stdout, Duration::from_secs(60));

    // The child should have exited on its own; reap it (kill as a safety net).
    let _ = child.kill();
    let _ = child.wait();

    let names: Vec<&str> = events
        .iter()
        .map(|e| e.get("event").and_then(Value::as_str).unwrap_or("<none>"))
        .collect();

    assert_eq!(
        events
            .first()
            .and_then(|e| e.get("event"))
            .and_then(Value::as_str),
        Some("ready"),
        "first event must be ready; got sequence: {names:?}"
    );
    assert_eq!(
        events[0].get("session_id").and_then(Value::as_str),
        Some("test-contract"),
        "ready must carry the --session-id"
    );

    assert!(
        names.contains(&"transcript"),
        "a transcript event must appear; got: {names:?}"
    );
    assert!(
        names.contains(&"token"),
        "at least one token event must appear; got: {names:?}"
    );

    let state_values: Vec<&str> = events
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("state"))
        .filter_map(|e| e.get("state").and_then(Value::as_str))
        .collect();
    for needed in ["wait", "listen", "thinking", "speak"] {
        assert!(
            state_values.contains(&needed),
            "state '{needed}' must appear; got states: {state_values:?}"
        );
    }
    let idx = |s: &str| state_values.iter().position(|v| *v == s).unwrap();
    assert!(
        idx("wait") < idx("listen")
            && idx("listen") < idx("thinking")
            && idx("thinking") < idx("speak"),
        "states must progress wait→listen→thinking→speak; got: {state_values:?}"
    );

    let turn_complete_count = names.iter().filter(|n| **n == "turn_complete").count();
    assert_eq!(
        turn_complete_count, 1,
        "exactly one turn_complete on completion; got: {names:?}"
    );
    let tc = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("turn_complete"))
        .unwrap();
    assert_eq!(
        tc.get("session_id").and_then(Value::as_str),
        Some("test-contract")
    );

    let last = events.last().expect("at least one event");
    assert_eq!(
        last.get("event").and_then(Value::as_str),
        Some("exit"),
        "last event must be exit; got: {names:?}"
    );
    assert_eq!(
        last.get("reason").and_then(Value::as_str),
        Some("stdin_eof"),
        "exit reason must be stdin_eof after closing stdin"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
