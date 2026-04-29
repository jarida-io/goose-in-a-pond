# Face Recognition

Goose-in-a-Pond can identify household members from a webcam feed so the
assistant knows who it is talking to.  Recognition runs entirely on-device:
no biometric data ever leaves the host machine, and the on-disk records
are opaque embedding vectors that cannot be reversed back into an image.

This document covers the full pipeline — model stack, the four `/api/v1/faces/*`
endpoints, anti-spoof + burst-liveness defences, the low-light
auto-exposure layer, the desktop / web UI flows, and every environment
variable that can be tuned without recompiling.

---

## End-to-End Pipeline

```
Webcam frame (JPEG)
       │
       │  ── ENROL or IDENTIFY request to /api/v1/faces/* ─────────────────────
       │
       ├─ [Image decode]                     image::load_from_memory()
       │
       ├─ [Pre-detection auto-exposure]      mean luminance < 0.30?
       │        │                              → CLAHE / 2-98 % stretch on 640²
       │        │                                detector input
       │
       ├─ [SCRFD 34G GNKPS detector]         5-pt landmarks + face bbox
       │        │                              · Adaptive score threshold:
       │        │                                0.50 normal, 0.30 dim
       │        │                              · Falls back to SCRFD 10G,
       │        │                                then UltraFace, then
       │        │                                centre-square crop
       │
       ├─ [Umeyama 112×112 alignment]        2-D similarity transform onto
       │        │                              the canonical ArcFace template
       │
       ├─ [Embedder auto-exposure]           still dim post-alignment?
       │        │                              → CLAHE / stretch on the crop
       │
       ├─ [Quality gates]
       │        ├─ MIN_CONTENT_VARIANCE      ≥ 0.006   (rejects lens-cap)
       │        ├─ MIN_MEAN_BRIGHTNESS       ≥ 0.025   (rejects total-dark)
       │        ├─ MAX_MEAN_BRIGHTNESS       ≤ 0.95    (rejects pure white)
       │        └─ MIN_LAPLACIAN_VAR_X1000   ≥ 4.0     (rejects bad blur)
       │
       ├─ [Anti-spoof ensemble]              max(spoof_score) of:
       │        ├─ Silent-Face V2            80×80 BGR multi-class softmax
       │        └─ DeepPixBis OULU-NPU       224×224 RGB sigmoid
       │                                     (filename heuristic auto-detects
       │                                      which variant to load)
       │
       ├─ [ArcFace / Glint-R100 embedder]    1×3×112×112 → 1×512 float
       │        │                              L2-normalised so cosine ≡ dot
       │
       │
       ├─ ENROL ────►  Append (profile_id, embedding, model_dims, ts)
       │              to `face_embeddings` table.  Min 3 samples required
       │              before identify_face will return that profile.
       │
       └─ IDENTIFY ─► Top-K + centroid + S-norm against all enrolled
                      embeddings.  Returns (profile_id, confidence).
                      Confidence ≥ DEFAULT_MATCH_THRESHOLD (0.62)
                      AND runner-up margin ≥ 0.08 ⇒ identified.

Burst identify (5 frames @ 400 ms) additionally runs:
       │
       ├─ [Inter-frame embedding cosine]     mean cos > 0.9994 ⇒ frozen
       │                                     (still photo / phone replay)
       ├─ [Landmark pixel motion]            std-dev across frames < 0.5 px ⇒ rigid
       ├─ [Differential landmark motion]     non-rigid component < 0.6 px ⇒ rigid
       ├─ [Eye-ratio spread]                 (max - min) / mean < 0.003 ⇒ flat
       └─ [Face-size variation]              (max - min) / mean < 1.2 % ⇒ rigid

       Hard-reject when any TWO of those five photo-like signals trip.
```

---

## Model Stack

Each slot has a **preferred** model + a **fallback** model.  The auto-downloader
on first server boot fetches both; `build_face_recognition` then prefers the
new files when present and falls back transparently when not.

