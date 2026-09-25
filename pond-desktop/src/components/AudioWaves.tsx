import { useEffect, useRef } from "react";
import type { VoiceState } from "../state/reducer";
import { ORB_STATE_COLORS } from "../lib/colors";

// ── Config ─────────────────────────────────────────────────────
const CONFIG = {
  sm: { height: 40, bars: 5,  gap: 3, radius: 2 },
  lg: { height: 72, bars: 9,  gap: 4, radius: 3 },
} as const;

const MIN_H_RATIO = 0.08; // minimum bar height as fraction of canvas height
const MAX_H_RATIO = 0.88; // maximum bar height as fraction of canvas height

// Per-state phase increment per frame
const PHASE_STEP: Record<VoiceState, number> = {
  idle:      0.018,
  wait:      0.012,  // slower than idle — barely-breathing passive state
  recording: 0.05,
  thinking:  0.065,
  speaking:  0.042,
  error:     0,
};

// ── Props ──────────────────────────────────────────────────────

interface AudioWavesProps {
  /** Current voice pipeline state — drives color + animation style */
  state: VoiceState;
  /** RMS audio level 0–1 (from Tauri audio-level events or 0 when not recording) */
  audioLevel: number;
  /** Visual size — sm fits in cards; lg fills the voice mode stage */
  size?: "sm" | "lg";
  style?: React.CSSProperties;
}

// ── Component ──────────────────────────────────────────────────

export function AudioWaves({ state, audioLevel, size = "lg", style }: AudioWavesProps) {
  const canvasRef  = useRef<HTMLCanvasElement>(null);
  const phaseRef   = useRef(0);
  const rafRef     = useRef<number>(0);
  const levelRef   = useRef(audioLevel);

  // Keep level ref in sync without re-triggering RAF
  useEffect(() => { levelRef.current = audioLevel; }, [audioLevel]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const cfg = CONFIG[size];
    const dpr = window.devicePixelRatio || 1;

    function resize() {
      if (!canvas) return;
      const w = canvas.offsetWidth;
      canvas.width  = w * dpr;
      canvas.height = cfg.height * dpr;
    }

    const ro = new ResizeObserver(resize);
    ro.observe(canvas);
    resize();

    function frame() {
      if (!canvas) return;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      const W = canvas.width;
      const H = canvas.height;
      const n = cfg.bars;
      const gap = cfg.gap * dpr;
      const radius = cfg.radius * dpr;
      const minH = H * MIN_H_RATIO;
      const maxH = H * MAX_H_RATIO;
      const barW = (W - gap * (n + 1)) / n;

      ctx.clearRect(0, 0, W, H);

      const color = ORB_STATE_COLORS[state];
      ctx.fillStyle = color;

      const phase = phaseRef.current;
      const level = levelRef.current;

      for (let i = 0; i < n; i++) {
        const x = gap + i * (barW + gap);
        let heightFraction: number;

        const norm = (i / (n - 1)) * 2 - 1; // -1 at leftmost, +1 at rightmost
        const centerWeight = 1 - Math.abs(norm) * 0.4; // center bars are taller

        switch (state) {
          case "wait": {
            // Near-static breath — much slower and shallower than idle
            const breath = Math.sin(phase + (i / n) * Math.PI) * 0.5 + 0.5;
            heightFraction = Math.min(0.40, (0.08 + breath * 0.18) * centerWeight);
            break;
          }
          case "idle": {
            // Gentle breathing: all bars follow a slow sine, phase offset by position
            const breath = Math.sin(phase + (i / n) * Math.PI) * 0.5 + 0.5;
            heightFraction = (0.12 + breath * 0.28) * centerWeight;
            break;
          }
          case "recording": {
            // Bars react to mic level; add a subtle wave so they're never static
            const wave = Math.sin(phase * 2 + i * 0.9) * 0.15 + 0.15;
            heightFraction = (wave + level * 0.72) * centerWeight;
            break;
          }
          case "thinking": {
            // Traveling sine wave — phase offset shifts across bars
            const travel = Math.sin(phase + i * (Math.PI * 2 / n)) * 0.5 + 0.5;
            heightFraction = (0.15 + travel * 0.65) * centerWeight;
            break;
          }
          case "speaking": {
            // Smooth pulsing wave with medium amplitude
            const pulse = Math.sin(phase + i * 0.7) * 0.5 + 0.5;
            heightFraction = (0.20 + pulse * 0.55) * centerWeight;
            break;
          }
          case "error": {
            // Flat bars — just the minimum to show something is there
            heightFraction = (i % 2 === 0 ? 0.18 : 0.10) * centerWeight;
            break;
          }
        }

        const barH = Math.max(minH, Math.min(maxH, heightFraction * H));
        const y = (H - barH) / 2;

        // Some environments lack ctx.roundRect.
        if (ctx.roundRect) {
          ctx.beginPath();
          ctx.roundRect(x, y, barW, barH, radius);
          ctx.fill();
        } else {
          ctx.fillRect(x, y, barW, barH);
        }
      }

      phaseRef.current += PHASE_STEP[state];

      rafRef.current = requestAnimationFrame(frame);
    }

    rafRef.current = requestAnimationFrame(frame);

    return () => {
      cancelAnimationFrame(rafRef.current);
      ro.disconnect();
    };
  }, [state, size]);

  const cfg = CONFIG[size];

  return (
    <canvas
      ref={canvasRef}
      style={{
        display: "block",
        width: "100%",
        height: `${cfg.height}px`,
        ...style,
      }}
      aria-hidden="true"
    />
  );
}
