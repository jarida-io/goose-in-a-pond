import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import { statementAt } from "./voiceCatalogue";

/** Fixed preview line, in step with `PREVIEW_SENTENCE` in `pond-adapters-kokoro`. */
export const PREVIEW_SENTENCE =
  "Hello, I'm Jarida. I live here on your shelf, I think on my own, " +
  "and nothing you say to me leaves this room.";

export type PreviewState = "idle" | "loading" | "playing" | "error";

interface UseVoicePreview {
  /** Speak the next statement. Pass text to override the rotation. */
  play: (text?: string) => Promise<void>;
  /** Stop immediately and return to idle. */
  stop: () => void;
  state: PreviewState;
  error: string | null;
  /** True while either fetching or sounding — the button's disabled/spinner cue. */
  busy: boolean;
  /** The statement currently being spoken, or last spoken. */
  statement: string | null;
  /** Live 0–1 amplitude read from the decoded waveform, so the orb moves with this voice. */
  level: number;
}

function rms(samples: Float32Array, from: number, to: number): number {
  let sum = 0;
  const end = Math.min(to, samples.length);
  for (let i = from; i < end; i++) sum += samples[i] * samples[i];
  const n = Math.max(1, end - from);
  return Math.sqrt(sum / n);
}

/**
 * Speak a sample through the server's TTS with the saved settings (screens persist on change).
 * Each call supersedes the last, so a dragged pace slider doesn't queue stale samples.
 */
export function useVoicePreview(): UseVoicePreview {
  const [state, setState] = useState<PreviewState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [statement, setStatement] = useState<string | null>(null);
  const [level, setLevel] = useState(0);

  const audioRef = useRef<HTMLAudioElement | null>(null);
  const urlRef = useRef<string | null>(null);
  const rafRef = useRef<number | null>(null);
  /** Per-window RMS of the clip being played, and its window size in seconds. */
  const envelopeRef = useRef<{ windows: Float32Array; windowSecs: number } | null>(null);
  /** Bumped on every play/stop; a request whose generation is stale is dropped. */
  const generation = useRef(0);
  /** How many previews have played — drives which statement comes next. */
  const playCount = useRef(0);
  const mounted = useRef(true);

  const stopMeter = useCallback(() => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    envelopeRef.current = null;
  }, []);

  const releaseAudio = useCallback(() => {
    stopMeter();
    if (audioRef.current) {
      audioRef.current.pause();
      audioRef.current.src = "";
      audioRef.current = null;
    }
    if (urlRef.current) {
      URL.revokeObjectURL(urlRef.current);
      urlRef.current = null;
    }
  }, [stopMeter]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      // Leaving the screen must not leave a voice talking to an empty room.
      mounted.current = false;
      generation.current += 1;
      releaseAudio();
    };
  }, [releaseAudio]);

  const stop = useCallback(() => {
    generation.current += 1;
    releaseAudio();
    if (mounted.current) {
      setState("idle");
      setError(null);
      setLevel(0);
    }
  }, [releaseAudio]);

  /** Amplitude envelope parsed from the WAV (no AudioContext needed); null unless mono PCM16. */
  const buildEnvelope = useCallback((wav: ArrayBuffer) => {
    const view = new DataView(wav);
    if (wav.byteLength < 44) return null;
    const sampleRate = view.getUint32(24, true);
    const bitsPerSample = view.getUint16(34, true);
    if (bitsPerSample !== 16 || sampleRate === 0) return null;

    const pcmBytes = wav.byteLength - 44;
    const count = Math.floor(pcmBytes / 2);
    const samples = new Float32Array(count);
    for (let i = 0; i < count; i++) samples[i] = view.getInt16(44 + i * 2, true) / 32768;

    // ~30 ms: catches a syllable without the orb flickering.
    const windowSecs = 0.03;
    const per = Math.max(1, Math.round(sampleRate * windowSecs));
    const windows = new Float32Array(Math.ceil(count / per));
    for (let w = 0; w < windows.length; w++) windows[w] = rms(samples, w * per, (w + 1) * per);
    return { windows, windowSecs };
  }, []);

  const play = useCallback(
    async (text?: string) => {
      generation.current += 1;
      const mine = generation.current;
      releaseAudio();

      const line = text ?? statementAt(playCount.current);
      setStatement(line);
      setState("loading");
      setError(null);
      setLevel(0);

      try {
        const wav = await api.synthesizeSpeech(line);
        // Superseded while the request was in flight, or unmounted.
        if (!mounted.current || generation.current !== mine) return;

        envelopeRef.current = buildEnvelope(wav);
        const url = URL.createObjectURL(new Blob([wav], { type: "audio/wav" }));
        const audio = new Audio(url);
        urlRef.current = url;
        audioRef.current = audio;

        const finish = () => {
          if (mounted.current && generation.current === mine) {
            stopMeter();
            setLevel(0);
            setState("idle");
          }
        };
        audio.onended = finish;
        audio.onerror = () => {
          if (mounted.current && generation.current === mine) {
            stopMeter();
            setLevel(0);
            setState("error");
            setError("Could not play the sample.");
          }
        };

        await audio.play();
        if (!mounted.current || generation.current !== mine) return;
        playCount.current += 1;
        setState("playing");

        // Index by `currentTime`: a timer started at play() drifts once the tab is throttled.
        const tick = () => {
          if (!mounted.current || generation.current !== mine) return;
          const env = envelopeRef.current;
          const el = audioRef.current;
          if (env && el && !el.paused) {
            const idx = Math.floor(el.currentTime / env.windowSecs);
            const raw = env.windows[Math.min(idx, env.windows.length - 1)] ?? 0;
            // Speech RMS sits well under 1; x3 lifts it to the mic-level range the orb expects.
            setLevel(Math.min(1, raw * 3));
          }
          rafRef.current = requestAnimationFrame(tick);
        };
        rafRef.current = requestAnimationFrame(tick);
      } catch (e) {
        if (!mounted.current || generation.current !== mine) return;
        stopMeter();
        setLevel(0);
        setState("error");
        setError(
          e instanceof Error && e.message
            ? `Could not play a sample: ${e.message}`
            : "Could not play a sample.",
        );
      }
    },
    [releaseAudio, stopMeter, buildEnvelope],
  );

  return {
    play,
    stop,
    state,
    error,
    busy: state === "loading" || state === "playing",
    statement,
    level,
  };
}
