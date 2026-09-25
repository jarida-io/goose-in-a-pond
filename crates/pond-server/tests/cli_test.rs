//! CLI smoke tests, each in a fresh `POND_DATA_DIR`; network/hardware/stdin commands are skipped.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn pond(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("pond-server").unwrap();
    cmd.env("POND_DATA_DIR", tmp.path());
    cmd
}

/// First token of the first line containing `keyword` (pulls UUIDs out of tables).
fn extract_first_col(stdout: &str, keyword: &str) -> String {
    stdout
        .lines()
        .find(|l| l.contains(keyword))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("")
        .to_string()
}

// ── Group 0: Global flags (no data dir needed) ────────────────────────────────

#[test]
fn help_flag_succeeds() {
    Command::cargo_bin("pond-server")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Goose In A Pond"));
}

#[test]
fn version_flag_succeeds() {
    Command::cargo_bin("pond-server")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"\d+\.\d+\.\d+").unwrap());
}

#[test]
fn subcommand_help_serve() {
    Command::cargo_bin("pond-server")
        .unwrap()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HTTP server"));
}

#[test]
fn subcommand_help_chat() {
    Command::cargo_bin("pond-server")
        .unwrap()
        .args(["chat", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("provider"));
}

#[test]
fn subcommand_help_setup() {
    Command::cargo_bin("pond-server")
        .unwrap()
        .args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("setup"));
}

// ── Group 1: Status ───────────────────────────────────────────────────────────

/// `pond status` on a brand-new temp dir (no pond_system.db) must not crash.
#[test]
fn status_fresh_data_dir() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Version"))
        .stdout(predicate::str::contains("Hostname"));
}

/// `pond status` after a command that initialises the DB shows Settings values.
#[test]
fn status_after_db_init() {
    let tmp = TempDir::new().unwrap();

    // Run `memories list` to force DB creation.
    pond(&tmp).args(["memories", "list"]).assert().success();

    pond(&tmp)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Database"))
        .stdout(predicate::str::contains("pond_system.db"));
}

// ── Group 2: Prompts ──────────────────────────────────────────────────────────

#[test]
fn prompts_list_empty_db() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["prompts", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No templates found"));
}

#[test]
fn prompts_reset_creates_template() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["prompts", "reset", "balanced"])
        .assert()
        .success()
        .stdout(predicate::str::contains("balanced").and(predicate::str::contains("reset")));
}

#[test]
fn prompts_show_after_reset() {
    let tmp = TempDir::new().unwrap();

    pond(&tmp)
        .args(["prompts", "reset", "balanced"])
        .assert()
        .success();

    pond(&tmp)
        .args(["prompts", "show", "balanced"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn prompts_list_shows_seeded_template() {
    let tmp = TempDir::new().unwrap();

    for name in &["balanced", "concise", "technical", "warm"] {
        pond(&tmp)
            .args(["prompts", "reset", name])
            .assert()
            .success();
    }

    pond(&tmp)
        .args(["prompts", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("balanced"))
        .stdout(predicate::str::contains("concise"))
        .stdout(predicate::str::contains("technical"))
        .stdout(predicate::str::contains("warm"));
}

#[test]
fn prompts_reset_invalid_name_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["prompts", "reset", "does-not-exist"])
        .assert()
        .failure();
}

#[test]
fn prompts_show_unknown_name_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["prompts", "show", "no-such-template"])
        .assert()
        .failure();
}

// ── Group 3: Skills ───────────────────────────────────────────────────────────

#[test]
fn skills_list_empty() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["skills", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No skills"));
}

