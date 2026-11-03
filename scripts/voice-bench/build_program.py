"""Builds the bench's playback programs: every rendered clip, levelled, in a seeded order with gaps,
between two sync chirps, optionally over a continuous babble bed at a fixed speech-to-babble ratio.

usage: build_program.py DIR GAIN_DB
writes DIR/programs/{quiet,babble10,babble3,noise}.wav and manifest.json
"""
import json
import random
import sys
import wave
from pathlib import Path

import numpy as np
from scipy.signal import resample_poly

RATE = 24_000
SPEECH_DBFS = -23.0
GAP_S = 2.6
CHIRP_S = 0.4
VOICES = ["af_heart", "am_michael", "bf_emma", "jf_alpha"]


def read(path):
    with wave.open(str(path)) as w:
        n, rate, width, ch = w.getnframes(), w.getframerate(), w.getsampwidth(), w.getnchannels()
        raw = w.readframes(n)
    if width == 2:
        x = np.frombuffer(raw, "<i2").astype(np.float64) / 32768.0
    else:
        raise SystemExit(f"{path}: width {width}")
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    return x, rate


def read_any(path):
    data = Path(path).read_bytes()
    fmt = int.from_bytes(data[20:22], "little")
    if fmt == 3:  # float32, as kokoro-synth writes
        rate = int.from_bytes(data[24:28], "little")
        i = data.find(b"data")
        return np.frombuffer(data[i + 8:], "<f4").astype(np.float64), rate
    return read(path)


def to_rate(x, rate):
    if rate == RATE:
        return x
    g = np.gcd(rate, RATE)
    return resample_poly(x, RATE // g, rate // g)


def active_rms_db(x):
    f = 240
    n = len(x) // f
    frames = x[: n * f].reshape(n, f)
    db = 10 * np.log10(np.maximum((frames ** 2).mean(axis=1), 1e-12))
    loud = db[db > db.max() - 30]
    return 10 * np.log10(np.mean(10 ** (loud / 10)))


def trim(x, floor_db=-50):
    f = 240
    n = len(x) // f
    db = 10 * np.log10(np.maximum((x[: n * f].reshape(n, f) ** 2).mean(axis=1), 1e-12))
    on = np.where(db > max(db.max() - 40, floor_db))[0]
    if len(on) == 0:
        return x
    a, b = max(on[0] - 5, 0) * f, min(on[-1] + 6, n) * f
    return x[a:b]


def level(x, db):
    return x * 10 ** ((db - active_rms_db(x)) / 20)


def chirp():
    t = np.arange(int(CHIRP_S * RATE)) / RATE
    f0, f1 = 500.0, 4000.0
    ph = 2 * np.pi * (f0 * t + (f1 - f0) / (2 * CHIRP_S) * t ** 2)
    return 0.25 * np.sin(ph) * np.hanning(len(t))


def babble_bed(root, seconds, seed):
    clips = sorted((root / "babble").glob("*.wav"))
    rng = random.Random(seed)
    n = int(seconds * RATE)
    bed = np.zeros(n)
    for stream in range(6):
        pos = rng.randrange(0, RATE)
        while pos < n:
            x, r = read_any(rng.choice(clips))
            x = trim(to_rate(x, r))
            if len(x) < RATE // 3 or not np.isfinite(x).all():
                continue
            x = level(x, -23.0)
            end = min(pos + len(x), n)
            bed[pos:end] += x[: end - pos]
            pos = end + int(rng.uniform(0.05, 0.4) * RATE)
    return bed


def write(path, x):
    if not np.isfinite(x).all():
        raise SystemExit(f"{path}: not finite")
    peak = np.abs(x).max()
    if peak > 0.98:
        raise SystemExit(f"{path}: peak {peak:.2f}")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes((np.clip(x, -1, 1) * 32767).astype("<i2").tobytes())


def main():
    root = Path(sys.argv[1])
    gain = float(sys.argv[2])
    speech = SPEECH_DBFS + gain
    sentences = dict(line.rstrip("\n").split("\t", 1) for line in open(root / "sentences.tsv"))
    items = [(v, s) for v in VOICES for s in sentences]
    random.Random(42).shuffle(items)
    pieces, clips = [np.zeros(RATE)], []
    pieces.append(chirp() * 10 ** (gain / 20))
    t = sum(len(p) for p in pieces) + 2 * RATE
    pieces.append(np.zeros(2 * RATE))
    for voice, sid in items:
        x, r = read_any(root / "tts" / voice / f"{sid}.wav")
        x = level(trim(to_rate(x, r)), speech)
        clips.append({"id": f"{voice}/{sid}", "voice": voice, "sentence": sid, "text": sentences[sid],
                      "start": t / RATE, "end": (t + len(x)) / RATE})
        pieces += [x, np.zeros(int(GAP_S * RATE))]
        t += len(x) + int(GAP_S * RATE)
    pieces.append(chirp() * 10 ** (gain / 20))
    end_chirp = t / RATE
    pieces.append(np.zeros(RATE))
    program = np.concatenate(pieces)
    first_chirp = 1.0
    out = root / "programs"
    out.mkdir(exist_ok=True)
    write(out / "quiet.wav", program)
    bed = babble_bed(root, len(program) / RATE, seed=7)
    bed = level(bed, speech)  # the bed's active level equals the speech level before the ratio
    fade = np.zeros(len(program))
    a, b = int((first_chirp + CHIRP_S + 0.5) * RATE), int((end_chirp - 0.5) * RATE)
    fade[a:b] = 1.0
    ramp = int(0.5 * RATE)
    fade[a:a + ramp] = np.linspace(0, 1, ramp)
    fade[b - ramp:b] = np.linspace(1, 0, ramp)
    for name, snr in [("babble10", 10.0), ("babble3", 3.0)]:
        write(out / f"{name}.wav", program + bed * fade * 10 ** (-snr / 20))
    noise_s = 300
    noise = np.concatenate([np.zeros(RATE), chirp() * 10 ** (gain / 20), np.zeros(RATE),
                            level(babble_bed(root, noise_s, seed=11), speech) * 10 ** (-3.0 / 20),
                            np.zeros(RATE), chirp() * 10 ** (gain / 20), np.zeros(RATE)])
    write(out / "noise.wav", noise)
    manifest = {"rate": RATE, "speech_dbfs": speech, "first_chirp": first_chirp, "end_chirp": end_chirp,
                "clips": clips, "program_seconds": len(program) / RATE,
                "noise": {"first_chirp": 1.0, "end_chirp": 2.0 + CHIRP_S + noise_s + 1.0,
                          "seconds": len(noise) / RATE}}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1))
    print(f"{len(clips)} clips, program {len(program) / RATE / 60:.1f} min, speech {speech:.1f} dBFS")


if __name__ == "__main__":
    main()
