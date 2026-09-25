//! Drives the synthetic household corpus through the real ingest path; only the repo is mocked.
//! `include_str!` so a moved fixture fails the build instead of yielding an empty, passing corpus.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pond_core::context::domain::{
    ContextItem, ContextSource, ItemKind, SourceAvailability, SourceKind, SourceParts, SourceStatus,
};
use pond_core::context::ingest::{IngestPipeline, RawItem};
use pond_core::context::mocks::mock_context_repository::MockContextRepository;
use pond_core::context::retrieval::{
    estimated_tokens, rank_by_relevance, render_block, select_within_budget, split_preamble_budget,
};
use pond_infra::rule_redactor::RuleRedactor;
use serde_json::Value;

const SOURCES: &str = include_str!("../../../fixtures/personal-context/sources.json");
const ITEMS: &str = include_str!("../../../fixtures/personal-context/household.jsonl");
const EXPECTATIONS: &str = include_str!("../../../fixtures/personal-context/expectations.jsonl");
const MANIFEST: &str = include_str!("../../../fixtures/personal-context/manifest.json");
const QUERIES: &str = include_str!("../../../fixtures/personal-context/queries.jsonl");

/// The corpus's anchored time, not the wall clock, so recency scores don't drift daily.
fn corpus_now() -> DateTime<Utc> {
    let m: Value = serde_json::from_str(MANIFEST).expect("manifest");
    m["corpus_now"]
        .as_str()
        .expect("corpus_now")
        .parse()
        .expect("corpus_now parses")
}

fn sources() -> Vec<ContextSource> {
    let raw: Vec<Value> = serde_json::from_str(SOURCES).expect("sources.json");
    raw.iter()
        .map(|s| {
            let kind = SourceKind::parse(s["kind"].as_str().unwrap())
                .expect("fixture names a SourceKind that no longer exists");
            ContextSource::from_parts(SourceParts {
                id: s["id"].as_str().unwrap().to_string(),
                kind,
                provider: s["provider"].as_str().unwrap().to_string(),
                profile_id: s["profile_id"].as_str().unwrap().to_string(),
                scopes: s["scopes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect(),
                cursor: None,
                last_sync: None,
                status: SourceStatus::Connected,
                secret_ref: None,
                created_at: corpus_now(),
            })
            .expect("fixture source is valid")
        })
        .collect()
}

/// JSONL lines as `(source id, RawItem)`; no line has a `profile_id`, the source sets the owner.
fn items() -> Vec<(String, RawItem)> {
    ITEMS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("corpus line");
            assert!(
                v.get("profile_id").is_none(),
                "a corpus line named its own owner; the ingest invariant says the \
                 source decides whose data this is"
            );
            let raw = RawItem {
                external_id: v["external_id"].as_str().unwrap().to_string(),
                kind: ItemKind::parse(v["kind"].as_str().unwrap())
                    .expect("fixture names an ItemKind that no longer exists"),
                occurred_at: v["occurred_at"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .expect("timestamp"),
                title: v["title"].as_str().unwrap().to_string(),
                body: v["body"].as_str().unwrap().to_string(),
                participants: v["participants"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap().to_string())
                    .collect(),
            };
            (v["source_id"].as_str().unwrap().to_string(), raw)
        })
        .collect()
}

struct Expectation {
    external_id: String,
    findings: BTreeSet<String>,
    note: String,
}

fn expectations() -> Vec<Expectation> {
    EXPECTATIONS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("expectation line");
            Expectation {
                external_id: v["external_id"].as_str().unwrap().to_string(),
                findings: v["expect_findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap().to_string())
                    .collect(),
                note: v["note"].as_str().unwrap().to_string(),
            }
        })
        .collect()
}

