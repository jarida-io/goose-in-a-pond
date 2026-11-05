"""Reads embed-scores output for two models and reports tool-selection recall and surplus per
floor, and the dedup cosines per label, so EmbeddingGemma 2's floors can be set to match what
MiniLM's do today."""
import json
import statistics
import sys

# Never offered to an ordinary turn, so they are not part of the choice being measured.
INTERNAL = {"giap-interview", "giap-task-work"}


def scored_needs(message):
    """The needed groups embed-scores scored. A core group (giap-knowledge since 2026-11-02) is
    loaded without scoring, so a need on it is met whatever the floor and is left out."""
    return [g for g in message["need"] if g in message["scores"]]


def load(path):
    return json.load(open(path))


def selection(scores, floor):
    groups = {g: s for g, s in scores.items() if g not in INTERNAL}
    top = max(groups, key=groups.get)
    return {g for g, s in groups.items() if s >= floor} | {top}


def tools_at(data, floor):
    needed = kept = surplus = 0
    missed = []
    for m in data["messages"]:
        chosen = selection(m["scores"], floor)
        for g in scored_needs(m):
            needed += 1
            if g in chosen:
                kept += 1
            else:
                missed.append((m["text"], g, round(m["scores"][g], 3)))
        surplus += len(chosen - set(m["need"]))
    return kept / needed, surplus / len(data["messages"]), missed


def describe(values):
    values = sorted(values)
    return "min %.3f  p10 %.3f  median %.3f  p90 %.3f  max %.3f" % (
        values[0],
        values[max(0, int(len(values) * 0.1) - 1)],
        statistics.median(values),
        values[min(len(values) - 1, int(len(values) * 0.9))],
        values[-1],
    )


def report(name, data, floors):
    print("==", name, data["model_id"])
    needed_scores = [m["scores"][g] for m in data["messages"] for g in scored_needs(m)]
    unneeded = [s for m in data["messages"] for g, s in m["scores"].items()
                if g not in m["need"] and g not in INTERNAL]
    print("  needed group cosines  ", describe(needed_scores))
    print("  unneeded group cosines", describe(unneeded))
    for f in floors:
        recall, surplus, missed = tools_at(data, f)
        print("  floor %.2f  recall %.3f  surplus groups/turn %.2f  missed %d" % (f, recall, surplus, len(missed)))
    for label in ("same", "related", "fresh"):
        values = [p["cosine"] for p in data["pairs"] if p["label"] == label]
        print("  dedup %-8s" % label, describe(values))
    return needed_scores


if __name__ == "__main__":
    minilm, eg2 = load(sys.argv[1]), load(sys.argv[2])
    report("MiniLM", minilm, [0.20, 0.24, 0.28, 0.32])
    grid = [round(0.20 + 0.02 * i, 2) for i in range(26)]
    report("EmbeddingGemma 2", eg2, grid)
    ref_recall, ref_surplus, ref_missed = tools_at(minilm, 0.28)
    print("\nMiniLM at 0.28: recall %.3f, surplus %.2f" % (ref_recall, ref_surplus))
    for text, g, s in ref_missed:
        print("   missed", g, s, text)
    best = None
    for f in grid:
        recall, surplus, _ = tools_at(eg2, f)
        if recall >= ref_recall:
            best = f
    if best is not None:
        recall, surplus, missed = tools_at(eg2, best)
        print("EG2 highest floor with recall >= MiniLM's: %.2f (recall %.3f, surplus %.2f)" % (best, recall, surplus))
        for text, g, s in missed:
            print("   missed", g, s, text)
    for name, data in (("MiniLM", minilm), ("EmbeddingGemma 2", eg2)):
        print("\n%s pairs, highest first:" % name)
        for p in sorted(data["pairs"], key=lambda p: -p["cosine"]):
            print("  %.3f %-7s %s | %s" % (p["cosine"], p["label"], p["stored"], p["candidate"]))