#[test]
fn skills_full_crud_cycle() {
    let tmp = TempDir::new().unwrap();

    // ── Add ──────────────────────────────────────────────────────────────────
    pond(&tmp)
        .args([
            "skills",
            "add",
            "morning-brief",
            "--content",
            "When asked for a briefing, call giap__get_current_weather first.",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("morning-brief").and(predicate::str::contains("created")));

    // ── List (active only) shows it ───────────────────────────────────────────
    pond(&tmp)
        .args(["skills", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("morning-brief"));

    // ── List --all gives us the UUID ──────────────────────────────────────────
    let out = pond(&tmp)
        .args(["skills", "list", "--all"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let uuid = extract_first_col(&stdout, "morning-brief");
    assert!(
        !uuid.is_empty(),
        "Expected UUID in first column, got:\n{stdout}"
    );

    // ── Toggle (disable) ─────────────────────────────────────────────────────
    pond(&tmp)
        .args(["skills", "toggle", &uuid])
        .assert()
        .success()
        .stdout(predicate::str::contains("disabled"));

    pond(&tmp)
        .args(["skills", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No skills"));

    pond(&tmp)
        .args(["skills", "list", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("morning-brief"));

    // ── Toggle again (re-enable) ──────────────────────────────────────────────
    pond(&tmp)
        .args(["skills", "toggle", &uuid])
        .assert()
        .success()
        .stdout(predicate::str::contains("enabled"));

    // ── Remove ───────────────────────────────────────────────────────────────
    pond(&tmp)
        .args(["skills", "remove", &uuid])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted"));

    // ── List is empty again ───────────────────────────────────────────────────
    pond(&tmp)
        .args(["skills", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No skills"));
}

#[test]
fn skills_toggle_unknown_id_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["skills", "toggle", "00000000-0000-0000-0000-000000000000"])
        .assert()
        .failure();
}

#[test]
fn skills_multiple_entries_all_listed() {
    let tmp = TempDir::new().unwrap();

    pond(&tmp)
        .args(["skills", "add", "skill-one", "--content", "First skill"])
        .assert()
        .success();
    pond(&tmp)
        .args(["skills", "add", "skill-two", "--content", "Second skill"])
        .assert()
        .success();

    pond(&tmp)
        .args(["skills", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skill-one"))
        .stdout(predicate::str::contains("skill-two"));
}

// ── Group 4: Memories ─────────────────────────────────────────────────────────

#[test]
fn memories_list_empty() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["memories", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No memory"));
}

#[test]
fn memories_full_crud_cycle() {
    let tmp = TempDir::new().unwrap();

    // ── Add ──────────────────────────────────────────────────────────────────
    let add_out = pond(&tmp)
        .args(["memories", "add", "User prefers temperatures in Celsius"])
        .output()
        .unwrap();
    let add_stdout = String::from_utf8_lossy(&add_out.stdout);
    assert!(
        add_out.status.success(),
        "memories add failed: {add_stdout}"
    );
    assert!(
        add_stdout.contains("Memory saved"),
        "Unexpected: {add_stdout}"
    );

    // Parse UUID from "✓ Memory saved (id: <uuid>)."
    let uuid = add_stdout
        .split("id: ")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .unwrap_or("")
        .trim()
        .to_string();
    assert!(!uuid.is_empty(), "Could not parse UUID from: {add_stdout}");

    // ── List shows it ─────────────────────────────────────────────────────────
    pond(&tmp)
        .args(["memories", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Celsius"));

    // ── Custom limit ─────────────────────────────────────────────────────────
    pond(&tmp)
        .args(["memories", "list", "--limit", "5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Celsius"));

    // ── Remove ───────────────────────────────────────────────────────────────
    pond(&tmp)
        .args(["memories", "remove", &uuid])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted"));

    // ── List is empty again ───────────────────────────────────────────────────
    pond(&tmp)
        .args(["memories", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No memory"));
}

#[test]
fn memories_add_multiple_and_list() {
    let tmp = TempDir::new().unwrap();

    pond(&tmp)
        .args(["memories", "add", "First memory"])
        .assert()
        .success();
    pond(&tmp)
        .args(["memories", "add", "Second memory"])
        .assert()
        .success();
    pond(&tmp)
        .args(["memories", "add", "Third memory"])
        .assert()
        .success();

    pond(&tmp)
        .args(["memories", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("First memory"))
        .stdout(predicate::str::contains("Second memory"))
        .stdout(predicate::str::contains("Third memory"));

    let out = pond(&tmp)
        .args(["memories", "list", "--limit", "2"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let count = stdout.lines().filter(|l| l.contains("memory")).count();
    assert!(
        count <= 2,
        "Expected at most 2 entries with --limit 2, got {count}: {stdout}"
    );
}

// ── Group 5: Recipes ──────────────────────────────────────────────────────────

#[test]
fn recipes_list_empty() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["recipes", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No recipes"));
}

#[test]
fn recipes_full_crud_cycle() {
    let tmp = TempDir::new().unwrap();

    let recipe_yaml = tmp.path().join("morning_brief.yaml");
    std::fs::write(
        &recipe_yaml,
        indoc::indoc! {r#"
        version: 1
        title: "Morning Brief"
        description: "Daily briefing automation"
        instructions: |
          Fetch the current weather and summarise it briefly.
    "#},
    )
    .unwrap();

    // ── Import ────────────────────────────────────────────────────────────────
    pond(&tmp)
        .args([
            "recipes",
            "import",
            "morning_brief",
            recipe_yaml.to_str().unwrap(),
            "--description",
            "Daily morning briefing",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("morning_brief").and(predicate::str::contains("imported")),
        );

    // ── List shows it ─────────────────────────────────────────────────────────
    pond(&tmp)
        .args(["recipes", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("morning_brief"));

    // ── Show prints the YAML content ──────────────────────────────────────────
    pond(&tmp)
        .args(["recipes", "show", "morning_brief"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Morning Brief").or(predicate::str::contains("morning")));

    // ── Remove ───────────────────────────────────────────────────────────────
    pond(&tmp)
        .args(["recipes", "remove", "morning_brief"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted"));

    // ── List is empty again ───────────────────────────────────────────────────
    pond(&tmp)
        .args(["recipes", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No recipes"));
}

#[test]
fn recipes_show_unknown_name_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["recipes", "show", "no-such-recipe"])
        .assert()
        .failure();
}

#[test]
fn recipes_import_nonexistent_file_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args([
            "recipes",
            "import",
            "bad",
            "/tmp/does_not_exist_pond_test.yaml",
        ])
        .assert()
        .failure();
}

// ── Group 6: Models ───────────────────────────────────────────────────────────

#[test]
fn models_list_empty_catalog() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["models", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Category").or(predicate::str::contains("catalog empty")));
}

#[test]
fn models_list_with_valid_category_filter() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["models", "list", "--category", "gguf"])
        .assert()
        .success();
}

#[test]
fn models_list_invalid_category_exits_nonzero() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["models", "list", "--category", "definitely-not-a-category"])
        .assert()
        .failure();
}

// ── Group 7: Agent subcommands ────────────────────────────────────────────────

/// `agent extras` with an empty DB should report no extras and exit 0.
#[test]
fn agent_extras_empty() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["agent", "extras"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No").or(predicate::str::contains("extras")));
}

// ── Group 8: Edge cases / error handling ─────────────────────────────────────

#[test]
fn unknown_subcommand_shows_help() {
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .arg("definitely-not-a-subcommand")
        .assert()
        .failure(); // clap exits nonzero for unknown subcommands
}

#[test]
fn memories_remove_nonexistent_id_is_noop() {
    // SQLite DELETE on a missing row is silent — exits 0.
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["memories", "remove", "00000000-0000-0000-0000-000000000000"])
        .assert()
        .success();
}

#[test]
fn skills_remove_nonexistent_id_is_noop() {
    // SQLite DELETE on a missing row is silent — no error, "deleted" message still prints.
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["skills", "remove", "00000000-0000-0000-0000-000000000000"])
        .assert()
        .success();
}

#[test]
fn recipes_remove_nonexistent_name_exits_nonzero() {
    // The remove handler checks `get_by_name` first and calls `process::exit(1)` when missing.
    let tmp = TempDir::new().unwrap();
    pond(&tmp)
        .args(["recipes", "remove", "no-such-recipe"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}