/// Ingests the corpus; returns the stored items and a per-kind `(accepted, refused)` tally.
async fn ingest_corpus() -> (Vec<ContextItem>, BTreeMap<&'static str, (usize, usize)>) {
    let repo = Arc::new(MockContextRepository::new());
    let pipeline = IngestPipeline::new(repo.clone(), Arc::new(RuleRedactor::new()));
    let srcs = sources();
    let now = corpus_now();

    // Keyed by string: `SourceKind` isn't `Ord`, and core shouldn't derive it for a test.
    let mut tally: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    for (source_id, raw) in items() {
        let source = srcs
            .iter()
            .find(|s| s.id() == source_id)
            .expect("corpus line routes to a source that sources.json does not define");
        let entry = tally.entry(source.kind().as_str()).or_insert((0, 0));
        match pipeline.ingest(source, raw, now).await {
            Ok(_) => entry.0 += 1,
            Err(_) => entry.1 += 1,
        }
    }
    (repo.all_items(), tally)
}

#[tokio::test]
async fn every_landed_kind_ingests_and_every_gated_kind_is_refused() {
    let (stored, tally) = ingest_corpus().await;

    let mut accepted_total = 0;
    let mut refused_total = 0;
    println!("\n-- ingest by source kind ------------------------------------");
    for (name, (ok, refused)) in &tally {
        let kind = SourceKind::parse(name).expect("tally key came from as_str");
        accepted_total += ok;
        refused_total += refused;
        println!(
            "  {:9} accepted {:>4}  refused {:>4}   ({:?})",
            name,
            ok,
            refused,
            kind.availability()
        );
        match kind.availability() {
            SourceAvailability::Landed => assert_eq!(
                *refused, 0,
                "a Landed kind refused {refused} items; the pipeline and the \
                 availability map disagree"
            ),
            _ => assert_eq!(
                *ok, 0,
                "a gated kind accepted {ok} items; SourceAvailability stopped \
                 being enforced at the pipeline"
            ),
        }
    }
    println!(
        "  total     accepted {accepted_total:>4}  refused {refused_total:>4}   \
         ({:.0}% of the corpus is unreachable today)",
        refused_total as f32 * 100.0 / (accepted_total + refused_total) as f32
    );

    assert_eq!(
        stored.len(),
        accepted_total,
        "a stored row was not accounted for"
    );
    assert!(
        refused_total > 0,
        "the corpus must exercise the gated kinds too"
    );
}

