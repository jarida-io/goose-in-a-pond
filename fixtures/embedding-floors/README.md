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
- `closest.py`: the `closest_memory` floor, from the P0 harness's saved query and memory vectors
  (`scripts/jetson/embed-p0`), which carry the same query and document prompts as the pond.

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

**Since this run** `giap-knowledge` is a core group (2026-11-02, `small-model-tools.md` D1), so
`embed-scores` no longer scores it and `analyse.py` leaves its seven needs out: a core group is
loaded whatever the floor. The figures above were measured with it scored and were not re-run.

## 2026-11-06, the closest memory, EmbeddingGemma 2 and MiniLM

A turn keeps five memories, ranked `0.5 similarity + 0.3 importance + 0.2 recency` with a 14-day
half-life. On the nano, the owner asked "Which phone do I use?". The answer was a 17-day-old identity
memory, which ranked 44th of 49 on importance and recency. Five memories written the day before
filled the turn, and E4B said it did not know. `recall_memories`, which ranks on similarity alone,
returned that memory first.

EmbeddingGemma 2's cosines are too narrow for the blend to recover. On P0's 90 questions (the
memory-reachability facts, 403 household items and the Swahili and Sheng set, at the pond's 256
dimensions), the answer scores a median 0.79. A memory that is not the answer scores 0.61, and the
closest non-answer per question 0.76. No floor tells an answer from its neighbours, but rank does:
the closest memory is an answer for 60% of questions (MiniLM, 44%). So a turn gives its closest
memory a seat when that memory clears a floor (`select_for_turn`). The floor is set where the closest
memory stays on the question's subject in another household's store, where nothing answers. Above
it, "Is the greenhouse finished?" finds the tomato blight. Below it, "when is the dentist" finds the
dog's vet visit, and "Is the water tank leaking?" a preference about the network.

```text
python3 fixtures/embedding-floors/closest.py ~/p0-embed
```

| Model | Floor | Seat holds an answer | Seat holds another memory | Seat fires, nothing in the store | Recall@5, answer 17 days old among day-old memories |
|---|---|---|---|---|---|
| EmbeddingGemma 2 | 0.77 | 49% | 16% | 16% | 0.00 today, 0.49 with the seat |
| all-MiniLM-L6-v2 | 0.55 | 22% | 18% | 7% | 0.21 today, 0.30 with the seat |

"Another memory" is mostly the same subject, often a restatement of the answer, because P0 labels
one answer per fact. "Nothing in the store" puts each set's questions to the other two stores. Those
households share subjects (rent, school fees, a plumber), so some of those firings are answers too.
The seat displaces the blend's fifth memory and never adds a sixth. An unmeasured model gets no seat.

**On the nano's store.** The same day, on two copies of the live store, the live build left the
iPhone memory out and E4B said it did not know, twice. With the seat, that memory scored 0.795
against the question, took the seat, and E4B answered "You use an iPhone.", twice. Two questions with
no answer in the store ("Which car do I drive?", a favourite football team) found nothing closer than
0.61 and 0.62, so no seat was given and E4B said it did not know.

**What this does not show.** The table is P0's vectors (Orin, GPU, Q8_0); the live store gave one
answered question and two unanswered ones, not a rate.
