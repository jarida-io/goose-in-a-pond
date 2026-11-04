"""table.py NAME...: one row per config and recording."""
import json, sys, os
B = os.environ.get("BENCH", os.path.dirname(os.path.abspath(__file__)))
print(f"{'config':22s} {'rec':9s} {'segs':>4s} {'wake':>6s} {'falseP':>6s} {'stray':>5s} {'werO':>6s} {'werP':>6s} {'werC':>6s} {'msO':>5s}")
for name in sys.argv[1:]:
    path = f"{B}/out/{name}/score.json"
    if not os.path.exists(path):
        continue
    for line in open(path):
        d = json.loads(line)
        rec = d["file"].split("/")[-1].replace(".jsonl", "")
        ms = d["ms_opening"]
        med = ms[len(ms) // 2] if ms else 0
        if "wake_rate" in d:
            print(f"{name:22s} {rec:9s} {d['segments']:4d} {d['wake_hits']:3d}/{d['wake_clips']:<2d} {d['false_on_plain']:6d} {d['stray_wakes']:5d} {100*d['wer_oracle']:5.1f}% {100*d['wer_pipeline']:5.1f}% {100*d['wer_command']:5.1f}% {med:5d}")
        else:
            print(f"{name:22s} {rec:9s} {d['segments']:4d} {'':6s} {d['false_wakes']:6d} {'':5s} {'':6s} {'':6s} {'':6s} {med:5d}  {d['audio_s']:.0f}s {d['false_heard'][:4]}")