| Slot          | Preferred (default download)              | Fallback                       | Notes |
| ------------- | ----------------------------------------- | ------------------------------ | ----- |
| Embedder      | **Glint-R100** (~261 MB, 512-d)           | ArcFace R50 from buffalo_l     | Drop-in replacement: same 112×112 input, same matcher math, same threshold table. Glint-R100 is the deeper backbone trained on the cleaned Glint360K corpus → +0.3-0.6 % on hard verification benchmarks vs. R50. |
| Detector      | **SCRFD 34G GNKPS** (~39 MB)              | SCRFD 10G from buffalo_l       | Same 5-point landmark contract Umeyama alignment relies on; deeper backbone catches faces ~30 % smaller pixel-wise. |
| Anti-spoof #1 | **Silent-Face MiniFASNetV2** (~2 MB)      | Heuristic gate (saturation + highlight + gradient skew) | 3-class softmax `[fake_2D, fake_3D, live]` at 80×80 BGR. The heuristic is dependency-free and runs whenever the ONNX file is absent. |
| Anti-spoof #2 | **DeepPixBis OULU-NPU** (~13 MB)          | Disabled (Silent-Face runs alone) | 224×224 RGB sigmoid head. Filename heuristic (`OULU_*`, `*deeppixbis*`, `*pixel_supervision*`, `pixbis_*`) auto-selects this variant. Adapter ensembles via `max(spoof_score)` so either model firing rejects the frame. |

### Verified ONNX mirrors (all return 200 at the time of writing)

```
Glint-R100        https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx
SCRFD 34G GNKPS   https://huggingface.co/immich-app/scrfd_34g_gnkps/resolve/main/detection/model.onnx
Silent-Face V2    https://huggingface.co/hash-ash/Silent-Face-Anti-Spoofing-ONNX/resolve/main/2.7_80x80_MiniFASNetV2.onnx
DeepPixBis OULU   https://github.com/ffletcherr/face-recognition-liveness/releases/download/v0.1/OULU_Protocol_2_model_0_0.onnx
buffalo_l (fallback) https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip
```

If a mirror dies, override at the env layer — no rebuild required:

```
POND_FACE_EMBEDDING_URL      Glint-R100 / AdaFace mirror
POND_FACE_DETECTOR_URL       SCRFD 34G mirror
POND_FACE_ANTISPOOF_URL      Silent-Face mirror
POND_FACE_ANTISPOOF_2_URL    DeepPixBis / secondary PAD mirror
```

---

## On-Disk Layout

All models live under the platform data dir, e.g.
`~/Library/Application Support/goose-in-a-pond/models/face/` on macOS:

```
adaface_ir101.onnx              ← preferred embedder (filename kept neutral
                                  so a future swap to true AdaFace doesn't
                                  require renaming on disk)
scrfd_34g.onnx                  ← preferred detector
antispoof.onnx                  ← Silent-Face V2 (primary PAD)
OULU_Protocol_2_model_0_0.onnx  ← DeepPixBis (secondary PAD,
                                  filename triggers variant detection)
w600k_r50.onnx                  ← buffalo_l ArcFace R50 fallback
scrfd.onnx                      ← buffalo_l SCRFD 10G fallback
```

Embeddings live in SQLite at `<data_dir>/system.db`, table `face_embeddings`:

```
id           TEXT PRIMARY KEY
profile_id   TEXT  → profiles.id
embedding    BLOB  (512×f32, L2-normalised, opaque)
model_dims   INTEGER  (always 512 for ArcFace family)
created_at   TEXT
```

---

## REST API

All endpoints sit behind the `face-onnx` Cargo feature.  When pond-server is
built without that feature every endpoint returns `503` with a friendly
message — the desktop / web UIs surface this as "Face recognition isn't
turned on yet" rather than a raw 503.

