"""score.py MANIFEST RESULTS.jsonl... : wake hits, false wakes and WER per ear_bench run (stdlib only).

Program results (with sync and window lines) are scored against the manifest's clips; a run with no
sync line is noise or quiet, and every wake in it is a false one.
"""
import json
import re
import sys

ONES = "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen".split()
TENS = "_ _ twenty thirty forty fifty sixty seventy eighty ninety".split()


def number_words(m):
    n = int(m.group(0))
    if n < 20:
        return ONES[n]
    if n < 100:
        return TENS[n // 10] + ("" if n % 10 == 0 else " " + ONES[n % 10])
    return m.group(0)


def norm(text):
    t = text.lower().replace("’", "'")
    t = re.sub(r"\d+", number_words, t)
    t = t.replace("'", "").replace("-", " ")
    t = re.sub(r"[^a-z ]+", " ", t)
    return t.split()


def edits(ref, hyp):
    prev = list(range(len(hyp) + 1))
    for i, r in enumerate(ref, 1):
        cur = [i] + [0] * len(hyp)
        for j, h in enumerate(hyp, 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r != h))
        prev = cur
    return prev[-1]


def load(path):
    rows = [json.loads(l) for l in open(path) if l.strip()]
    run = next(r for r in rows if r["kind"] == "run")
    sync = next((r for r in rows if r["kind"] == "sync"), None)
    segs = [r for r in rows if r["kind"] == "segment"]
    wins = {r["id"]: r for r in rows if r["kind"] == "window"}
    return run, sync, segs, wins


def score(manifest, path):
    run, sync, segs, wins = load(path)
    out = {"file": path, "segments": len(segs), "audio_s": run["audio_s"],
           "ms_opening": sorted(s["ms_opening"] for s in segs)}
    if sync is None:
        out["false_wakes"] = sum(s["wake"] for s in segs)
        out["false_heard"] = [s["opening"] for s in segs if s["wake"]]
        return out
    at = lambda p: sync["first"] + (p - manifest["first_chirp"]) * sync["scale"]
    clips = []
    for c in manifest["clips"]:
        s, e = at(c["start"]), at(c["end"])
        clips.append({**c, "rs": s, "re": e})
    for s in segs:
        s["clips"] = [c for c in clips if s["start"] < c["re"] + 0.3 and s["end"] > c["rs"] - 0.3]
    hits, wake_total, false_on_plain, stray, heard_as = 0, 0, 0, 0, []
    ref_words = oracle_edits = pipe_edits = 0
    cmd_ref = cmd_edits = 0
    per_voice = {}
    for c in clips:
        wake_clip = c["sentence"].startswith("w")
        over = [s for s in segs if c in s["clips"]]
        woke = any(s["wake"] for s in over)
        if wake_clip:
            wake_total += 1
            hits += woke
            v = per_voice.setdefault(c["voice"], [0, 0])
            v[0] += woke
            v[1] += 1
            if over:
                heard_as.append((c["id"], over[0]["opening"], woke))
        elif woke and not any(x["sentence"].startswith("w") for s in over if s["wake"] for x in s["clips"]):
            false_on_plain += 1
        ref = norm(c["text"])
        ref_words += len(ref)
        win = wins.get(c["id"])
        oracle_edits += edits(ref, norm(win["transcript"])) if win else len(ref)
        mine = [s for s in segs if c["rs"] - 0.25 <= (s["start"] + s["end"]) / 2 <= c["re"] + 0.6]
        pipe_edits += edits(ref, norm(" ".join(s["transcript"] for s in mine)))
        if wake_clip and len(ref) > 2:
            # The request as the loop would get it: after the wake phrase, if it was heard at all.
            cmd = ref[2:]
            cmd_ref += len(cmd)
            hyp = norm(" ".join(s["transcript"] for s in mine)) if woke else []
            cmd_edits += edits(cmd, hyp[2:] if hyp[:2] == ["hey", "goose"] else hyp[-len(cmd):] if hyp else [])
    for s in segs:
        if s["wake"] and not s["clips"]:
            stray += 1
    out.update({
        "wake_hits": hits, "wake_clips": wake_total, "wake_rate": hits / max(wake_total, 1),
        "false_on_plain": false_on_plain, "plain_clips": len(clips) - wake_total, "stray_wakes": stray,
        "wer_oracle": oracle_edits / ref_words, "wer_pipeline": pipe_edits / ref_words,
        "wer_command": cmd_edits / max(cmd_ref, 1), "per_voice": per_voice, "heard_as": heard_as,
        "windows": {k: v["transcript"] for k, v in wins.items()},
        "ms_window": sorted(w["ms"] for w in wins.values()),
    })
    return out


def main():
    manifest = json.load(open(sys.argv[1]))
    for path in sys.argv[2:]:
        print(json.dumps(score(manifest, path)))


if __name__ == "__main__":
    main()
