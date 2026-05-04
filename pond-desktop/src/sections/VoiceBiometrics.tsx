import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@heroui/react";
import { Mic, UserCheck, Trash2, RefreshCw } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";

// Mirrors Faces.tsx three-action layout (enroll, identify, delete) over the
// speaker-biometric endpoints. Enrollment and identification both trigger
// server-side mic recording (5 s by default) — pond-server runs on the same
// machine as the Tauri webview, so the server mic IS the user's mic.

interface Profile {
  id: string;
  display_name: string;
  avatar_emoji: string;
}

interface SpeakerEnrollment {
  id: string;
  profile_id: string;
  model: string;
  dims: number;
  created_at: string;
}

interface EnrollmentMeta {
  embedding_id: string;
  profile_id: string;
  model: string;
  dims: number;
  enrolled_count: number;
  created_at: string;
}

interface IdentifyResult {
  identified: boolean;
  profile_id: string | null;
  confidence: number | null;
}

type Busy = "idle" | "enrolling" | "identifying" | "deleting";

const RECORD_SECS = 5;

export function VoiceBiometrics() {
  const countdownRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const [profiles, setProfiles]               = useState<Profile[]>([]);
  const [selectedProfile, setSelectedProfile] = useState<string>("");
  const [enrollments, setEnrollments]         = useState<SpeakerEnrollment[]>([]);
  const [available, setAvailable]             = useState<"unknown" | "available" | "unavailable">("unknown");
  const [lastEnroll, setLastEnroll]           = useState<EnrollmentMeta | null>(null);
  const [lastIdentify, setLastIdentify]       = useState<IdentifyResult | null>(null);
  const [busy, setBusy]                       = useState<Busy>("idle");
  const [countdown, setCountdown]             = useState<number>(0);
  const [banner, setBanner]                   = useState<string | null>(null);
  const [error, setError]                     = useState<string | null>(null);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

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

  // ── Enrollment list for selected profile ─────────────────────────────────
  const refreshEnrollments = useCallback(async (pid: string) => {
    if (!pid) return;
    try {
      const res = await api.listSpeakerEnrollments(pid);
      setEnrollments(res.enrollments);
      setAvailable("available");
    } catch (e) {
      if (e instanceof ApiError && e.status === 503) {
        setAvailable("unavailable");
      } else {
        const msg = e instanceof Error ? e.message : String(e);
        setError(msg);
      }
    }
  }, []);

  useEffect(() => {
    refreshEnrollments(selectedProfile);
    setLastEnroll(null);
    setLastIdentify(null);
    setBanner(null);
    setError(null);
  }, [selectedProfile, refreshEnrollments]);

  useEffect(() => () => {
    if (countdownRef.current) clearInterval(countdownRef.current);
  }, []);

  // ── Helpers ───────────────────────────────────────────────────────────────
  function profileLabel(id: string): string {
    const p = profiles.find((p) => p.id === id);
    return p ? `${p.avatar_emoji} ${p.display_name}` : id.slice(0, 8);
  }

  function pct(v: number | null | undefined): string {
    if (v == null) return "—";
    return `${(v * 100).toFixed(1)}%`;
  }

  function startCountdown() {
    setCountdown(RECORD_SECS);
    countdownRef.current = setInterval(() => {
      setCountdown((c) => {
        if (c <= 1) { if (countdownRef.current) clearInterval(countdownRef.current); return 0; }
        return c - 1;
      });
    }, 1000);
  }

  function stopCountdown() {
    if (countdownRef.current) clearInterval(countdownRef.current);
    setCountdown(0);
  }

  function flashError(e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    if (e instanceof ApiError && e.status === 503) {
      setAvailable("unavailable");
      setError("Voice biometrics isn't enabled on this server yet. Run 'pond-server setup' first.");
    } else if (e instanceof ApiError && e.status === 404) {
      setError("Profile not found. Refresh the page and try again.");
    } else if (e instanceof ApiError && e.status === 422) {
      setError("The audio sample couldn't be processed. Speak clearly and try again in a quiet spot.");
    } else if (e instanceof ApiError && e.status >= 500) {
      setError("Something went wrong on the server. Please try again in a few seconds.");
    } else {
      setError("That didn't work — please try again. If it keeps happening, restart the app.");
      console.warn("[VoiceBiometrics] action failed:", msg);
    }
  }

  // ── Actions ───────────────────────────────────────────────────────────────
  async function handleEnroll() {
    setError(null); setBanner(null); setLastIdentify(null);
    if (!selectedProfile) { setError("Choose who you're enrolling first."); return; }
    setBusy("enrolling");
    startCountdown();
    try {
      const res = await api.enrollSpeaker(selectedProfile, RECORD_SECS);
      setLastEnroll(res);
      setAvailable("available");
      const fresh = await api.listSpeakerEnrollments(selectedProfile);
      setEnrollments(fresh.enrollments);
      const n = fresh.count;
      if (n >= 3) {
        setBanner(`Sample ${n} saved — ${profileLabel(selectedProfile)} is ready to be recognised. ✅`);
      } else {
        setBanner(`Sample ${n} of 3 saved — record ${3 - n} more in different conditions for best accuracy.`);
      }
    } catch (e) { flashError(e); }
    finally { stopCountdown(); setBusy("idle"); }
  }

  async function handleIdentify() {
    setError(null); setBanner(null); setLastIdentify(null); setLastEnroll(null);
    setBusy("identifying");
    startCountdown();
    try {
      const res = await api.identifyVoice(RECORD_SECS);
      setLastIdentify(res);
      if (res.identified && res.profile_id) {
        setBanner(`Welcome back, ${profileLabel(res.profile_id)}! 👋`);
      } else {
        setBanner(
          "Hi there — we don't recognise this voice yet. " +
          "If you're a household member, head over to enrolment and record a few samples first."
        );
      }
    } catch (e) { flashError(e); }
    finally { stopCountdown(); setBusy("idle"); }
  }

  function handleDelete() {
    if (!selectedProfile) return;
    setError(null); setBanner(null);
    setConfirmingDelete(true);
  }

  async function confirmDelete() {
    if (!selectedProfile) { setConfirmingDelete(false); return; }
    setConfirmingDelete(false);
    setError(null); setBanner(null);
    setBusy("deleting");
    try {
      await api.deleteSpeakerBiometrics(selectedProfile);
      setEnrollments([]);
      setLastEnroll(null);
      setLastIdentify(null);
      setBanner(`Voice data for ${profileLabel(selectedProfile)} has been deleted.`);
    } catch (e) { flashError(e); }
    finally { setBusy("idle"); }
  }

  // ── Render ────────────────────────────────────────────────────────────────
  const isRecording = busy === "enrolling" || busy === "identifying";

  return (
    <div style={st.root}>
      {/* Header */}
      <div style={st.header}>
        <Mic size={18} style={{ color: "var(--color-accent)" }} />
        <div>
          <h2 style={st.title}>Voice Recognition</h2>
          <p style={st.subtitle}>
            Enroll household members so Goose can tell who is speaking. Only opaque
            embedding vectors are stored — never the raw audio, and the vector
            cannot be reversed back into a voice recording.
          </p>
        </div>
      </div>

      {available === "unavailable" && (
        <div style={st.bannerWarn}>
          <strong>Voice recognition isn't set up yet.</strong> Run{" "}
          <code style={st.code}>pond-server setup</code> to download the speaker
          model, then come back to this page.
        </div>
      )}
      {error  && <div style={st.bannerError}>{error}</div>}
      {banner && <div style={st.bannerOk}>{banner}</div>}

      <div style={st.grid}>
        {/* Microphone panel */}
        <section style={st.panel}>
          <h3 style={st.panelTitle}>Microphone</h3>
          <div style={st.micVisual}>
            <div style={{
              ...st.micOrb,
              background: isRecording ? "rgba(239,68,68,0.15)" : "rgba(140,82,255,0.08)",
              border: `2px solid ${isRecording ? "rgba(239,68,68,0.5)" : "var(--color-border)"}`,
            }}>
              <Mic size={40} style={{ color: isRecording ? "#ef4444" : "var(--color-accent)", transition: "color 0.2s" }} />
            </div>
            {isRecording ? (
              <div style={{ textAlign: "center" }}>
                <p style={{ margin: 0, fontWeight: 600, color: "#ef4444", fontSize: "var(--text-sm)" }}>
                  {busy === "enrolling" ? "Recording sample… speak clearly" : "Listening… speak now"}
                </p>
                {countdown > 0 && (
                  <p style={{ margin: "var(--space-1) 0 0", fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)" }}>
                    {countdown}s remaining
                  </p>
                )}
              </div>
            ) : (
              <p style={st.hint}>
                When you click <em>Record sample</em> or <em>Identify voice</em>,
                speak naturally for {RECORD_SECS} seconds. The server captures audio
                from the local microphone.
              </p>
            )}
          </div>
        </section>

        {/* Controls panel */}
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
              isDisabled={busy !== "idle" || !selectedProfile || available === "unavailable"}
            >
              <Mic size={13} /> {busy === "enrolling" ? `Recording… ${countdown}s` : "Record sample"}
            </Button>
            <Button
              variant="outline" size="sm"
              onPress={handleIdentify}
              isDisabled={busy !== "idle" || available === "unavailable"}
            >
              <UserCheck size={13} /> {busy === "identifying" ? `Identifying… ${countdown}s` : "Identify voice"}
            </Button>
            <Button
              variant="ghost" size="sm"
              onPress={handleDelete}
              isDisabled={busy !== "idle" || !selectedProfile || confirmingDelete}
            >
              <Trash2 size={13} /> {busy === "deleting" ? "Deleting…" : "Delete voice data"}
            </Button>
          </div>

          {confirmingDelete && (
            <div style={st.confirmBox}>
              <strong style={{ display: "block", marginBottom: 6 }}>
                Delete all voice data for {profileLabel(selectedProfile)}?
              </strong>
              <span style={{ display: "block", fontSize: "var(--text-xs)", color: "var(--color-text-secondary)", marginBottom: "var(--space-2)" }}>
                This permanently removes {enrollments.length} enrolled sample{enrollments.length === 1 ? "" : "s"}. The profile itself is kept; only the voice vectors are erased.
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

          {/* Identify result */}
          {lastIdentify && (
            <div style={st.result}>
              {lastIdentify.identified ? (
                <>
                  <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                    Recognised as {profileLabel(lastIdentify.profile_id ?? "")}
                  </div>
                  <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)" }}>
                    Confidence {pct(lastIdentify.confidence)} · this looks like a strong match.
                  </div>
                </>
              ) : (
                <>
                  <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                    No matching voice found
                  </div>
                  <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)" }}>
                    {lastIdentify.confidence != null && lastIdentify.confidence > 0
                      ? "We weren't confident enough to make a match. Try in a quieter environment, or enroll another sample."
                      : "The audio was too short or too quiet. Speak up and try again."}
                  </div>
                </>
              )}
            </div>
          )}

          {/* Last enroll result */}
          {lastEnroll && !lastIdentify && (
            <div style={st.result}>
              <div style={{ fontWeight: 600, fontSize: "var(--text-sm)", marginBottom: 4 }}>
                Sample saved
              </div>
              <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-secondary)" }}>
                Model: {lastEnroll.model} · {lastEnroll.dims}-d
              </div>
              <div style={{ fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", marginTop: 2 }}>
                {new Date(lastEnroll.created_at).toLocaleString()}
              </div>
            </div>
          )}
        </section>
      </div>

      {/* Enrollment list — mirrors Faces.tsx */}
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
            No samples enrolled yet. Record at least 3 samples (different environments / distances)
            for best accuracy.
          </p>
        ) : (
          <ul style={{ listStyle: "none", padding: 0, margin: "var(--space-2) 0 0" }}>
            {enrollments.map((e) => (
              <li key={e.id} style={st.enrollmentRow}>
                <code style={{ color: "var(--color-text-tertiary)", fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)" }}>
                  {e.id.slice(0, 12)}…
                </code>
                <span style={{ fontSize: "var(--text-xs)" }}>{e.model} · {e.dims}-d</span>
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
  root:      { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  header:    { display: "flex", gap: "var(--space-3)", alignItems: "flex-start" },
  title:     { margin: 0, fontFamily: "var(--font-display)", fontWeight: 700, fontSize: "var(--text-md)", color: "var(--color-text)" },
  subtitle:  { margin: "var(--space-1) 0 0", fontSize: "var(--text-sm)", color: "var(--color-text-secondary)" },
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
  grid:      { display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))", gap: "var(--space-4)" },
  label:     { fontSize: "var(--text-xs)", fontWeight: 600, color: "var(--color-text-secondary)", marginBottom: 4 },
  select: {
    width: "100%",
    padding: "8px 10px",
    borderRadius: "var(--radius-md)",
    border: "1px solid var(--color-border-strong)",
    fontSize: "var(--text-sm)",
    background: "var(--color-bg)",
    color: "var(--color-text)",
  },
  micVisual: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "var(--space-3)",
    padding: "var(--space-5) var(--space-3)",
  },
  micOrb: {
    width: 96,
    height: 96,
    borderRadius: "50%",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    transition: "background 0.3s, border-color 0.3s",
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
  hint:        { fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", margin: 0, textAlign: "center" },
  code:        { fontFamily: "var(--font-mono)", padding: "1px 5px", background: "rgba(0,0,0,0.06)", borderRadius: 3, fontSize: "0.9em" },
};
