//! Every production `SqliteMemoryRepository` in `main.rs` must chain the vector index.
//! A text scan via `include_str!`: pond-server's tests don't run in CI, pond-infra's do.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

/// `(enclosing fn, reason)` exemptions: only a site with no vector pool may be listed here.
const EXCLUSIONS: &[(&str, &str)] = &[];

/// Whether `method` is on the `site` line or just below (rustfmt wraps it), stopping before
/// the next `construction` so a bare site can't borrow a later site's call.
fn chained_within(lines: &[&str], site: usize, method: &str, construction: &str) -> bool {
    let ceiling = (site + 7).min(lines.len());
    let mut end = ceiling;
    for (i, line) in lines.iter().enumerate().take(ceiling).skip(site + 1) {
        if line.contains(construction) {
            end = i;
            break;
        }
    }
    lines[site..end].iter().any(|l| l.contains(method))
}

fn sites(lines: &[&str], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i)
        .collect()
}

/// Nearest column-zero `fn` above `site`, so failures name a function, not a line number.
fn enclosing_fn(lines: &[&str], site: usize) -> String {
    for line in lines[..=site].iter().rev() {
        for prefix in ["async fn ", "fn ", "pub async fn ", "pub fn "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    "<unknown>".to_string()
}

#[test]
fn every_memory_repository_construction_mirrors_into_the_index() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteMemoryRepository::new(");
    // Pinned, not a floor: `>= 4` would let a fifth, copied bypass site slip in unnoticed.
    assert_eq!(
        found.len(),
        4,
        "found {} SqliteMemoryRepository::new( sites in main.rs; there were 4 \
         (run_server, run_chat, run_agent_cmd, run_memories_cmd). A new one is a \
         new write path and needs the index chained; a missing one means this \
         guard has stopped matching and is asserting nothing.",
        found.len()
    );

    let where_they_are: Vec<String> = found.iter().map(|&s| enclosing_fn(&lines, s)).collect();
    assert_eq!(
        where_they_are,
        vec![
            "run_server",
            "run_chat",
            "run_agent_cmd",
            "run_memories_cmd"
        ],
        "the four constructions are no longer in the four functions this guard \
         reasons about. Either a path moved or a new one appeared; re-read the \
         module comment before adjusting this list, because the exemption \
         reasons below are written against these specific paths."
    );

    for (site, function) in found.iter().zip(&where_they_are) {
        if let Some((_, reason)) = EXCLUSIONS.iter().find(|(f, _)| f == function) {
            // A site that chains anyway makes its exemption stale; the entry should go.
            assert!(
                !chained_within(
                    &lines,
                    *site,
                    ".with_vector_index(",
                    "SqliteMemoryRepository::new("
                ),
                "{function} is listed in EXCLUSIONS ({reason}) but now chains \
                 .with_vector_index anyway. Remove the exemption -- a stale one \
                 will excuse the next real bypass on this path."
            );
            continue;
        }
        assert!(
            chained_within(
                &lines,
                *site,
                ".with_vector_index(",
                "SqliteMemoryRepository::new("
            ),
            "main.rs:{} constructs SqliteMemoryRepository in {function} without \
             chaining .with_vector_index. Every memory that path writes lands in \
             pond_system.db and never reaches pond_vectors.db, and every memory \
             it deletes leaves its vector behind as an orphan -- both invisible \
             until semantic recall silently misses a fact the member is certain \
             they said. Chain the index, or if this path genuinely cannot reach \
             a vector pool, add it to EXCLUSIONS with the reason.",
            site + 1
        );
    }
}

/// These three paths have no embedder. `None` makes `mirror` defer to the sweep; a guessed
/// model id would silently corrupt the index.
#[test]
fn the_cli_paths_index_without_a_model_id() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteMemoryRepository::new(");

    let attributed: Vec<String> = found
        .iter()
        .filter(|&&site| {
            chained_within(
                &lines,
                site,
                "vector_model_id",
                "SqliteMemoryRepository::new(",
            )
        })
        .map(|&site| enclosing_fn(&lines, site))
        .collect();

    assert_eq!(
        attributed,
        vec!["run_server"],
        "exactly one path -- run_server -- constructs an embedding provider and \
         can therefore say which model produced a vector. If a CLI path has \
         started passing a model id, check it actually built an embedder: \
         attributing vectors to a model that did not produce them makes the \
         index disagree with the store in a way no sweep repairs. If run_server \
         has stopped passing one, its writes are no longer attributable and the \
         sweep now owns work it used to do inline."
    );
}

#[test]
fn the_window_never_reaches_over_a_later_construction() {
    let lines = vec![
        "    let leaky = Arc::new(SqliteMemoryRepository::new(db.system.clone()));",
        "",
        "    let split = Arc::new(",
        "        SqliteMemoryRepository::new(db.system.clone())",
        "            .with_vector_index(vector_index.clone(), vector_model_id.clone()),",
        "    );",
        "    let inline = Arc::new(SqliteMemoryRepository::new(p).with_vector_index(ix, None));",
    ];
    let found = sites(&lines, "SqliteMemoryRepository::new(");
    assert_eq!(found, vec![0, 3, 6], "fixture must contain all three forms");

    assert!(
        !chained_within(
            &lines,
            0,
            ".with_vector_index(",
            "SqliteMemoryRepository::new("
        ),
        "a bare construction ABOVE a chained one must NOT inherit its builder \
         call -- this is the whole failure mode the window bound exists for"
    );
    // Both must still read as chained, or a guard rejecting everything would pass.
    assert!(
        chained_within(
            &lines,
            3,
            ".with_vector_index(",
            "SqliteMemoryRepository::new("
        ),
        "a construction whose builder call rustfmt split onto the next line must \
         still read as chained"
    );
    assert!(
        chained_within(
            &lines,
            6,
            ".with_vector_index(",
            "SqliteMemoryRepository::new("
        ),
        "a construction chained on its own line must still read as chained -- \
         three of the four production sites are written this way"
    );
}

/// `EXCLUSIONS` is keyed by `enclosing_fn`, so a regression to `<unknown>` breaks exemptions.
#[test]
fn enclosing_fn_finds_the_nearest_column_zero_function() {
    let lines = vec![
        "async fn run_server(",
        "    port: u16,",
        ") -> Result<()> {",
        "    let repo = SqliteMemoryRepository::new(db.system.clone());",
        "}",
        "",
        "async fn run_memories_cmd(action: MemoryAction) -> Result<()> {",
        "    let repo = SqliteMemoryRepository::new(db.system.clone());",
    ];
    assert_eq!(enclosing_fn(&lines, 3), "run_server");
    assert_eq!(
        enclosing_fn(&lines, 7),
        "run_memories_cmd",
        "the scan must stop at the NEAREST enclosing fn, not the first one in \
         the file"
    );
}
