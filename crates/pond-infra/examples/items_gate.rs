//! Context space P1's store gate, for a copy of a real pond's data directory: migrate it, then
//! prove a second pass changes nothing and that memory search, recall and the slot's backlog read
//! the item-keyed index. Loads no model: queries are vectors already in the index.
//!
//! `cargo run --release -p pond-infra --example items_gate -- <copied data dir>`

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use pond_core::context::retrieval_service::PersonalContextRetrieval;
use pond_core::context::vector_index::{VectorIndex, INDEXED, MEMORY};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_infra::db::Database;
use pond_infra::sqlite_items::{reconcile, Mode};
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_vector_index::SqliteVectorIndex;

/// Names the index's model and answers every query with one stored vector.
struct Stored {
    model_id: String,
    vector: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for Stored {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(self.vector.clone())
    }
    fn dimensions(&self) -> usize {
        self.vector.len()
    }
    fn model_id(&self) -> String {
        self.model_id.clone()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .context("usage: items_gate <copied data dir>")?;
    let dir = std::path::Path::new(&dir);
    if !dir.join("pond_system.db").exists() {
        bail!("{} holds no pond_system.db", dir.display());
    }

    let started = std::time::Instant::now();
    let db = Database::init(dir).await?;
    println!("migrated and backfilled in {:?}", started.elapsed());
    let started = std::time::Instant::now();
    let first = reconcile(&db.system, Mode::All).await?;
    println!(
        "full check after the backfill: {first:?} in {:?}",
        started.elapsed()
    );
    for pass in 1..=2 {
        let again = reconcile(&db.system, Mode::All).await?;
        if again.changed() {
            bail!("pass {pass} changed something: {again:?}");
        }
    }
    drop(db);
    let db = Database::init(dir).await?;
    if reconcile(&db.system, Mode::All).await?.changed() {
        bail!("reopening the database changed a header");
    }
    println!("re-run: reopening and two full checks changed nothing");

    let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
    let probe: Option<(String, String, Vec<u8>, Option<String>)> = sqlx::query_as(
        "SELECT v.item_id, v.model_id, v.vector, i.owner_profile_id FROM vectors v \
         JOIN sys.items i ON i.id = v.item_id \
         WHERE v.chunk_ix = 0 AND i.kind IN ('fact', 'trait') AND i.state = 'active' LIMIT 1",
    )
    .fetch_optional(&db.vectors)
    .await?;
    let Some((item_id, model_id, blob, owner)) = probe else {
        bail!("no active memory carried a vector across");
    };
    let vector: Vec<f32> = blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let scope = owner.map_or(ProfileScope::Household, ProfileScope::Owner);
    let embedder: Arc<dyn EmbeddingProvider> = Arc::new(Stored {
        model_id: model_id.clone(),
        vector: vector.clone(),
    });

    let memories = SqliteMemoryRepository::new(db.system.clone())
        .with_vector_index(index.clone(), Some(embedder.clone()));
    let found = memories.search_similar(&vector, &scope, 5).await?;
    let first_item: Option<String> = match found.first() {
        Some(m) => {
            sqlx::query_scalar("SELECT item_id FROM memory_fragments WHERE id = ?")
                .bind(&m.id)
                .fetch_one(&db.system)
                .await?
        }
        None => None,
    };
    if first_item.as_deref() != Some(item_id.as_str()) {
        bail!("memory search did not put the probed memory first: {found:?}");
    }
    if found[0].embedding.as_deref() != Some(vector.as_slice()) {
        bail!("memory search returned no vector to band by");
    }
    println!(
        "memory search: the probed memory first of {}, with its vector",
        found.len()
    );

    if !memories
        .search_similar(&vector, &ProfileScope::Guest, 5)
        .await?
        .is_empty()
    {
        bail!("a guest's memory search returned something");
    }
    let hits = index
        .search(&vector, &model_id, &ProfileScope::Guest, &MEMORY, 5)
        .await?;
    if !hits.is_empty() {
        bail!("a guest's index search returned something");
    }
    println!("guest: memory search and the index return nothing");

    let recall = PersonalContextRetrieval::new(index.clone(), embedder)
        .recall("the probe", &scope, 5)
        .await;
    if recall.first().map(|r| r.item_id.as_str()) != Some(item_id.as_str()) {
        bail!("recall did not find the probed memory: {recall:?}");
    }
    println!("recall: {}", recall[0].labelled());

    let health = index.health(&model_id).await?;
    let stale = index.needs_embedding(&INDEXED, &model_id, 10_000).await?;
    println!(
        "index for {model_id}: {} current, {} another model's, {} missing; the slot's backlog holds {}",
        health.matching,
        health.mismatched,
        health.missing,
        stale.len()
    );
    for k in health.per_kind.iter().filter(|k| k.source_rows > 0) {
        println!(
            "  {:<12} {} headers, {} searchable, {} current, {} missing",
            k.kind, k.source_rows, k.rows, k.indexed_rows, k.missing_rows
        );
    }
    Ok(())
}
