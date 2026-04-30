import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@heroui/react";
import { ScanFace, Camera, UserCheck, Trash2, RefreshCw } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";

// Mirrors the web `FaceEnrollment` page. Three-action layout (enroll,
// identify, delete) over the same `/api/v1/faces/*` endpoints, styled with
// desktop tokens so it sits naturally next to Models / Prompts / Settings
// in the CONFIGURE group of the sidebar.
//
// The capture pipeline is intentionally identical to the web version: 5
// JPEG frames at ~400 ms intervals → POST /faces/identify-burst, which
// runs the inter-frame embedding-sameness + landmark-motion gates that
// catch photo / phone-screen presentation attacks single frames cannot
// see. Returns `reason: "liveness_failed"` on detected stills.

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
  rejection_reason?: RejectionReason | null;
}

type RejectionReason =
  | "no_face"
  | "quality_gate"
  | "anti_spoof"
  | "under_enrolled"
  | "below_threshold"
  | "lost_to_runner_up"
  | "open_set_gap"
  | "no_enrolled_profiles";

type Busy = "idle" | "enrolling" | "identifying" | "deleting" | "guided";

// Cues for the guided in-place enrollment flow. Designed so the user can
// complete every step from a single comfortable position — no walking
// between rooms required. Variety across rooms / times-of-day is filled
// in over time by opportunistic auto-enrollment on the server.
const GUIDED_STEPS: { title: string; hint: string }[] = [
  { title: "Look directly at the camera",       hint: "Neutral expression, eyes on the lens." },
  { title: "Slight head turn — left",           hint: "Just a small angle, like glancing away." },
  { title: "Slight head turn — right",          hint: "Same on the other side." },
  { title: "Tilt chin slightly down",           hint: "A subtle nod, eyes still on the camera." },
  { title: "Take half a step closer, then back", hint: "Recapture so the system sees you a touch nearer." },
];