### `POST /api/v1/faces/register`

Multipart form: `profile_id`, `image` (JPEG/PNG), optional `bbox=x,y,w,h`.

```json
{
  "id":         "f1a3…",
  "profile_id": "550e8400-…",
  "model_dims": 512,
  "created_at": "2026-04-26T05:11:32Z"
}
```

### `POST /api/v1/faces/identify` (legacy single-frame)

Multipart `image` (+ optional `bbox`).  Vulnerable to photo attacks (no
multi-frame liveness signal) — kept for API callers who already integrate
against it.  **Do not use from new UI flows; use `/identify-burst`.**

```json
{
  "identified": true,
  "profile_id": "550e8400-…",
  "confidence": 0.81,
  "threshold":  0.62
}
```

### `POST /api/v1/faces/identify-burst` (production verification)

Multipart `image` repeated 5×, optional `bbox`.  Runs the burst-liveness
gates listed in the pipeline diagram above.  Returns the same envelope as
`/identify` plus, on a still-image attack:

```json
{
  "identified": false,
  "profile_id": null,
  "confidence": 0.0,
  "threshold":  0.62,
  "reason":     "liveness_failed",
  "liveness": {
    "hard_reject":      true,
    "suspicious":       false,
    "mean_inter_cos":   0.99971,
    "landmark_motion":  0.32,
    "eye_ratio_spread": 0.0011,
    "differential_motion": 0.18,
    "face_size_spread": 0.004
  }
}
```

### `GET /api/v1/faces/profile/{profile_id}`

Lists enrolment metadata (id, dims, created_at) for a profile.  No raw
embeddings; the BLOB stays server-side.

### `DELETE /api/v1/users/{profile_id}/biometrics`

Wipes every face embedding for a profile.

### `POST /api/v1/faces/enroll-quality`

Pre-flight quality check for a single frame (no persistence).  Returns the
same gate results an enrolment would hit, plus a numeric quality score.
The desktop / web wizards call this between captures to nudge the user
("good lighting", "hold still").

### `POST /api/v1/faces/identify-burst` (per-profile threshold override)

When migration `0014` has set a per-profile threshold (used for users with
distinctive faces or restricted access), the server uses that override
instead of `DEFAULT_MATCH_THRESHOLD`.  Read / set / clear via:

```
GET    /api/v1/faces/profile/{profile_id}/threshold
PUT    /api/v1/faces/profile/{profile_id}/threshold
DELETE /api/v1/faces/profile/{profile_id}/threshold
```

### `GET /api/v1/faces/models`

Read-only status of the four model files on disk.  Drives the Face
Recognition card on both Models pages.

```json
{
  "feature_enabled": true,
  "models_dir": "/Users/.../models/face",
  "models": [
    { "name": "adaface_ir101.onnx",            "label": "AdaFace IR-101 (preferred)", "role": "embedding", "expected_mb": 250, "size_mb": 248, "downloaded": true,  "path": "…" },
    { "name": "w600k_r50.onnx",                "label": "ArcFace R50 (fallback)",     "role": "embedding", "expected_mb": 174, "size_mb": 174, "downloaded": true,  "path": "…" },
    { "name": "scrfd_34g.onnx",                "label": "SCRFD 34G (preferred)",      "role": "detector",  "expected_mb": 140, "size_mb": 38,  "downloaded": true,  "path": "…" },
    { "name": "scrfd.onnx",                    "label": "SCRFD 10G (fallback)",       "role": "detector",  "expected_mb": 17,  "size_mb": 16,  "downloaded": true,  "path": "…" },
    { "name": "antispoof.onnx",                "label": "Silent-Face V2 (primary PAD)",       "role": "antispoof", "expected_mb": 2,   "size_mb": 2,   "downloaded": true,  "path": "…" },
    { "name": "OULU_Protocol_2_model_0_0.onnx","label": "DeepPixBis OULU-NPU (secondary PAD)","role": "antispoof", "expected_mb": 13,  "size_mb": 12,  "downloaded": true,  "path": "…" }
  ]
}
```

