//! Scores labelled cases with one embedding model the way the pond does, so a model's similarity
//! floors can be set from its own cosines: each opening message against every scored tool group's
//! description, and each stored memory against a candidate. Every text goes through `embed`, as
//! tool selection and memory dedup do. Writes JSON to stdout.
//!
//! ```text
//! cargo run --release -p pond-server --features local-inference --example embed-scores -- \
//!     --data-dir DIR --model embeddinggemma-2 --cases cases.json [--gpu]
//! ```
//!
//! `cases.json`: `{"messages": [{"text", "need": [extension]}], "pairs": [{"stored",
//! "candidate", "label"}]}`. Fastembed models need `ORT_DYLIB_PATH`, as the server sets it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use pond_core::mcp::domain::tool_group::TOOL_GROUPS;
use pond_core::models::domain::embedding_models::{self, EmbeddingEngine};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::user_data::domain::memory::cosine_similarity;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Cases {
    messages: Vec<Message>,
    pairs: Vec<Pair>,
}

#[derive(Deserialize)]
struct Message {
    text: String,
    need: Vec<String>,
}

#[derive(Deserialize)]
struct Pair {
    stored: String,
    candidate: String,
    label: String,
}

struct Args {
    data_dir: PathBuf,
    model: String,
    cases: PathBuf,
    gpu: bool,
}

fn args() -> Result<Args> {
    let mut data_dir = None;
    let mut model = None;
    let mut cases = None;
    let mut gpu = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--data-dir" => data_dir = it.next().map(PathBuf::from),
            "--model" => model = it.next(),
            "--cases" => cases = it.next().map(PathBuf::from),
            "--gpu" => gpu = true,
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(Args {
        data_dir: data_dir.context("--data-dir is required")?,
        model: model.context("--model is required")?,
        cases: cases.context("--cases is required")?,
        gpu,
    })
}

fn provider(args: &Args) -> Result<Arc<dyn EmbeddingProvider>> {
    let model = embedding_models::find(&args.model)
        .ok_or_else(|| anyhow!("{} is not in the embedding catalogue", args.model))?;
    let dir = args.data_dir.join("models").join("embedding");
    Ok(match model.engine() {
        EmbeddingEngine::Fastembed => Arc::new(
            pond_infra::fastembed_embedding::FastembedEmbeddingProvider::new(model.name, Some(dir))?,
        ),
        EmbeddingEngine::LlamaCpp => {
            use pond_inference::{EmbeddingModelSpec, GgufEmbeddingProvider, Placement};
            let placement = if args.gpu { Placement::Gpu } else { Placement::Cpu };
            let spec = EmbeddingModelSpec::resolve(model.name)?.with_placement(placement);
            Arc::new(GgufEmbeddingProvider::new(spec, &dir))
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = args()?;
    let cases: Cases = serde_json::from_slice(&std::fs::read(&args.cases)?)?;
    let embedder = provider(&args)?;

    let mut groups = Vec::new();
    for group in TOOL_GROUPS.iter().filter(|g| !g.core) {
        groups.push((group.extension, embedder.embed(group.description).await?));
    }

    let mut messages = Vec::new();
    for message in &cases.messages {
        let vector = embedder.embed(&message.text).await?;
        let scores: serde_json::Map<String, Value> = groups
            .iter()
            .map(|(extension, v)| (extension.to_string(), json!(cosine_similarity(&vector, v))))
            .collect();
        messages.push(json!({"text": message.text, "need": message.need, "scores": scores}));
    }

    let mut pairs = Vec::new();
    for pair in &cases.pairs {
        let stored = embedder.embed(&pair.stored).await?;
        let candidate = embedder.embed(&pair.candidate).await?;
        pairs.push(json!({
            "stored": pair.stored,
            "candidate": pair.candidate,
            "label": pair.label,
            "cosine": cosine_similarity(&stored, &candidate),
        }));
    }

    let out = json!({
        "model": args.model,
        "model_id": embedder.model_id(),
        "groups": groups.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
        "messages": messages,
        "pairs": pairs,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
