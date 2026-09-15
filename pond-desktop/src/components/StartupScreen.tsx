import { useState, useEffect, useCallback } from "react";
import { invoke, isDesktopShell } from "../shell";
import { Button } from "@heroui/react";
import { defaultServerUrl } from "../api/PondApiClient";
import { Logo } from "./Logo";

interface Props {
  onReady: () => void;
}

type StartupPhase = "starting" | "connecting" | "ready" | "error";

// 120 polls × 500 ms = 60 s. Cold start with face recognition + Whisper +
// TTS warming the GGUF cache can easily take 30-45 s the very first time
// (model auto-downloads, ONNX runtime init, sqlite migrations). The old
// 30 s budget timed out before the parent server was ready, leaving the
// WebView blank ("nothing shows; close-and-reopen fixes it" — by then the
// parent server had finished booting in the background).
const MAX_POLLS = 120;
const POLL_INTERVAL_MS = 500;

export function StartupScreen({ onReady }: Props) {
  const [phase, setPhase] = useState<StartupPhase>("starting");
  const [error, setError] = useState<string | null>(null);
  const [dots, setDots] = useState(".");

  const tryStartup = useCallback(async () => {
    setPhase("starting");
    setError(null);

    const inShell = isDesktopShell();

    if (inShell) {
      // Deliberately NOT awaited. A cold start loading face recognition,
      // Whisper and TTS routinely takes over a minute, and this call does not
      // resolve until the server answers -- awaiting it held this screen blank
      // for the whole budget before the poll below even began. Kick it and
      // poll concurrently; the polling is what decides when we are ready.
      void invoke("ensure_server_running").catch((e: unknown) => {
        // May fail when no binary is found. We still poll, in case the user
        // has a server running by hand.
        console.warn("[GIAP] ensure_server_running error:", e);
      });
    }

    setPhase("connecting");

    if (!inShell) {
      // Browser dev mode: poll the health endpoint directly via fetch.
      // This lets Playwright and web browser testing work without Tauri.
      const serverUrl = defaultServerUrl();
      for (let i = 0; i < MAX_POLLS; i++) {
        await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
        try {
          const res = await fetch(`${serverUrl}/api/v1/health`);
          if (res.ok) {
            setPhase("ready");
            await new Promise((resolve) => setTimeout(resolve, 400));
            onReady();
            return;
          }
        } catch {
          // Server not yet available — keep polling
        }
      }
      setPhase("error");
      setError(
        "Could not reach pond-server at " +
          serverUrl +
          ". Make sure it is running.",
      );
      return;
    }

    // Poll health endpoint via Tauri IPC
    for (let i = 0; i < MAX_POLLS; i++) {
      await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));

      try {
        const healthy = await invoke("server_health");
        if (healthy) {
          setPhase("ready");
          // Small delay so "Ready" is visible briefly
          await new Promise((resolve) => setTimeout(resolve, 400));
          onReady();
          return;
        }
      } catch {
        // Keep polling
      }
    }

    setPhase("error");
    setError("Could not connect to pond-server within 60 seconds.");
  }, [onReady]);

  // Animated dots
  useEffect(() => {
    const id = setInterval(() => {
      setDots((d) => (d.length >= 3 ? "." : d + "."));
    }, 400);
    return () => clearInterval(id);
  }, []);

  // Start on mount
  useEffect(() => {
    tryStartup();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const statusLabel =
    phase === "starting"
      ? `Starting pond-server${dots}`
      : phase === "connecting"
        ? `Connecting${dots}`
        : phase === "ready"
          ? "Ready"
          : "Failed to start";

  return (
    <div style={styles.root}>
      <div style={styles.card}>
        <Logo size={96} style={styles.logoMark} />

        <h1 style={styles.title}>Goose In A Pond</h1>
        <p style={styles.subtitle}>by Jarida Open Source</p>

        {/* Progress bar */}
        <div style={styles.progressTrack}>
          {phase !== "error" && phase !== "ready" && (
            <div style={styles.progressBar} />
          )}
          {phase === "ready" && (
            <div style={{ ...styles.progressBar, ...styles.progressFull }} />
          )}
        </div>

        <p style={styles.statusText}>{statusLabel}</p>

        {phase === "error" && (
          <div style={styles.errorBox}>
            <p style={styles.errorText}>{error}</p>
            <p style={styles.errorHint}>
              Make sure pond-server is installed or run{" "}
              <code style={styles.code}>cargo run -p pond-server -- serve</code>{" "}
              in a terminal.
            </p>
            <Button variant="primary" onPress={tryStartup}>
              Retry
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    width: "100%",
    height: "100%",
    background: "var(--color-bg)",
    fontFamily: "var(--font-body)",
  },
  card: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "12px",
    width: "340px",
  },
  logoMark: {
    width: "96px",
    height: "96px",
    objectFit: "contain",
    marginBottom: "4px",
  },
  title: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-xl)",
    color: "var(--color-text)",
    margin: 0,
  },
  subtitle: {
    fontSize: "var(--text-sm)",
    color: "var(--color-text-tertiary)",
    margin: 0,
    letterSpacing: "0.02em",
  },
  progressTrack: {
    width: "100%",
    height: "3px",
    background: "var(--color-border)",
    borderRadius: "var(--radius-pill)",
    overflow: "hidden",
    marginTop: "8px",
  },
  progressBar: {
    height: "100%",
    borderRadius: "var(--radius-pill)",
    background: "var(--color-accent)",
    animation: "progressSlide 1.2s ease infinite",
    width: "60%",
  },
  progressFull: {
    width: "100%",
    animation: "none",
  },
  statusText: {
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
    margin: 0,
    minHeight: "18px",
  },
  errorBox: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "10px",
    marginTop: "8px",
    padding: "16px",
    background: "var(--color-destructive-soft)",
    border: "1px solid var(--color-destructive)",
    borderRadius: "var(--radius-lg)",
    width: "100%",
  },
  errorText: {
    fontSize: "var(--text-base)",
    color: "var(--color-destructive)",
    textAlign: "center",
    margin: 0,
  },
  errorHint: {
    fontSize: "var(--text-xs)",
    color: "var(--color-text-secondary)",
    textAlign: "center",
    margin: 0,
    lineHeight: "1.5",
  },
  code: {
    fontFamily: "var(--font-mono)",
    fontSize: "var(--text-xs)",
    background: "var(--color-border)",
    padding: "1px 4px",
    borderRadius: "var(--radius-xs)",
  },
  retryBtn: {
    background: "var(--color-accent)",
    color: "var(--color-text-on-accent)",
    fontFamily: "var(--font-body)",
    fontWeight: 600,
    fontSize: "var(--text-base)",
    border: "none",
    borderRadius: "var(--radius-md)",
    padding: "8px 20px",
    cursor: "pointer",
    height: "36px",
    display: "inline-flex",
    alignItems: "center",
  },
};
