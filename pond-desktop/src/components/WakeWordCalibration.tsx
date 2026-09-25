import { useState, useEffect, useRef, useCallback } from "react";
import { invoke, isDesktopShell } from "../shell";
import { recordFixedDuration, micErrorMessage, type FixedRecorder } from "../modes/voice/micRecorder";
import { Button } from "@heroui/react";
import { Mic, RotateCcw, Check, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { AudioWaves } from "./AudioWaves";

// ── Types ──────────────────────────────────────────────────────

type Phase = "idle" | "countdown" | "recording" | "review" | "submitting" | "error" | "complete";

interface Props {
  /** The wake phrase to calibrate */
  phrase: string;
  /** Called when calibration succeeds or user clicks Done */
  onComplete: () => void;
  /** Called when user cancels */
  onCancel: () => void;
}

const RECORD_DURATION_MS = 3500;
const COUNTDOWN_TICK_MS = 800;
const MIN_SAMPLES_FOR_DONE = 3;

// ── Calibration sentences ─────────────────────────────────────
// Sentences containing the wake word capture natural pronunciation; the server extracts it.

function buildCalibrationPrompts(phrase: string): string[] {
  const p = phrase.trim();
  return [
    `${p}`,
    `Hey ${p}, good morning`,
    `OK ${p}, what time is it`,
    `${p}, tell me a joke`,
    `Thank you ${p}`,
  ];
}

// ── Helpers ────────────────────────────────────────────────────

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// ── Component ──────────────────────────────────────────────────

export function WakeWordCalibration({ phrase, onComplete, onCancel }: Props) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [sampleCount, setSampleCount] = useState(0);
  const [targetCount, setTargetCount] = useState(5);
  const [variants, setVariants] = useState<string[]>([]);
  const [lastTranscript, setLastTranscript] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [audioLevel, setAudioLevel] = useState(0);
  const [countdownValue, setCountdownValue] = useState(3);

  const [pendingWav, setPendingWav] = useState<ArrayBuffer | null>(null);

  const [prompts] = useState(() => buildCalibrationPrompts(phrase));

  const prevSampleCount = useRef(0);
  const abortedRef = useRef(false);
  const recorderRef = useRef<FixedRecorder | null>(null);

  // ── Cleanup on unmount ────────────────────────────────────────
  // Reset on mount: StrictMode's remount reuses the ref, so a stale `true` would block sampling.
  useEffect(() => {
    abortedRef.current = false;
    return () => {
      abortedRef.current = true;
      recorderRef.current?.abort();
      recorderRef.current = null;
    };
  }, []);

  // ── Record a sample (stops at review phase) ───────────────────
  const startSample = useCallback(async () => {
    if (abortedRef.current) return;
    setErrorMsg(null);
    setLastTranscript(null);
    setPendingWav(null);

    // 1. Countdown 3-2-1
    setPhase("countdown");
    for (let i = 3; i >= 1; i--) {
      if (abortedRef.current) return;
      setCountdownValue(i);
      await sleep(COUNTDOWN_TICK_MS);
    }
    if (abortedRef.current) return;

    // 2. Stop any live voice session: it holds the mic exclusively and would hear the sample.
    if (isDesktopShell()) {
      try {
        await invoke("stop_voice_session");
      } catch {
        // Nothing was running, which is the common case.
      }
    }

    // 3. Record a fixed window, with the level coming back as it goes.
    setPhase("recording");
    const recorder = recordFixedDuration(RECORD_DURATION_MS, setAudioLevel);
    recorderRef.current = recorder;

    let wav: ArrayBuffer;
    try {
      ({ wav } = await recorder.done);
    } catch (e) {
      recorderRef.current = null;
      setAudioLevel(0);
      if (abortedRef.current) return;
      setErrorMsg(micErrorMessage(e));
      setPhase("error");
      return;
    }
    recorderRef.current = null;
    setAudioLevel(0);
    if (abortedRef.current) return;

    // 4. Hold at review — let the user decide to submit or discard
    setPendingWav(wav);
    setPhase("review");
  }, []);

  // ── Submit the reviewed sample to the calibration API ─────────
  const submitSample = useCallback(async () => {
    if (!pendingWav || abortedRef.current) return;
    prevSampleCount.current = sampleCount;
    setPhase("submitting");

    try {
      const result = await api.calibrateWakeWord(pendingWav);

      if (abortedRef.current) return;

      setPendingWav(null);
      setLastTranscript(result.transcript);
      setTargetCount(result.target_count);
      setVariants(result.all_variants);

      // Unchanged sample_count means a duplicate variant.
      if (result.sample_count === prevSampleCount.current && !result.complete) {
        setSampleCount(result.sample_count);
        setErrorMsg("Already have that transcription. Try the next sentence — the different context helps capture a new variant.");
        setPhase("error");
        return;
      }

      setSampleCount(result.sample_count);

      if (result.complete) {
        setPhase("complete");
      } else {
        setPhase("idle");
      }
    } catch (e) {
      if (abortedRef.current) return;
      if (e instanceof ApiError) {
        if (e.status === 422) {
          setErrorMsg("No speech detected. Speak more clearly and try again.");
        } else if (e.status === 502) {
          setErrorMsg("Speech recognition service unavailable. Check that Whisper is running.");
        } else {
          setErrorMsg(e.message);
        }
      } else {
        setErrorMsg(String(e));
      }
      setPhase("error");
    }
  }, [pendingWav, sampleCount]);

  // ── Discard the pending recording ─────────────────────────────
  const discardSample = useCallback(() => {
    setPendingWav(null);
    setPhase("idle");
  }, []);

  // ── Reset / Start Over ────────────────────────────────────────
  const handleReset = useCallback(async () => {
    try {
      await api.resetWakeWordCalibration();
    } catch { /* ignore */ }
    setSampleCount(0);
    setVariants([]);
    setLastTranscript(null);
    setErrorMsg(null);
    setPhase("idle");
  }, []);

  // ── Render ────────────────────────────────────────────────────

  return (
    <div style={styles.root}>
      {/* Header + progress dots */}
      <div style={styles.header}>
        <p style={styles.title}>
          Calibrate: <span style={styles.phrase}>"{phrase}"</span>
        </p>
        <div style={styles.dots}>
          {Array.from({ length: targetCount }, (_, i) => (
            <span
              key={i}
              style={{
                ...styles.dot,
                background: i < sampleCount ? "var(--color-accent)" : "var(--color-border-strong)",
              }}
            />
          ))}
        </div>
        <p style={styles.hint}>
          {sampleCount} of {targetCount} samples — read the sentence aloud
        </p>
      </div>

      {/* Center stage */}
      <div style={styles.stage}>
        {phase === "idle" && (
          <div style={styles.centerCol}>
            {lastTranscript && (
              <p style={styles.feedback}>
                Heard: "<span style={styles.feedbackText}>{lastTranscript}</span>"
              </p>
            )}
            <p style={styles.promptSentence}>
              "{prompts[sampleCount % prompts.length]}"
            </p>
            <Button
              variant="primary"
              onPress={startSample}
              aria-label={`Record sample ${sampleCount + 1} of ${targetCount}`}
              style={styles.recordBtn}
            >
              <Mic size={18} />
              Record Sample {sampleCount + 1}
            </Button>
            <p style={styles.stageHint}>Click, then read the sentence above</p>
          </div>
        )}

        {phase === "countdown" && (
          <div style={styles.centerCol}>
            <span style={styles.countdownNumber}>{countdownValue}</span>
            <p style={styles.stageHint}>Get ready...</p>
          </div>
        )}

        {phase === "recording" && (
          <div style={styles.centerCol}>
            <AudioWaves state="recording" audioLevel={audioLevel} size="sm" />
            <p style={styles.speakPrompt}>
              "{prompts[sampleCount % prompts.length]}"
            </p>
          </div>
        )}

        {phase === "review" && (
          <div style={styles.centerCol}>
            <div style={styles.reviewIcon}>
              <Mic size={22} color="var(--color-accent)" />
            </div>
            <p style={styles.stageHint}>Sample recorded. Sound good?</p>
            <div style={styles.reviewActions}>
              <Button variant="outline" onPress={discardSample}>
                <X size={14} /> Discard
              </Button>
              <Button variant="primary" onPress={submitSample}>
                <Check size={14} /> Submit
              </Button>
            </div>
          </div>
        )}

        {phase === "submitting" && (
          <div style={styles.centerCol}>
            <div style={styles.spinner} />
            <p style={styles.stageHint}>Processing...</p>
          </div>
        )}

        {phase === "error" && (
          <div style={styles.centerCol}>
            {lastTranscript && (
              <p style={styles.feedback}>
                Heard: "<span style={styles.feedbackText}>{lastTranscript}</span>"
              </p>
            )}
            <p style={styles.errorText}>{errorMsg}</p>
            <Button variant="outline" onPress={startSample}>
              Try Again
            </Button>
          </div>
        )}

        {phase === "complete" && (
          <div style={styles.centerCol}>
            <div style={styles.checkCircle}>
              <Check size={28} color="var(--color-success)" />
            </div>
            <p style={styles.successText}>Calibration complete!</p>
            <div style={styles.variantList}>
              {variants.map((v, i) => (
                <span key={i} style={styles.variantChip}>{v}</span>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Footer actions */}
      <div style={styles.footer}>
        {phase !== "recording" && phase !== "countdown" && phase !== "submitting" && phase !== "review" && (
          <>
            <Button
              variant="outline"
              onPress={handleReset}
              isDisabled={sampleCount === 0 && phase !== "complete"}
            >
              <RotateCcw size={14} /> Start Over
            </Button>

            <div style={styles.footerRight}>
              <Button variant="outline" onPress={onCancel}>
                Cancel
              </Button>
              <Button
                variant={phase === "complete" ? "primary" : "outline"}
                onPress={onComplete}
                isDisabled={sampleCount < MIN_SAMPLES_FOR_DONE && phase !== "complete"}
              >
                {phase === "complete" ? "Done" : `Done (${sampleCount}/${MIN_SAMPLES_FOR_DONE} min)`}
              </Button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// ── Styles ──────────────────────────────────────────────────────

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-4)",
    padding: "var(--space-4)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-lg)",
    background: "var(--color-bg)",
  },
  header: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "var(--space-2)",
  },
  title: {
    margin: 0,
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "var(--text-base)",
    color: "var(--color-text)",
  },
  phrase: {
    color: "var(--color-accent)",
  },
  dots: {
    display: "flex",
    gap: "6px",
    alignItems: "center",
  },
  dot: {
    width: "10px",
    height: "10px",
    borderRadius: "50%",
    transition: "background 0.3s ease",
  },
  hint: {
    margin: 0,
    fontSize: "var(--text-xs)",
    color: "var(--color-text-tertiary)",
  },
  stage: {
    display: "flex",
    justifyContent: "center",
    alignItems: "center",
    minHeight: "140px",
    padding: "var(--space-3) 0",
  },
  centerCol: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "var(--space-3)",
  },
  recordBtn: {
    gap: "var(--space-2)",
  },
  stageHint: {
    margin: 0,
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
  },
  countdownNumber: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "48px",
    color: "var(--color-accent)",
    lineHeight: 1,
    animation: "pulse 0.8s ease-in-out infinite",
  },
  promptSentence: {
    margin: 0,
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "16px",
    color: "var(--color-text)",
    textAlign: "center" as const,
    lineHeight: 1.4,
    maxWidth: "320px",
  },
  speakPrompt: {
    margin: 0,
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "16px",
    color: "var(--color-accent)",
    textAlign: "center" as const,
    maxWidth: "320px",
  },
  spinner: {
    width: "32px",
    height: "32px",
    border: "3px solid var(--color-border)",
    borderTopColor: "var(--color-accent)",
    borderRadius: "50%",
    animation: "spin 0.8s linear infinite",
  },
  feedback: {
    margin: 0,
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
  },
  feedbackText: {
    color: "var(--color-text)",
    fontWeight: 500,
  },
  errorText: {
    margin: 0,
    fontSize: "var(--text-sm)",
    color: "var(--color-destructive)",
    textAlign: "center",
    maxWidth: "320px",
  },
  checkCircle: {
    width: "56px",
    height: "56px",
    borderRadius: "50%",
    background: "var(--color-success-soft, rgba(48,164,108,0.12))",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
  },
  successText: {
    margin: 0,
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "var(--text-base)",
    color: "var(--color-success)",
  },
  variantList: {
    display: "flex",
    flexWrap: "wrap",
    gap: "6px",
    justifyContent: "center",
  },
  variantChip: {
    padding: "2px 8px",
    fontSize: "var(--text-xs)",
    fontFamily: "var(--font-mono)",
    background: "var(--color-accent-soft)",
    color: "var(--color-accent)",
    borderRadius: "var(--radius-sm)",
  },
  footer: {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    gap: "var(--space-2)",
    minHeight: "36px",
  },
  footerRight: {
    display: "flex",
    gap: "var(--space-2)",
  },
  reviewIcon: {
    width: "48px",
    height: "48px",
    borderRadius: "50%",
    background: "var(--color-accent-soft)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
  },
  reviewActions: {
    display: "flex",
    gap: "var(--space-3)",
  },
};
