import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Button } from "@heroui/react";
import { ScanFace, Camera, UserCheck, Trash2, RefreshCw, CheckCircle2, Hand } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { useDialogFocusTrap } from "../components/shared";

// Mirrors the web `FaceEnrollment` page; keep capture identical: 5 JPEG frames ~400 ms apart to
// /faces/identify-burst, whose inter-frame liveness gates catch photos and phone screens.

interface Profile {
  id: string;
  display_name: string;
  avatar_emoji: string;
}

interface Enrollment {
  id: string;
  profile_id: string;
  model_dims: number;
  created_at: string;
}

interface IdentifyResult {
  identified: boolean;
  profile_id: string | null;
  confidence: number | null;
  threshold: number;
  reason?: string;
}

type Busy = "idle" | "enrolling" | "identifying" | "deleting";

export function Faces() {
  const videoRef    = useRef<HTMLVideoElement | null>(null);
  const canvasRef   = useRef<HTMLCanvasElement | null>(null);
  const streamRef   = useRef<MediaStream | null>(null);

  const [profiles, setProfiles]               = useState<Profile[]>([]);
  const [selectedProfile, setSelectedProfile] = useState<string>("");
  const [enrollments, setEnrollments]         = useState<Enrollment[]>([]);
  const [available, setAvailable]             = useState<"unknown" | "available" | "unavailable">("unknown");
  const [cameraError, setCameraError]         = useState<string | null>(null);
  const [busy, setBusy]                       = useState<Busy>("idle");
  const [last, setLast]                       = useState<IdentifyResult | null>(null);
  const [banner, setBanner]                   = useState<ReactNode | null>(null);
  const [error, setError]                     = useState<string | null>(null);
  // In-component confirm for Delete: native `confirm()` can silently no-op in the app's WebView.
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const householdLabelId = useId();
  const confirmTitleId = useId();
  const confirmDialogRef = useDialogFocusTrap<HTMLDivElement>(confirmingDelete, () => setConfirmingDelete(false));

  // ── Profiles ─────────────────────────────────────────────────────────────
  useEffect(() => {
    api.listProfiles()
      .then((res) => {
        setProfiles(res.profiles);
        if (res.profiles.length > 0 && !selectedProfile) {
          setSelectedProfile(res.profiles[0].id);
        }
      })
      .catch(() => setError("We couldn't load your household profiles right now. Try again in a moment."));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Camera ───────────────────────────────────────────────────────────────
  useEffect(() => {
    let cancelled = false;
    async function start() {
      if (!navigator.mediaDevices?.getUserMedia) {
        setCameraError("This webview does not support getUserMedia.");
        return;
      }
      try {
        // Low-light tuning; `advanced[]` entries are best-effort, so unsupported ones are silently dropped.
        const stream = await navigator.mediaDevices.getUserMedia({
          video: {
            width:       { ideal: 640 },
            height:      { ideal: 480 },
            facingMode:  "user",
            // Cast: not every TS lib.dom ships the image-capture fields of MediaTrackConstraintSet.
            advanced: [{
              exposureMode:         "continuous",
              exposureCompensation: 1.0,    // +1 EV brighter
              brightness:           128,
              whiteBalanceMode:     "continuous",
              focusMode:            "continuous",
            }] as unknown as MediaTrackConstraintSet[],
          },
          audio: false,
        });
        if (cancelled) { stream.getTracks().forEach((t) => t.stop()); return; }
        streamRef.current = stream;
        if (videoRef.current) {
          videoRef.current.srcObject = stream;
          await videoRef.current.play().catch(() => {/* autoplay policy */});
        }
      } catch (e) {
        setCameraError(e instanceof Error ? e.message : "Camera permission denied.");
      }
    }
    start();
    return () => {
      cancelled = true;
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    };
  }, []);

  // ── Enrollments for selected profile ─────────────────────────────────────
  const refreshEnrollments = useCallback(async (pid: string) => {
    if (!pid) return;
    try {
      const res = await api.listFaceEnrollments(pid);
      setEnrollments(res.enrollments);
      setAvailable("available");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (e instanceof ApiError && e.status === 503) {
        setAvailable("unavailable");
      } else if (msg.includes("503")) {
        setAvailable("unavailable");
      } else {
        setError(msg);
      }
    }
  }, []);
  useEffect(() => { refreshEnrollments(selectedProfile); }, [selectedProfile, refreshEnrollments]);

  // ── Capture helpers ──────────────────────────────────────────────────────
  async function captureFrame(): Promise<Blob | null> {
    const v = videoRef.current;
    const c = canvasRef.current;
    if (!v || !c || v.videoWidth === 0) return null;
    c.width = v.videoWidth; c.height = v.videoHeight;
    const ctx = c.getContext("2d");
    if (!ctx) return null;
    ctx.drawImage(v, 0, 0, c.width, c.height);
    return new Promise((r) => c.toBlob((b) => r(b), "image/jpeg", 0.92));
  }

  function flashError(e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    if (e instanceof ApiError && e.status === 503) {
      setAvailable("unavailable");
      setError("Face recognition isn't enabled on this server yet. Ask your installer to rebuild it with face support turned on.");
    } else if (e instanceof ApiError && e.status === 401) {
      setError("Your session expired. Refresh the app to sign back in.");
    } else if (e instanceof ApiError && e.status >= 500) {
      setError("Something went wrong on the server. Please try again in a few seconds.");
    } else {
      // Don't dump raw HTTP / fetch errors at the user.
      setError("That didn't work — please try again. If it keeps happening, restart the app.");
      console.warn("[Faces] action failed:", msg);
    }
  }

  // ── Actions ──────────────────────────────────────────────────────────────
  async function handleEnroll() {
    setError(null); setBanner(null);
    if (!selectedProfile) { setError("Choose who you're enrolling first, then capture a sample."); return; }
    const blob = await captureFrame();
    if (!blob) { setError("The camera isn't sending video yet. Give it a moment, then try again."); return; }
    setBusy("enrolling");
    try {
      await api.registerFace(selectedProfile, blob);
      const fresh = await api.listFaceEnrollments(selectedProfile);
      const n = fresh.enrollments.length;
      if (n >= 3) {
        setBanner(<><CheckCircle2 size={14} />{" "}Sample {n} saved — {profileLabel(selectedProfile)} is ready to be recognised.</>);
      } else {
        setBanner(`Sample ${n} of 3 saved — capture ${3 - n} more from a slightly different angle for best accuracy.`);
      }
      setEnrollments(fresh.enrollments);
      setAvailable("available");
    } catch (e) { flashError(e); }
    finally { setBusy("idle"); }
  }

  async function handleIdentify() {
    setError(null); setBanner(null); setLast(null);
    setBusy("identifying");
    try {
      const frames: Blob[] = [];
      for (let i = 0; i < 5; i++) {
        if (i > 0) await new Promise((r) => setTimeout(r, 400));
        const b = await captureFrame();
        if (!b) { setError("Could not capture a frame — is the camera ready?"); return; }
        frames.push(b);
      }
      const res = await api.identifyFaceBurst(frames);
      if (res.reason === "liveness_failed") {
        setError(
          "We couldn't confirm a real, live face in front of the camera. " +
          "If you're using a real face, look at the camera and blink or move slightly, then try again. " +
          "Photos and phone screens won't work — that's by design, to keep your account safe."
        );
      } else if (res.identified && res.profile_id) {
        setBanner(<><Hand size={14} />{" "}Welcome back, {profileLabel(res.profile_id)}!</>);
      } else if (!res.identified) {
        setBanner(
          "Hi there — we don't recognise this face yet. " +
          "If you're a household member, head over to enrolment and capture a few samples first."
        );
      }
      setLast(res);
    } catch (e) { flashError(e); }
    finally { setBusy("idle"); }
  }

  // Only opens the confirm panel; `confirmDelete` does the delete.
  function handleDelete() {
    if (!selectedProfile) return;
    setError(null);
    setBanner(null);
    setConfirmingDelete(true);
  }

  async function confirmDelete() {
    if (!selectedProfile) { setConfirmingDelete(false); return; }
    setConfirmingDelete(false);
    setError(null); setBanner(null);
    setBusy("deleting");
    try {
      const res = await api.deleteUserBiometrics(selectedProfile);
      const n = res.face_embeddings_deleted;
      setBanner(
        n === 0
          ? `${profileLabel(selectedProfile)} had nothing on file — you're all clear.`
          : `Removed ${n} face sample${n === 1 ? "" : "s"} for ${profileLabel(selectedProfile)}.`
      );
      await refreshEnrollments(selectedProfile);
    } catch (e) { flashError(e); }
    finally { setBusy("idle"); }
  }

  // ── Render helpers ───────────────────────────────────────────────────────
  function profileLabel(id: string): string {
    const p = profiles.find((p) => p.id === id);
    return p ? p.display_name : id.slice(0, 8);
  }
  function pct(v: number | null | undefined): string {
    if (v == null) return "—";
    return `${(v * 100).toFixed(1)}%`;
  }

  return (
    <div className="faces-root">
      {/* Header */}
      <div className="faces-header">
        <ScanFace size={18} style={{ color: "var(--color-accent)" }} />
        <div>
          <h2 className="faces-title">Face Recognition</h2>
          <p className="faces-subtitle">
            Enroll household members so Goose can tell who is talking. Only opaque
            embedding vectors are stored — never the raw frame, and the vector
            cannot be reversed back into an image.
          </p>
        </div>
      </div>

      {available === "unavailable" && (
        <div className="faces-banner--warn">
          <strong>Face recognition isn't turned on yet.</strong> Ask whoever set
          up Goose for you to rebuild the server with face support enabled, then
          come back to this page.
        </div>
      )}
      {error  && <div className="faces-banner--error">{error}</div>}
      {banner && <div className="faces-banner--ok">{banner}</div>}

      <div className="faces-grid">
        {/* Camera */}
        <section className="faces-panel">
          <h3 className="faces-panel__title">Camera</h3>
          {cameraError ? (
            <p className="faces-cam-error">{cameraError}</p>
          ) : (
            <div className="faces-video-wrap">
              <video
                ref={videoRef}
                muted
                playsInline
                className="faces-video"
              />
              <div className="faces-crop-guide" aria-hidden />
            </div>
          )}
          <canvas ref={canvasRef} hidden />
          <p className="faces-hint">
            Keep your face inside the dashed square. The server auto-crops to the
            largest centered square when no bounding box is supplied.
          </p>
        </section>

        {/* Controls */}
        <section className="faces-panel">
          <h3 className="faces-panel__title">Enroll &amp; Identify</h3>

          <label htmlFor={householdLabelId} className="faces-label">Household member</label>
          <select
            id={householdLabelId}
            value={selectedProfile}
            onChange={(e) => setSelectedProfile(e.target.value)}
            disabled={profiles.length === 0 || busy !== "idle"}
            className="faces-select"
          >
            {profiles.length === 0 && <option value="">No profiles — finish onboarding first</option>}
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>{p.display_name}</option>
            ))}
          </select>

          <div className="faces-btn-row">
            <Button
              variant="primary" size="sm"
              onPress={handleEnroll}
              isDisabled={busy !== "idle" || !selectedProfile || available === "unavailable"}
            >
              <Camera size={13} /> {busy === "enrolling" ? "Enrolling…" : "Enroll sample"}
            </Button>
            <Button
              variant="outline" size="sm"
              onPress={handleIdentify}
              isDisabled={busy !== "idle" || available === "unavailable"}
            >
              <UserCheck size={13} /> {busy === "identifying" ? "Identifying…" : "Identify face"}
            </Button>
            <Button
              variant="ghost" size="sm"
              onPress={handleDelete}
              isDisabled={busy !== "idle" || !selectedProfile || confirmingDelete}
            >
              <Trash2 size={13} /> {busy === "deleting" ? "Deleting…" : "Delete biometrics"}
            </Button>
          </div>

          {confirmingDelete && (
            <div
              ref={confirmDialogRef}
              className="faces-confirm"
              role="alertdialog"
              aria-modal="true"
              aria-labelledby={confirmTitleId}
              tabIndex={-1}
            >
              <strong id={confirmTitleId} className="faces-confirm__title">
                Delete every face embedding for {profileLabel(selectedProfile)}?
              </strong>
              <span className="faces-confirm__desc">
                This permanently removes {enrollments.length} enrolled sample{enrollments.length === 1 ? "" : "s"}. The profile itself is kept; only the biometric vectors are erased.
              </span>
              <div className="faces-confirm__actions">
                <Button variant="ghost" size="sm" onPress={() => setConfirmingDelete(false)}>
                  Cancel
                </Button>
                <Button variant="primary" size="sm" onPress={confirmDelete}>
                  Yes, delete {enrollments.length} sample{enrollments.length === 1 ? "" : "s"}
                </Button>
              </div>
            </div>
          )}

          {last && (
            <div className="faces-result">
              {last.identified ? (
                <>
                  <div className="faces-result__heading">
                    Recognised as {profileLabel(last.profile_id ?? "")}
                  </div>
                  <div className="faces-result__body">
                    Confidence {pct(last.confidence)} · this looks like a strong match.
                  </div>
                </>
              ) : last.reason === "liveness_failed" ? (
                <>
                  <div className="faces-result__heading">
                    That didn't look like a live face
                  </div>
                  <div className="faces-result__body">
                    Try again with your face in front of the camera and a small natural movement (a blink or slight head turn).
                  </div>
                </>
              ) : (
                <>
                  <div className="faces-result__heading">
                    No matching profile found
                  </div>
                  <div className="faces-result__body">
                    {last.confidence != null && last.confidence > 0
                      ? "We weren't confident enough to make a match. Try a brighter spot, or enrol another sample under similar lighting."
                      : "We couldn't find a face in the camera view. Make sure your face is centred in the dashed square and try again."}
                  </div>
                </>
              )}
            </div>
          )}
        </section>
      </div>

      {/* Enrollments list */}
      <section className="faces-panel">
        <div className="faces-panel__head">
          <h3 className="faces-panel__title">
            Samples for {selectedProfile ? profileLabel(selectedProfile) : "—"}
            {enrollments.length > 0 && (
              <span className="faces-sample-count">
                · {enrollments.length} sample{enrollments.length === 1 ? "" : "s"}
              </span>
            )}
          </h3>
          <Button variant="ghost" size="sm" onPress={() => refreshEnrollments(selectedProfile)} isDisabled={!selectedProfile}>
            <RefreshCw size={12} /> Refresh
          </Button>
        </div>

        {enrollments.length === 0 ? (
          <p className="faces-hint">
            No samples enrolled yet. Capture 3+ samples (different lighting / angles)
            for best accuracy.
          </p>
        ) : (
          <ul className="faces-enrollments">
            {enrollments.map((e) => (
              <li key={e.id} className="faces-enrollment-row">
                <code className="faces-enrollment-id">{e.id.slice(0, 12)}…</code>
                <span className="faces-enrollment-dims">{e.model_dims}-d</span>
                <span className="faces-enrollment-date">
                  {new Date(e.created_at).toLocaleString()}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