---

## Anti-Spoof Ensemble

```
              aligned 112×112 RGB crop  +  loose-crop full frame
                            │
                            ▼
        ┌─────────────────────────────────────────────────────────┐
        │   Silent-Face V2  (primary PAD, always runs)            │
        │   80×80 BGR  →  softmax [fake_2D, fake_3D, live]        │
        │   spoof_score = 1 - p(live)                             │
        └─────────────────────────┬───────────────────────────────┘
                                  │
                                  │  (when secondary file is present)
                                  ▼
        ┌─────────────────────────────────────────────────────────┐
        │   DeepPixBis OULU-NPU (secondary PAD)                   │
        │   224×224 RGB  →  sigmoid output_binary ∈ [0, 1]        │
        │   spoof_score = 1 - p_live                              │
        │   (filename heuristic auto-selects this variant)        │
        └─────────────────────────┬───────────────────────────────┘
                                  │
                                  ▼
                    spoof = max(primary, secondary)
                                  │
                                  ▼
                    spoof ≥ POND_FACE_ANTISPOOF_THRESHOLD?
                            │            │
                            ▼            ▼
                      reject frame   pass to embedder
```

`POND_FACE_ANTISPOOF_THRESHOLD` defaults to 0.40 for the ONNX path
(calibrated probability) and 0.65 for the heuristic fallback path
(uncalibrated score).

The `OnnxAntispoof` adapter holds a `Variant` enum (`SilentFace80` /
`DeepPixBis224`) detected at session-init time:

1. Env override wins: `POND_FACE_ANTISPOOF_VARIANT` /
   `POND_FACE_ANTISPOOF_2_VARIANT` accept `silentface` or `deeppixbis`.
2. Otherwise the basename is sniffed: any of `deeppixbis`, `oulu_protocol`,
   `oulu_npu`, `pixel_supervision`, or `pixbis_` ⇒ DeepPixBis.
3. Default ⇒ Silent-Face.

---

## Burst Liveness — How Photos Get Rejected

The burst endpoint captures **5 frames at ~400 ms intervals** and computes
five per-burst statistics that separate live faces from re-imaged photos /
phone-screen replays.  Hard-reject fires when **any two** of these
photo-like signals trip:

| Signal                  | Live face          | Photo / replay       | Floor (env-overridable)               |
| ----------------------- | ------------------ | -------------------- | ------------------------------------- |
| Inter-frame cosine      | 0.997 — 0.9992     | > 0.9995 (frozen)    | `POND_FACE_LIVENESS_INTER_COS_MAX = 0.9994` |
| Landmark motion         | 2 – 15 px         | < 0.5 px             | `POND_FACE_LIVENESS_MOTION_MIN = 0.5` |
| Differential motion     | ≥ 0.4 px / pair    | ≈ 0 (rigid translate)| `POND_FACE_LIVENESS_DIFF_MOTION_MIN = 0.60` |
| Eye-ratio spread        | 0.03 — 0.20        | < 0.003              | `POND_FACE_LIVENESS_EYE_SPREAD_MIN = 0.003` |
| Face-size variation     | 1.5 — 6 %          | < 1.2 %              | `POND_FACE_LIVENESS_SIZE_SPREAD_MIN = 0.012` |

A live person briefly satisfying one of these (e.g. a still moment between
blinks) is fine — the "any two" rule keeps real users from being
false-rejected.  A held-up phone screen typically trips three or more.

---

## Low-Light Defence

Recognition under poor lighting is a layered problem.  The defence is
applied in the order most likely to make a single frame succeed:

### Layer 1 — Webcam ISP (sensor-level correction)

The desktop and web Faces UIs request a `getUserMedia` stream with:

```
exposureMode:         "continuous"
exposureCompensation: 1.0     // +1 EV brighter
brightness:           128
whiteBalanceMode:     "continuous"
focusMode:            "continuous"
```

These are placed in `advanced[]` so any constraint the camera doesn't
support is silently dropped.  When the camera does support them (most
laptop webcams do), the ISP applies longer exposure + higher analog gain
at the sensor level — far more effective than anything we can do in
software because the data the JS pipeline ever sees is already brightened.

### Layer 2 — Pre-detection auto-exposure (full-frame)

Inside `ScrfdDetector::detect_face`, the 640×640 detector tensor is
brightened **before** SCRFD looks for faces, when its mean luminance is
< `LOW_LIGHT_TRIGGER` (0.30).  Without this, SCRFD's confidence on a
dim face drops below its detection threshold and the embedder never sees
the frame.

### Layer 3 — Adaptive SCRFD threshold

When the same-frame mean luminance is below the trigger, the detector's
score floor drops from `0.50` (configured) to `0.30` (env-overridable via
`POND_FACE_SCRFD_LOW_LIGHT_THRESH`).  SCRFD's confidence drops uniformly
on dim faces, so this admits the slightly-less-confident detections that
correspond to genuine users while the matcher threshold + burst-liveness
gates remain the real spoof safeguards.

### Layer 4 — Embedder-side auto-exposure (aligned crop)

After Umeyama alignment to the canonical 112×112 template, if the crop
itself is still dim the same auto-exposure pass fires on the aligned
crop before normalisation.  Helps when the detector worked but the
recovered face region is still in shadow.

### Layer 5 — Brightness gate floor

`MIN_MEAN_BRIGHTNESS` is `0.025` (was `0.05`).  With layers 1-4 already
brightening real-user inputs, this floor only catches genuine dark frames
(lens cap, no light at all).

### Auto-exposure algorithm

Two implementations available, dispatched by `POND_FACE_AUTO_EXPOSURE_MODE`:

```
stretch  (default)   per-channel 2-98 percentile linear stretch
                     ~0.2 ms; preserves global tonality; best for
                     uniformly-dim scenes

clahe                Contrast Limited Adaptive Histogram Equalisation
                     8×8 tiles, 4× clip-limit, bilinearly-interpolated
                     CDFs per channel
                     ~3 ms; recovers face detail in mixed-lighting
                     scenes (one bright window + dim corners) far
                     better than `stretch`
```

Disable the whole layer with `POND_FACE_AUTO_EXPOSURE=off`.

---

## UI Flows

### Desktop — `Faces` section (CONFIGURE group)