/// False positives matter most: they silently rewrite text, and Secret-class originals are lost.
#[tokio::test]
async fn the_redactor_finds_what_the_corpus_plants_and_nothing_more() {
    let (stored, _) = ingest_corpus().await;
    let by_ext: BTreeMap<&str, &ContextItem> =
        stored.iter().map(|i| (i.external_id(), i)).collect();

    // Item -> source kind, to tell an unreached expectation from a wrong one.
    let srcs = sources();
    let source_of: BTreeMap<String, SourceKind> = items()
        .into_iter()
        .map(|(sid, r)| {
            let kind = srcs.iter().find(|s| s.id() == sid).expect("source").kind();
            (r.external_id, kind)
        })
        .collect();

    let mut failures: Vec<String> = Vec::new();
    let mut false_positive_items = 0;
    let mut checked_clean = 0;
    let mut blocked: Vec<String> = Vec::new();

    println!("\n-- redaction expectations -----------------------------------");
    for exp in expectations() {
        let item = match by_ext.get(exp.external_id.as_str()) {
            Some(i) => i,
            None => {
                // A refused source kind never reached the redactor: record it, don't fail.
                let kind = source_of
                    .get(&exp.external_id)
                    .copied()
                    .expect("expectation names an item the corpus does not contain");
                if kind.availability() == SourceAvailability::Landed {
                    failures.push(format!(
                        "{}: on a Landed source yet never stored",
                        exp.external_id
                    ));
                } else {
                    println!(
                        "  BLOCKED {:<20} expected {:?} -- {} source is {:?}",
                        exp.external_id,
                        exp.findings,
                        kind.as_str(),
                        kind.availability()
                    );
                    blocked.push(format!("{} ({})", exp.external_id, kind.as_str()));
                }
                continue;
            }
        };
        let actual: BTreeSet<String> = item
            .findings()
            .iter()
            .map(|f| f.as_str().to_string())
            .collect();

        if exp.findings.is_empty() {
            checked_clean += 1;
        }
        let verdict = if actual == exp.findings {
            "ok  "
        } else {
            if exp.findings.is_empty() {
                false_positive_items += 1;
            }
            failures.push(format!(
                "{}: expected {:?}, found {:?}\n      {}",
                exp.external_id, exp.findings, actual, exp.note
            ));
            "FAIL"
        };
        println!(
            "  {verdict} {:<20} expected {:?} found {:?}",
            exp.external_id, exp.findings, actual
        );
    }

    println!(
        "  {checked_clean} items asserted clean, {false_positive_items} false positives, \
         {} expectations blocked by a gated source kind",
        blocked.len()
    );
    if !blocked.is_empty() {
        println!("  blocked: {}", blocked.join(", "));
    }

    assert!(
        failures.is_empty(),
        "the redactor disagreed with the corpus on {} item(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// At the ingest level (`Secrets`), contact details are reported but kept, by design.
#[tokio::test]
async fn secret_class_findings_are_replaced_and_contact_details_are_not() {
    use pond_core::security::domain::redaction::RedactionKind;

    let (stored, _) = ingest_corpus().await;
    let raw_bodies: BTreeMap<String, String> = items()
        .into_iter()
        .map(|(_, r)| (r.external_id, r.body))
        .collect();

    let mut replaced = 0;
    let mut reported_only = 0;
    println!("\n-- what Secrets level replaced ------------------------------");
    for item in &stored {
        if item.findings().is_empty() {
            continue;
        }
        let raw = raw_bodies.get(item.external_id()).expect("raw body");
        for kind in item.findings() {
            if kind.sensitivity() == pond_core::security::domain::event::PrivacySensitivity::Secret
            {
                assert!(
                    item.body().contains(kind.placeholder()),
                    "{}: a {} was found and classifies Secret, but the stored body \
                     carries no placeholder -- the credential survived ingest",
                    item.external_id(),
                    kind.as_str()
                );
                replaced += 1;
                println!("  replaced  {:<20} {}", item.external_id(), kind.as_str());
            } else {
                assert!(
                    !item.body().contains(kind.placeholder()),
                    "{}: a {} was replaced at Secrets level. Either the level \
                     changed or the sensitivity mapping did; both change what the \
                     model reads",
                    item.external_id(),
                    kind.as_str()
                );
                reported_only += 1;
            }
        }
        if item.findings().iter().all(|k| {
            k.sensitivity() != pond_core::security::domain::event::PrivacySensitivity::Secret
        }) {
            assert_eq!(
                item.body(),
                raw,
                "{}: findings were all non-Secret yet the body changed",
                item.external_id()
            );
        }
    }
    println!(
        "  {replaced} secret spans replaced, {reported_only} contact details \
         reported and left in place"
    );
    assert!(
        replaced > 0,
        "the corpus must plant at least one real credential"
    );
    assert!(
        reported_only > 0,
        "the corpus must plant at least one contact detail, or the posture above \
         is untested"
    );
    let _ = RedactionKind::ALL;
}

/// Ranks with `similarity: None`, as on the Orin, whose only `EmbeddingProvider` won't initialise.
#[tokio::test]
async fn a_households_context_does_not_fit_a_jetson_preamble() {
    use pond_core::models::services::context::context_budget::CompactionProfile;

    let (stored, _) = ingest_corpus().await;
    let now = corpus_now();

    let total_tokens: usize = stored.iter().map(estimated_tokens).sum();
    println!("\n-- corpus cost ----------------------------------------------");
    println!(
        "  {} ingestible items, {total_tokens} tokens rendered in full \
         ({:.1} tokens/item)",
        stored.len(),
        total_tokens as f32 / stored.len() as f32
    );

    let mut ranked: Vec<(ContextItem, Option<f32>)> =
        stored.iter().cloned().map(|i| (i, None)).collect();
    rank_by_relevance(&mut ranked, now);

    println!("\n-- what fits, by context window -----------------------------");
    for window in [4_096usize, 8_192, 16_384, 32_768] {
        let profile = CompactionProfile::from_context_window(window);
        let split = split_preamble_budget(profile.memory_token_budget, true);
        let kept = select_within_budget(&ranked, split.context_tokens);
        let spent: usize = kept.iter().map(|i| estimated_tokens(i)).sum();
        println!(
            "  window {:>6}  memory budget {:>4}  context share {:>4}  \
             fits {:>3} of {} items ({:.1}%), spends {:>4}",
            window,
            profile.memory_token_budget,
            split.context_tokens,
            kept.len(),
            stored.len(),
            kept.len() as f32 * 100.0 / stored.len() as f32,
            spent,
        );
        assert!(
            !kept.is_empty(),
            "no item fit a {window} window; select_within_budget promises at least one"
        );
    }

    let jetson = CompactionProfile::from_context_window(16_384);
    let split = split_preamble_budget(jetson.memory_token_budget, true);
    let kept = select_within_budget(&ranked, split.context_tokens);
    println!("\n-- the block, at the Orin's pinned 16,384 window -------------");
    println!("{}", render_block(&kept));

    let by_kind = kept
        .iter()
        .fold(BTreeMap::new(), |mut m: BTreeMap<&str, usize>, i| {
            *m.entry(i.source_kind().as_str()).or_default() += 1;
            m
        });
    println!("\n  kinds represented in the block: {by_kind:?}");
    println!(
        "  the other {} items are reachable only by retrieval, which is the \
         argument for PAI-3 rather than a bigger slice",
        stored.len() - kept.len()
    );

    // Guards the premise: if a household fits the preamble, the corpus proves nothing.
    assert!(
        kept.len() * 4 < stored.len(),
        "the corpus is too small to exercise a budget: {} of {} items fit",
        kept.len(),
        stored.len()
    );
}

/// Baseline for a future embedder: its recall@k on `queries.jsonl` must beat this recency slice.
#[tokio::test]
async fn the_recency_slice_answers_almost_none_of_the_labelled_queries() {
    use pond_core::models::services::context::context_budget::CompactionProfile;

    let (stored, _) = ingest_corpus().await;
    let now = corpus_now();

    let mut ranked: Vec<(ContextItem, Option<f32>)> =
        stored.iter().cloned().map(|i| (i, None)).collect();
    rank_by_relevance(&mut ranked, now);

    let profile = CompactionProfile::from_context_window(16_384);
    let split = split_preamble_budget(profile.memory_token_budget, true);
    let kept = select_within_budget(&ranked, split.context_tokens);
    let in_block: BTreeSet<&str> = kept.iter().map(|i| i.external_id()).collect();
    let ingestible: BTreeSet<&str> = stored.iter().map(|i| i.external_id()).collect();

    let mut hits = 0usize;
    let mut answerable = 0usize;
    println!("\n-- can the block answer the question? ------------------------");
    for line in QUERIES.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).expect("query line");
        let query = v["query"].as_str().unwrap();
        let relevant: Vec<&str> = v["relevant"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();

        // Queries answerable only from gated source kinds can't be answered at any budget.
        let reachable: Vec<&&str> = relevant
            .iter()
            .filter(|r| ingestible.contains(**r))
            .collect();
        if reachable.is_empty() {
            println!("  n/a  {query}  (every relevant item is on a gated source)");
            continue;
        }
        answerable += 1;
        let hit = reachable.iter().any(|r| in_block.contains(**r));
        if hit {
            hits += 1;
        }
        println!("  {}  {query}", if hit { "HIT " } else { "miss" });
    }

    println!(
        "\n  {hits} of {answerable} answerable queries have an answer in the block \
         ({:.0}%)",
        hits as f32 * 100.0 / answerable as f32
    );
    println!(
        "  the block holds {} of {} ingestible items, chosen by recency alone",
        kept.len(),
        stored.len()
    );

    // Guards the premise: recency answering most queries means the corpus or budget isn't real.
    assert!(
        hits * 2 < answerable,
        "a recency-ordered slice answered {hits} of {answerable} labelled queries; \
         that is too many for this corpus to be evidence that retrieval is needed"
    );
}
