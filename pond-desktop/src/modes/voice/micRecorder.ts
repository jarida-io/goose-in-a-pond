// Wake-word calibration capture: a fixed window with no VAD, so nothing the user said is cut.

import { MIC_CONSTRAINTS, encodeWav, downsampleTo16k, calculateRms } from "./webAudioUtils";

/** How often to report a level, in ms. Matches the VAD frame rate. */
const LEVEL_INTERVAL_MS = 30;

const PROCESSOR_BUFFER = 4096;

export interface FixedRecording {
  /** 16 kHz mono WAV, ready for the calibration endpoint. */
  wav: ArrayBuffer;
  /** Loudest frame observed, so a caller can tell silence from speech. */
  peakRms: number;
}

export interface FixedRecorder {
  /** Resolves when the window closes; rejects if the mic could not be opened. */
  done: Promise<FixedRecording>;
  /** Stop early and discard. `done` rejects with an AbortError. */
  abort(): void;
}

/** Closes its own AudioContext on every exit, so a cancel can't leave the mic indicator lit. */
export function recordFixedDuration(
  durationMs: number,
  onLevel?: (rms: number) => void,
): FixedRecorder {
  let stop: ((reason?: Error) => void) | null = null;
  let aborted = false;

  const done = new Promise<FixedRecording>((resolve, reject) => {
    let ctx: AudioContext | null = null;
    let stream: MediaStream | null = null;
    let levelTimer: ReturnType<typeof setInterval> | null = null;
    let endTimer: ReturnType<typeof setTimeout> | null = null;
    let settled = false;

    const chunks: Float32Array[] = [];
    let peakRms = 0;

    const teardown = () => {
      if (levelTimer !== null) clearInterval(levelTimer);
      if (endTimer !== null) clearTimeout(endTimer);
      stream?.getTracks().forEach((t) => t.stop());
      void ctx?.close().catch(() => {});
    };

    stop = (reason?: Error) => {
      if (settled) return;
      settled = true;
      teardown();
      if (reason) {
        reject(reason);
        return;
      }
      if (ctx === null || chunks.length === 0) {
        resolve({ wav: encodeWav(new Float32Array(0), 16_000), peakRms });
        return;
      }
      const total = chunks.reduce((n, c) => n + c.length, 0);
      const all = new Float32Array(total);
      let at = 0;
      for (const c of chunks) {
        all.set(c, at);
        at += c.length;
      }
      resolve({ wav: encodeWav(downsampleTo16k(all, ctx.sampleRate), 16_000), peakRms });
    };

    navigator.mediaDevices
      .getUserMedia(MIC_CONSTRAINTS)
      .then((s) => {
        if (aborted) {
          s.getTracks().forEach((t) => t.stop());
          return;
        }
        stream = s;
        ctx = new AudioContext();
        const source = ctx.createMediaStreamSource(s);

        const analyser = ctx.createAnalyser();
        analyser.fftSize = 2048;
        source.connect(analyser);

        // Deprecated, but shared with every other capture path; move them all to AudioWorklet together.
        const processor = ctx.createScriptProcessor(PROCESSOR_BUFFER, 1, 1);
        processor.onaudioprocess = (e) => {
          chunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
        };
        source.connect(processor);
        processor.connect(ctx.destination);

        const frame = new Float32Array(analyser.fftSize);
        levelTimer = setInterval(() => {
          analyser.getFloatTimeDomainData(frame);
          const rms = calculateRms(frame);
          if (rms > peakRms) peakRms = rms;
          onLevel?.(rms);
        }, LEVEL_INTERVAL_MS);

        endTimer = setTimeout(() => stop?.(), durationMs);
      })
      .catch((err: Error) => {
        teardown();
        if (settled) return;
        settled = true;
        reject(err);
      });
  });

  return {
    done,
    abort() {
      aborted = true;
      stop?.(new DOMException("recording aborted", "AbortError"));
    },
  };
}

/** A message worth showing the user for a getUserMedia failure. */
export function micErrorMessage(err: unknown): string {
  const name = (err as { name?: string })?.name;
  if (name === "NotAllowedError") {
    return "Microphone access was denied. Allow it in System Settings > Privacy & Security > Microphone, then try again.";
  }
  if (name === "NotFoundError") {
    return "No microphone was found. Connect one and try again.";
  }
  if (name === "AbortError") return "Recording cancelled.";
  return `Microphone error: ${(err as Error)?.message ?? String(err)}`;
}
