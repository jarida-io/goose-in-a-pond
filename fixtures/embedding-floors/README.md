# Similarity floors per embedding model

An embedding model's cosines sit on its own scale, so the cut-offs the pond applies to them are set
per model (`SimilarityFloors` in `crates/pond-core/src/models/domain/embedding_models.rs`). This
fixture is how they are set: labelled cases, scored by the pond's own providers through the same
`embed` calls the pond makes, and compared against MiniLM's floors, which the household runs today.

- `cases.json`: 81 opening messages, each labelled with the tool groups it needs (none for 15 of
  them), and 34 pairs of a stored memory and a candidate, labelled `same` (one fact said again,
  including across Swahili and English), `related` (the same subject, a different fact) or `fresh`.
  Fictional, in English, Swahili and Sheng.
- `analyse.py`: tool-selection recall and surplus per floor, and dedup drops per band.

```text
cargo run --release -p pond-server --features local-inference --example embed-scores -- \
    --data-dir DIR --model embeddinggemma-2 --cases fixtures/embedding-floors/cases.json --gpu > eg2.json
python3 fixtures/embedding-floors/analyse.py minilm.json eg2.json
```

## 2026-11-02, Orin Nano, EmbeddingGemma 2 against MiniLM

Scored on the nano with the pond stopped: EmbeddingGemma 2 on the GPU as the pond places it, MiniLM
through fastembed.

**Tool selection.** A turn keeps the top-scoring group and every group at or above the floor.
Surplus is the groups a turn keeps that it does not need.

| Model | Floor | Needed groups kept | Surplus groups a turn | Top scorer is needed |
|---|---|---|---|---|
| all-MiniLM-L6-v2 | 0.28 | 56% | 0.69 | 55% |
| EmbeddingGemma 2 | 0.71 | 82% | 0.69 | 82% |
| EmbeddingGemma 2 | 0.66 | 92% | 2.70 | |

MiniLM's needed groups score a median 0.18 and its unneeded ones 0.05; EmbeddingGemma 2's score 0.74
and 0.63. MiniLM misses most of the Swahili requests. The floor is 0.71: the prompt cost MiniLM's
0.28 has today, for 26 more points of recall.

**Memory dedup.** A candidate at or above `relate` is dropped as already known; `reinforce` only
decides whether it is counted as the same memory or a related one.

| Model | relate | Duplicates dropped | Related facts dropped | Fresh facts dropped |
|---|---|---|---|---|
| all-MiniLM-L6-v2 (the dials) | 0.78 | 9 of 14 | 1 of 10 | 0 of 10 |
| EmbeddingGemma 2 | 0.92 | 9 of 14 | 1 of 10 | 0 of 10 |

`relate` 0.92 drops what MiniLM's 0.78 drops, in number: MiniLM all nine English paraphrases,
EmbeddingGemma 2 eight of them and one repeat said across Swahili and English. Neither catches most of
those five: MiniLM scores them 0.28 to 0.62, EmbeddingGemma 2 0.83 to 0.94. A lower `relate` would
catch them, and drop related facts with them: at 0.83, 6 of 10. Both drop "Otieno is allergic to cats"
as a repeat of "allergic to peanuts", a real loss no band fixes, because `same` and `related` overlap
on both scales. `reinforce` is 0.97.

**What this does not show.** 66 needed groups and 34 pairs are few, and the cases are written, not
taken from a household. The tool-selection floor matters only under `tool_selection_mode =
"relevant"`; the default keeps every group.
