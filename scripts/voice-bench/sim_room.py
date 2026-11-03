"""Renders the bench's programs as a two-microphone recording in a simulated room (numpy only), so
a beamformer can be checked offline before a room pass: the talkers from one place, the babble bed
from another, the two capsules SPACING apart.

usage: sim_room.py DIR OUT_DIR [--spacing M] [--rt60 S] [--target AZ,DIST] [--babble AZ,DIST]
  [--speaker AZ,DIST]
reads DIR/programs/{quiet,echo}.wav, DIR/beds/*.wav and manifest.json (build_program.py --split);
writes OUT_DIR/rec/S-{quiet,babble10,babble3,noise,echo}-cam.wav (stereo, 16 kHz) and
rec/manifest.json. echo.wav plays from the pond's speaker.
Azimuths are degrees from broadside, positive toward the right capsule.
"""
import json
import shutil
import sys
import wave
from pathlib import Path

import numpy as np

RATE = 16_000
C = 343.0
ROOM = np.array([4.6, 3.9, 2.6])
ARRAY = np.array([2.3, 0.35, 1.1])
RIR_S = 0.6
ORDER = 18
TAPS = 32


def option(name, default):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def read(path):
    with wave.open(str(path)) as w:
        rate, n = w.getframerate(), w.getnframes()
        x = np.frombuffer(w.readframes(n), "<i2").astype(np.float64) / 32768.0
    n = int(round(len(x) * RATE / rate))
    return np.fft.irfft(np.fft.rfft(x)[: n // 2 + 1], n) * n / len(x)


def rir(src, mic, beta):
    """Allen and Berkley's image method, frequency-independent walls, windowed-sinc delays."""
    n = int(RIR_S * RATE)
    h = np.zeros(n + TAPS)
    m = np.arange(-ORDER, ORDER + 1)
    mx, my, mz = np.meshgrid(m, m, m, indexing="ij")
    keep = (np.abs(mx) + np.abs(my) + np.abs(mz)) <= ORDER
    mx, my, mz = mx[keep], my[keep], mz[keep]
    taps = np.arange(-TAPS // 2 + 1, TAPS // 2 + 1)
    win = 0.5 + 0.5 * np.cos(np.pi * taps / (TAPS // 2 + 1))
    for px in (0, 1):
        for py in (0, 1):
            for pz in (0, 1):
                img = np.stack([(1 - 2 * px) * src[0] + 2 * mx * ROOM[0],
                                (1 - 2 * py) * src[1] + 2 * my * ROOM[1],
                                (1 - 2 * pz) * src[2] + 2 * mz * ROOM[2]], axis=1)
                d = np.linalg.norm(img - mic, axis=1)
                refl = (np.abs(mx - px) + np.abs(mx)) + (np.abs(my - py) + np.abs(my)) + (np.abs(mz - pz) + np.abs(mz))
                amp = beta ** refl / (4 * np.pi * d)
                t = d / C * RATE
                ok = t < n - TAPS
                t, amp = t[ok], amp[ok]
                i0 = np.floor(t).astype(int)
                frac = t - i0
                idx = i0[:, None] + taps[None, :]
                val = amp[:, None] * np.sinc(taps[None, :] - frac[:, None]) * win[None, :]
                np.add.at(h, idx.ravel() + TAPS // 2, val.ravel())
    return h[TAPS // 2: TAPS // 2 + n]


def conv(x, h):
    n = len(x) + len(h) - 1
    size = 1 << (n - 1).bit_length()
    return np.fft.irfft(np.fft.rfft(x, size) * np.fft.rfft(h, size), size)[: len(x)]


def diffuse(n, spacing, rng):
    """Two channels of noise with a diffuse field's coherence, sinc(2 pi f d / c), pink-ish."""
    a, b = rng.standard_normal(n), rng.standard_normal(n)
    fa, fb = np.fft.rfft(a), np.fft.rfft(b)
    f = np.fft.rfftfreq(n, 1 / RATE)
    g = np.sinc(2 * f * spacing / C)
    shape = 1 / np.sqrt(np.maximum(f, 50.0))
    left = np.fft.irfft(fa * shape, n)
    right = np.fft.irfft((g * fa + np.sqrt(1 - g ** 2) * fb) * shape, n)
    return left, right


def active_db(x):
    f = 160
    k = len(x) // f
    db = 10 * np.log10(np.maximum((x[: k * f].reshape(k, f) ** 2).mean(axis=1), 1e-12))
    loud = db[db > db.max() - 30]
    return 10 * np.log10(np.mean(10 ** (loud / 10)))


def write(path, left, right):
    x = np.stack([left, right], axis=1)
    peak = np.abs(x).max()
    if peak > 0.98:
        raise SystemExit(f"{path}: peak {peak:.2f}")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes((np.clip(x, -1, 1) * 32767).astype("<i2").tobytes())


def place(az_dist):
    az, dist = (float(v) for v in az_dist.split(","))
    a = np.radians(az)
    return ARRAY + dist * np.array([np.sin(a), np.cos(a), 0.0])


def main():
    root, out = Path(sys.argv[1]), Path(sys.argv[2])
    spacing = float(option("--spacing", 0.06))
    rt60 = float(option("--rt60", 0.4))
    target, babble = place(option("--target", "30,1.6")), place(option("--babble", "-55,2.2"))
    speaker = place(option("--speaker", "80,0.3"))
    volume = ROOM.prod()
    surface = 2 * (ROOM[0] * ROOM[1] + ROOM[0] * ROOM[2] + ROOM[1] * ROOM[2])
    beta = np.sqrt(1 - min(0.161 * volume / (surface * rt60), 0.99))
    mics = [ARRAY - np.array([spacing / 2, 0, 0]), ARRAY + np.array([spacing / 2, 0, 0])]
    ht = [rir(target, m, beta) for m in mics]
    hb = [rir(babble, m, beta) for m in mics]
    rng = np.random.default_rng(5)
    (out / "rec").mkdir(parents=True, exist_ok=True)
    shutil.copy(root / "programs" / "manifest.json", out / "rec" / "manifest.json")
    quiet = read(root / "programs" / "quiet.wav")
    talk = [conv(quiet, h) for h in ht]
    gain = 10 ** ((-36.0 - active_db(talk[0])) / 20)
    talk = [x * gain for x in talk]
    manifest = json.loads((root / "programs" / "manifest.json").read_text())
    for prog, snr in [("quiet", None), ("babble10", 10.0), ("babble3", 3.0), ("noise", 3.0)]:
        if prog == "noise":
            bed = read(root / "beds" / "noise.wav")
            sig = [np.zeros(len(bed)), np.zeros(len(bed))]
        else:
            bed = read(root / "beds" / f"{prog}.wav") if snr is not None else np.zeros(len(quiet))
            sig = [x.copy() for x in talk]
        n = len(sig[0])
        if snr is not None:
            jam = [conv(bed[:n], h) for h in hb]
            # The bed is placed at the stated ratio below the talkers' active level, at the left capsule.
            g = 10 ** ((-36.0 - snr - active_db(jam[0])) / 20)
            sig = [s + j * g for s, j in zip(sig, jam)]
        amb = diffuse(n, spacing, rng)
        floor = 10 ** (-62 / 20) / np.std(amb[0])
        sig = [s + a * floor + rng.standard_normal(n) * 10 ** (-80 / 20) for s, a in zip(sig, amb)]
        write(out / "rec" / f"S-{prog}-cam.wav", sig[0], sig[1])
        print(f"{prog}: {n / RATE:.0f} s")
    echo = read(root / "programs" / "echo.wav")
    he = [rir(speaker, m, beta) for m in mics]
    ec = [conv(echo, h) for h in he]
    g = 10 ** ((-30.0 - active_db(ec[0])) / 20)
    amb = diffuse(len(echo), spacing, rng)
    floor = 10 ** (-62 / 20) / np.std(amb[0])
    write(out / "rec" / "S-echo-cam.wav", *[e * g + a * floor for e, a in zip(ec, amb)])
    lag = lambda p: (np.linalg.norm(p - mics[0]) - np.linalg.norm(p - mics[1])) / C * RATE
    info = {"spacing_m": spacing, "rt60_s": rt60, "beta": float(beta), "target_tdoa": float(-lag(target)),
            "babble_tdoa": float(-lag(babble)),
            "speaker_tdoa": float(-lag(speaker)), "clips": len(manifest["clips"])}
    (out / "rec" / "sim.json").write_text(json.dumps(info, indent=1))
    print(info)


if __name__ == "__main__":
    main()