function rejectionCopy(r: RejectionReason | null | undefined): { title: string; hint: string } | null {
  switch (r) {
    case "no_face":
      return {
        title: "We couldn't find a face in the camera view",
        hint:  "Make sure your face is centred in the dashed square, then try again.",
      };
    case "quality_gate":
      return {
        title: "The frame wasn't clear enough",
        hint:  "Hold your head steadier and a bit more level — extreme tilt or motion blur trips the quality check.",
      };
    case "anti_spoof":
      return {
        title: "We couldn't confirm a real, live face",
        hint:  "Move slightly or blink and try again. Photos and phone screens are blocked by design.",
      };
    case "under_enrolled":
      return {
        title: "Not enough samples on file yet",
        hint:  "Use Quick enroll below to capture a few more samples — that fills out your reference set without lowering security.",
      };
    case "below_threshold":
      return {
        title: "We weren't confident enough to make a match",
        hint:  "Try Quick enroll once more from where you usually stand — adding 2–3 in-place samples covers the lighting that's tripping us up.",
      };
    case "lost_to_runner_up":
      return {
        title: "Two profiles looked similar in this frame",
        hint:  "Capture another sample under steadier lighting so your reference set pulls clearly ahead.",
      };
    case "open_set_gap":
      return {
        title: "Your match wasn't far enough ahead of other profiles",
        hint:  "One or two more samples in your usual spot will tighten this gap — security stays the same.",
      };
    case "no_enrolled_profiles":
      return {
        title: "No one is enrolled yet",
        hint:  "Pick a household member above and use Quick enroll to capture your first samples.",
      };
    default:
      return null;
  }
}

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
  const [banner, setBanner]                   = useState<string | null>(null);
  const [error, setError]                     = useState<string | null>(null);
  // Custom in-component confirm for the destructive Delete action — the
  // browser's native `confirm()` can be ignored or no-op'd inside a Tauri
  // WebView, which is why the previous "Delete biometrics" button silently
  // did nothing. Toggling this state shows a small inline confirmation panel.
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  // Guided in-place enrollment: when active, the user is being walked
  // through GUIDED_STEPS one cue at a time. `guidedStep` is the index of
  // the *current* cue (-1 = inactive).
  const [guidedStep, setGuidedStep] = useState<number>(-1);
  const guidedActive = guidedStep >= 0;

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
        // Low-light tuning: ask the webcam ISP for continuous auto-exposure +
        // a +1 EV bias and continuous auto-white-balance.  The `advanced[]`
        // entries are best-effort per the MediaCapabilities spec — any
        // constraint the device doesn't support is silently dropped, so
        // browsers/cameras without these capabilities still honour the
        // base ideal-resolution + facingMode constraints.
        const stream = await navigator.mediaDevices.getUserMedia({
          video: {
            width:       { ideal: 640 },
            height:      { ideal: 480 },
            facingMode:  "user",
            // Cast keeps older TS lib.dom.d.ts happy: not every TS release
            // ships the image-capture extensions in MediaTrackConstraintSet.
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
      // Use the freshly-refreshed enrollment list to give a precise count
      // ("Sample 2 of 3 saved!") rather than a generic "saved".
      const fresh = await api.listFaceEnrollments(selectedProfile);
      const n = fresh.enrollments.length;
      if (n >= 3) {
        setBanner(`Sample ${n} saved — ${profileLabel(selectedProfile)} is ready to be recognised. ✅`);
      } else {
        setBanner(`Sample ${n} of 3 saved — capture ${3 - n} more from a slightly different angle for best accuracy.`);
      }
      setEnrollments(fresh.enrollments);
      setAvailable("available");
    } catch (e) { flashError(e); }
    finally { setBusy("idle"); }
  }

  function startGuided() {
    setError(null);
    setBanner(null);
    if (!selectedProfile) {
      setError("Choose who you're enrolling first, then start Quick enroll.");
      return;
    }
    setGuidedStep(0);
  }

  function cancelGuided() {
    setGuidedStep(-1);
    setBusy("idle");
  }

  // Capture a sample for the current guided step, advance to the next, and
  // wrap up when every step has been recorded. Reuses the same
  // /faces/register endpoint as the single-frame path.
  async function captureGuidedStep() {
    if (!selectedProfile || guidedStep < 0) return;
    setError(null);
    setBanner(null);
    const blob = await captureFrame();
    if (!blob) { setError("The camera isn't sending video yet — give it a moment."); return; }
    setBusy("guided");
    try {
      await api.registerFace(selectedProfile, blob);
      const fresh = await api.listFaceEnrollments(selectedProfile);
      setEnrollments(fresh.enrollments);
      const next = guidedStep + 1;
      if (next >= GUIDED_STEPS.length) {
        setGuidedStep(-1);
        setBanner(
          `Quick enroll complete — ${GUIDED_STEPS.length} new samples saved for ${profileLabel(selectedProfile)}. ✅`
        );
      } else {
        setGuidedStep(next);
      }
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
        // Friendly explanation: this happens for printed photos, phone-screen
        // images, or someone holding very still. Tell the user what to do
        // instead of leaking the technical reason.
        setError(
          "We couldn't confirm a real, live face in front of the camera. " +
          "If you're using a real face, look at the camera and blink or move slightly, then try again. " +
          "Photos and phone screens won't work — that's by design, to keep your account safe."
        );
      } else if (res.identified && res.profile_id) {
        setBanner(`Welcome back, ${profileLabel(res.profile_id)}! 👋`);
      } else if (!res.identified) {
        // Open-set non-match — frame a friendly hint instead of a raw "no match".
        setBanner(
          "Hi there — we don't recognise this face yet. " +
          "If you're a household member, head over to enrolment and capture a few samples first."
        );
      }
      setLast(res);
    } catch (e) { flashError(e); }
    finally { setBusy("idle"); }
  }

  // Open the inline confirmation panel — the actual delete fires from
  // `confirmDelete` below once the user clicks the explicit confirm button.
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
    return p ? `${p.avatar_emoji} ${p.display_name}` : id.slice(0, 8);
  }
  function pct(v: number | null | undefined): string {
    if (v == null) return "—";
    return `${(v * 100).toFixed(1)}%`;
  }

  return (
    <div style={st.root}>
      {/* Header */}
      <div style={st.header}>
        <ScanFace size={18} style={{ color: "var(--color-accent)" }} />
        <div>
          <h2 style={st.title}>Face Recognition</h2>
          <p style={st.subtitle}>
            Enroll household members so Goose can tell who is talking. Only opaque
            embedding vectors are stored — never the raw frame, and the vector
            cannot be reversed back into an image.
          </p>
        </div>
      </div>

      {available === "unavailable" && (
        <div style={st.bannerWarn}>
          <strong>Face recognition isn't turned on yet.</strong> Ask whoever set
          up Goose for you to rebuild the server with face support enabled, then
          come back to this page.
        </div>
      )}
      {error  && <div style={st.bannerError}>{error}</div>}
      {banner && <div style={st.bannerOk}>{banner}</div>}

      <div style={st.grid}>
        {/* Camera */}
        <section style={st.panel}>
          <h3 style={st.panelTitle}>Camera</h3>
          {cameraError ? (
            <p style={{ color: "var(--color-destructive)" }}>{cameraError}</p>
          ) : (
            <div style={{ position: "relative" }}>
              <video
                ref={videoRef}
                muted
                playsInline
                style={{
                  width: "100%",
                  borderRadius: "var(--radius-md)",
                  background: "#000",
                  aspectRatio: "4/3",
                }}
              />
              <div style={st.cropGuide} aria-hidden />
            </div>
          )}
          <canvas ref={canvasRef} style={{ display: "none" }} />
          <p style={st.hint}>
            Keep your face inside the dashed square. The server auto-crops to the
            largest centered square when no bounding box is supplied.
          </p>
        </section>

        {/* Controls */}
        <section style={st.panel}>
          <h3 style={st.panelTitle}>Enroll &amp; Identify</h3>

          <label style={st.label}>Household member</label>
          <select
            value={selectedProfile}
            onChange={(e) => setSelectedProfile(e.target.value)}
            disabled={profiles.length === 0 || busy !== "idle"}
            style={st.select}
          >
            {profiles.length === 0 && <option value="">No profiles — finish onboarding first</option>}
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>{p.avatar_emoji} {p.display_name}</option>
            ))}
          </select>

          <div style={{ display: "flex", flexWrap: "wrap", gap: "var(--space-2)", marginTop: "var(--space-3)" }}>
            <Button
              variant="primary" size="sm"
              onPress={handleEnroll}
              isDisabled={busy !== "idle" || !selectedProfile || available === "unavailable" || guidedActive}
            >
              <Camera size={13} /> {busy === "enrolling" ? "Enrolling…" : "Enroll sample"}
            </Button>
            <Button
              variant="outline" size="sm"
              onPress={startGuided}
              isDisabled={busy !== "idle" || !selectedProfile || available === "unavailable" || guidedActive}
            >
              <Camera size={13} /> Quick enroll ({GUIDED_STEPS.length} samples)
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

          {guidedActive && (
            <div style={st.guidedBox}>
              <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", textTransform: "uppercase", letterSpacing: "0.06em", marginBottom: 4 }}>
                Sample {guidedStep + 1} of {GUIDED_STEPS.length}
              </div>
              <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                {GUIDED_STEPS[guidedStep].title}
              </div>
              <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)", marginBottom: "var(--space-2)" }}>
                {GUIDED_STEPS[guidedStep].hint} You can stay where you are — variety across rooms is captured automatically over time.
              </div>
              <div style={{ display: "flex", gap: "var(--space-2)" }}>
                <Button variant="primary" size="sm" onPress={captureGuidedStep} isDisabled={busy === "guided"}>
                  <Camera size={13} /> {busy === "guided" ? "Saving…" : "Capture"}
                </Button>
                <Button variant="ghost" size="sm" onPress={cancelGuided} isDisabled={busy === "guided"}>
                  Cancel
                </Button>
              </div>
            </div>
          )}

          {confirmingDelete && (
            <div style={st.confirmBox}>
              <strong style={{ display: "block", marginBottom: 6 }}>
                Delete every face embedding for {profileLabel(selectedProfile)}?
              </strong>
              <span style={{ display: "block", fontSize: "var(--text-xs)", color: "var(--color-text-secondary)", marginBottom: "var(--space-2)" }}>
                This permanently removes {enrollments.length} enrolled sample{enrollments.length === 1 ? "" : "s"}. The profile itself is kept; only the biometric vectors are erased.
              </span>
              <div style={{ display: "flex", gap: "var(--space-2)" }}>
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
            <div style={st.result}>
              {last.identified ? (
                <>
                  <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                    Recognised as {profileLabel(last.profile_id ?? "")}
                  </div>
                  <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)" }}>
                    Confidence {pct(last.confidence)} · this looks like a strong match.
                  </div>
                </>
              ) : last.reason === "liveness_failed" ? (
                <>
                  <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                    That didn't look like a live face
                  </div>
                  <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)" }}>
                    Try again with your face in front of the camera and a small natural movement (a blink or slight head turn).
                  </div>
                </>
              ) : (() => {
                const copy = rejectionCopy(last.rejection_reason ?? null) ?? {
                  title: "No matching profile found",
                  hint:
                    last.confidence != null && last.confidence > 0
                      ? "We weren't confident enough to make a match. Try Quick enroll to add a couple more samples from where you usually stand."
                      : "We couldn't find a face in the camera view. Make sure your face is centred in the dashed square and try again.",
                };
                return (
                  <>
                    <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                      {copy.title}
                    </div>
                    <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)" }}>
                      {copy.hint}
                      {last.confidence != null && last.confidence > 0 && (
                        <> · best score {pct(last.confidence)} (threshold {pct(last.threshold)})</>
                      )}
                    </div>
                  </>
                );
              })()}
            </div>
          )}
        </section>
      </div>

      {/* Enrollments list */}
      <section style={st.panel}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <h3 style={st.panelTitle}>
            Samples for {selectedProfile ? profileLabel(selectedProfile) : "—"}
            {enrollments.length > 0 && (
              <span style={{ marginLeft: "var(--space-2)", fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", fontWeight: 400 }}>
                · {enrollments.length} sample{enrollments.length === 1 ? "" : "s"}
              </span>
            )}
          </h3>
          <Button variant="ghost" size="sm" onPress={() => refreshEnrollments(selectedProfile)} isDisabled={!selectedProfile}>
            <RefreshCw size={12} /> Refresh
          </Button>
        </div>

        {enrollments.length === 0 ? (
          <p style={st.hint}>
            No samples enrolled yet. Capture 3+ samples (different lighting / angles)
            for best accuracy.
          </p>
        ) : (
          <ul style={{ listStyle: "none", padding: 0, margin: "var(--space-2) 0 0" }}>
            {enrollments.map((e) => (
              <li key={e.id} style={st.enrollmentRow}>
                <code style={{ color: "var(--color-text-tertiary)", fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)" }}>{e.id.slice(0, 12)}…</code>
                <span style={{ fontSize: "var(--text-xs)" }}>{e.model_dims}-d</span>
                <span style={{ color: "var(--color-text-tertiary)", fontSize: "var(--text-xs)" }}>
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

const st: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  header: { display: "flex", gap: "var(--space-3)", alignItems: "flex-start" },
  title: { margin: 0, fontFamily: "var(--font-display)", fontWeight: 700, fontSize: "var(--text-md)", color: "var(--color-text)" },
  subtitle: { margin: "var(--space-1) 0 0", fontSize: "var(--text-sm)", color: "var(--color-text-secondary)" },
  panel: {
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-lg)",
    padding: "var(--space-4)",
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-2)",
  },
  panelTitle: { margin: 0, fontSize: "var(--text-sm)", fontWeight: 700, color: "var(--color-text)", fontFamily: "var(--font-display)", textTransform: "uppercase", letterSpacing: "0.06em" },
  grid: { display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))", gap: "var(--space-4)" },
  label: { fontSize: "var(--text-xs)", fontWeight: 600, color: "var(--color-text-secondary)", marginBottom: 4 },
  select: {
    width: "100%",
    padding: "8px 10px",
    borderRadius: "var(--radius-md)",
    border: "1px solid var(--color-border-strong)",
    fontSize: "var(--text-sm)",
    background: "var(--color-bg)",
    color: "var(--color-text)",
  },
  result: {
    marginTop: "var(--space-3)",
    padding: "var(--space-3)",
    background: "rgba(140,82,255,0.06)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-md)",
    fontSize: "var(--text-sm)",
    color: "var(--color-text)",
  },
  enrollmentRow: {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    gap: "var(--space-3)",
    padding: "var(--space-2) 0",
    borderBottom: "1px solid var(--color-border)",
  },
  bannerWarn:  { padding: "var(--space-3)", borderRadius: "var(--radius-md)", background: "rgba(245,158,11,0.12)", color: "#92400e", border: "1px solid rgba(245,158,11,0.35)", fontSize: "var(--text-sm)" },
  bannerError: { padding: "var(--space-3)", borderRadius: "var(--radius-md)", background: "rgba(239,68,68,0.10)", color: "#991b1b", border: "1px solid rgba(239,68,68,0.30)", fontSize: "var(--text-sm)" },
  bannerOk:    { padding: "var(--space-3)", borderRadius: "var(--radius-md)", background: "rgba(34,197,94,0.10)", color: "#065f46", border: "1px solid rgba(34,197,94,0.30)", fontSize: "var(--text-sm)" },
  confirmBox:  { marginTop: "var(--space-3)", padding: "var(--space-3)", borderRadius: "var(--radius-md)", background: "rgba(239,68,68,0.06)", border: "1px solid rgba(239,68,68,0.30)", fontSize: "var(--text-sm)", color: "var(--color-text)" },
  guidedBox:   { marginTop: "var(--space-3)", padding: "var(--space-3)", borderRadius: "var(--radius-md)", background: "rgba(140,82,255,0.08)", border: "1px solid rgba(140,82,255,0.35)", fontSize: "var(--text-sm)", color: "var(--color-text)" },
  hint: { fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", margin: "var(--space-2) 0 0" },
  cropGuide: {
    position: "absolute",
    top: "50%", left: "50%",
    width: "60%", aspectRatio: "1/1",
    transform: "translate(-50%, -50%)",
    border: "2px dashed rgba(255,255,255,0.75)",
    borderRadius: "var(--radius-md)",
    pointerEvents: "none",
  },
  code: { fontFamily: "var(--font-mono)", padding: "1px 6px", background: "rgba(0,0,0,0.06)", borderRadius: 4, fontSize: "0.9em" },
};