1. Webcam preview with a centred crop guide.
2. Profile selector (loaded from `/api/v1/profiles`).
3. Three actions:
   - **Enrol sample** — captures one frame, POSTs `/faces/register`,
     reports the running count ("Sample 2 of 3 saved — capture 1 more
     from a slightly different angle for best accuracy").
   - **Identify face** — captures a 5-frame burst, POSTs
     `/identify-burst`, surfaces the result as either "Welcome back, X"
     or "We couldn't confirm a real, live face" (liveness failure) or
     "No matching profile found".
   - **Delete biometrics** — opens an inline confirmation panel (the
     Tauri WebView ignores the browser `confirm()` dialog), then POSTs
     `DELETE /users/{id}/biometrics`.

### Web — `/faces` page

Same three actions over the same endpoints, plus an optional **Step 9 —
Face** in the onboarding wizard that captures three enrolment frames
against the primary profile created in Step 1.  Skipping is always
allowed — face recognition is an enhancement, not a gate.

### Both UIs — friendly status messages

| Situation             | Message                                                                                                   |
| --------------------- | --------------------------------------------------------------------------------------------------------- |
| Match found           | "Welcome back, 🦆 jerry! 👋"                                                                              |
| Liveness failed       | "We couldn't confirm a real, live face in front of the camera. Photos and phone screens won't work…"     |
| No match              | "No matching profile found" + lighting / enrolment hint                                                   |
| Sample saved          | "Sample 2 of 3 saved — capture 1 more from a slightly different angle for best accuracy."                |
| Camera not ready      | "The camera isn't sending video yet. Give it a moment, then try again."                                   |
| Server feature off    | "Face recognition isn't turned on yet. Ask whoever set up Goose for you to rebuild the server with face support enabled." |

Raw HTTP status codes / fetch errors are logged to the dev console,
never shown to the user.

---

## Configuration Reference

All env vars are optional.  Defaults are chosen to work on a fresh macOS
dev install with `cargo run -p pond-server --features face-onnx -- serve --native`.

### Model discovery / download

| Env var                          | Purpose                                                       | Default                     |
| -------------------------------- | ------------------------------------------------------------- | --------------------------- |
| `POND_FACE_MODEL_PATH`           | Embedder ONNX path                                            | adaface → arcface fallback  |
| `POND_FACE_SCRFD_PATH`           | SCRFD detector ONNX path                                      | 34G → 10G fallback          |
| `POND_FACE_DETECTOR_PATH`        | UltraFace fallback path (legacy)                              | `models/face/ultraface.onnx`|
| `POND_FACE_ANTISPOOF_PATH`       | Primary anti-spoof ONNX path                                  | `models/face/antispoof.onnx`|
| `POND_FACE_ANTISPOOF_PATH_2`     | Secondary anti-spoof ONNX path (auto-set when file on disk)   | unset                       |
| `POND_FACE_EMBEDDING_URL`        | Override embedder download URL                                | Glint-R100 mirror           |
| `POND_FACE_DETECTOR_URL`         | Override SCRFD-34G download URL                               | immich-app mirror           |
| `POND_FACE_ANTISPOOF_URL`        | Override Silent-Face download URL                             | hash-ash mirror             |
| `POND_FACE_ANTISPOOF_2_URL`      | Override DeepPixBis download URL                              | ffletcherr release          |
| `HF_TOKEN`                       | Hugging Face access token for gated downloads                 | unset                       |

### Embedder

| Env var                          | Purpose                                                       | Default |
| -------------------------------- | ------------------------------------------------------------- | ------- |
| `POND_FACE_EMBED_CHANNEL_ORDER`  | `rgb` (default) or `bgr` for non-canonical exports            | `rgb`   |
| `POND_FACE_AUTO_EXPOSURE`        | `off` to disable all auto-exposure passes                     | enabled |
| `POND_FACE_AUTO_EXPOSURE_MODE`   | `stretch` or `clahe` algorithm                                | stretch |

### Anti-spoof

| Env var                                | Purpose                                                | Default |
| -------------------------------------- | ------------------------------------------------------ | ------- |
| `POND_FACE_ANTISPOOF_THRESHOLD`        | Spoof-score reject threshold                          | 0.40 (ONNX) / 0.65 (heuristic) |
| `POND_FACE_ANTISPOOF_LIVE_INDEX`       | Slot index of "live" class in softmax output          | 2 (Silent-Face V2)             |
| `POND_FACE_ANTISPOOF_PIXEL_SCALE`      | `unit` ([0,1]) or `raw` ([0,255]) input to PAD model  | unit                           |
| `POND_FACE_ANTISPOOF_VARIANT`          | Force `silentface` or `deeppixbis` variant for primary | filename heuristic            |
| `POND_FACE_ANTISPOOF_2_VARIANT`        | Same for secondary                                     | filename heuristic            |
| `POND_FACE_ANTISPOOF_SCALE_2`          | Loose-crop scale for secondary PAD                    | 4.0                           |

### Detector (low-light tuning)

| Env var                                | Purpose                                                | Default |
| -------------------------------------- | ------------------------------------------------------ | ------- |
| `POND_FACE_SCRFD_LOW_LIGHT_THRESH`     | Detection score floor when frame is dim                | 0.30    |

### Burst liveness

| Env var                                | Purpose                                          | Default |
| -------------------------------------- | ------------------------------------------------ | ------- |
| `POND_FACE_LIVENESS_DIFF_MOTION_MIN`   | Differential landmark motion floor (px)          | 0.60    |
| `POND_FACE_LIVENESS_MOTION_MIN`        | Mean landmark motion floor (px)                  | 0.50    |
| `POND_FACE_LIVENESS_EYE_SPREAD_MIN`    | Eye-ratio spread floor                           | 0.003   |
| `POND_FACE_LIVENESS_SIZE_SPREAD_MIN`   | Inter-eye size spread floor                      | 0.012   |
| `POND_FACE_LIVENESS_INTER_COS_MAX`     | Inter-frame cosine ceiling (frozen embedding)    | 0.9994  |

### Matcher

| Env var                          | Purpose                                                       | Default |
| -------------------------------- | ------------------------------------------------------------- | ------- |
| `POND_FACE_MATCH_THRESHOLD`      | Cosine threshold for identification                           | 0.62 / 0.70 (model-dependent) |
| `POND_FACE_RUNNER_UP_MARGIN`     | Required gap between top-1 and top-2 candidates               | 0.08    |
| `POND_FACE_OPEN_SET_GAP`         | Open-set gap-to-mean rejection floor                          | 0.06    |
| `POND_FACE_MIN_SAMPLES`          | Minimum enrolments before a profile is identifiable           | 3       |

---

## Build & Run

```bash
# One-line dev launch (starts pond-server + spawns the Tauri desktop window)
cargo run -p pond-server --features face-onnx -- serve --native
```

The first boot downloads the model files (preferred + fallback ≈ 700 MB
total) into `<data_dir>/models/face/`.  Subsequent boots use the cached
files; download lines are replaced by `✅ … already present` in the log.

To verify each layer is firing, watch the boot log for:

```
INFO pond_adapters_face_onnx: face embedding ONNX model loaded model=ArcFace512 path=…/adaface_ir101.onnx
INFO pond_adapters_face_onnx::scrfd: SCRFD detector loaded path=…/scrfd_34g.onnx
INFO pond_adapters_face_onnx: anti-spoof ONNX model loaded variant=SilentFace80 path=…/antispoof.onnx
INFO pond_adapters_face_onnx: anti-spoof ONNX model loaded variant=DeepPixBis224 path=…/OULU_Protocol_2_model_0_0.onnx
INFO pond_server: SCRFD face detector loaded from … — landmark alignment enabled
INFO pond_server: face recognition enabled (model=ArcFace512, threshold=0.7, aligned=true)
```

The two `anti-spoof ONNX model loaded` lines with **different `variant=`
values** are the proof the ensemble is wired.

---

## Diagnostics

```
GET /api/v1/faces/debug/pairwise          Cross-sample cosine matrix per profile
GET /api/v1/faces/debug/eval              ROC-style calibration harness
```

Use `pairwise` to verify enrolment quality (within-profile pairs should
average ≥ 0.85, cross-profile ≤ 0.50).  Use `eval` after tuning thresholds
to check the false-accept / false-reject curve on the user's own data.

---

## Privacy

- No raw frames are persisted to disk.  Embeddings are 512 × float32 = 2 KB
  per enrolment, stored as opaque `BLOB`s in the local SQLite database.
- Embeddings cannot be reversed back into an image — they are an
  identification fingerprint, not a likeness.
- `DELETE /users/{profile_id}/biometrics` removes every embedding for a
  profile irreversibly (the desktop UI surfaces this as a destructive
  inline confirmation panel; the web UI uses `confirm()`).
- All processing is on-device.  The auto-downloader is the only network
  egress, and only the once-per-install model fetches from the URLs
  listed above.
