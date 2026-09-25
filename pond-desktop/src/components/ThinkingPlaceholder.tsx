import { useEffect, useRef, useState } from "react";

const QUIPS = [
  "Ruffling through possibilities…",
  "Wading into the knowledge pool…",
  "Preening the context window…",
  "Migrating toward the right answer…",
  "Consulting the pond elders…",
  "Hatching a response…",
  "Skimming the surface of understanding…",
  "Molting old assumptions…",
  "Paddling upstream through the data…",
  "Flocking toward clarity…",
  "Squinting at the training data…",
  "Assembling thoughts, feather by feather…",
  "Cross-referencing the wetlands…",
  "Untangling the context…",
  "Swimming through token space…",
  "Surveying the flock of ideas…",
  "Translating your intent…",
  "Honking at the knowledge graph…",
  "Marshalling the evidence…",
  "Locating the right synapse…",
];

const INTERVAL_MS = 2800;
const FADE_MS = 300;

interface Props {
  /** Optional status text (e.g. "Using tool: get_weather"). Shown above the quip. */
  status?: string;
  /** When true, uses a minimal single-line layout without the status line. */
  compact?: boolean;
}

export function ThinkingPlaceholder({ status, compact = false }: Props) {
  // Pick a random start index so consecutive requests don't start at the same quip.
  const [index, setIndex] = useState(() => Math.floor(Math.random() * QUIPS.length));
  const [visible, setVisible] = useState(true);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const fadeRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    timerRef.current = setInterval(() => {
      setVisible(false);
      fadeRef.current = setTimeout(() => {
        setIndex((i) => (i + 1) % QUIPS.length);
        setVisible(true);
      }, FADE_MS);
    }, INTERVAL_MS);

    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
      if (fadeRef.current) clearTimeout(fadeRef.current);
    };
  }, []);

  const quip = QUIPS[index];

  if (compact) {
    return (
      <span
        style={{
          color: "var(--color-text-tertiary)",
          fontStyle: "italic",
          fontSize: "var(--text-sm)",
          opacity: visible ? 1 : 0,
          transition: `opacity ${FADE_MS}ms ease`,
        }}
      >
        {quip}
      </span>
    );
  }

  return (
    <span style={styles.root}>
      {status && <span style={styles.status}>{status}</span>}
      <span
        style={{
          ...styles.quip,
          opacity: visible ? 1 : 0,
          transition: `opacity ${FADE_MS}ms ease`,
        }}
      >
        {quip}
      </span>
    </span>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  status: {
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
    fontStyle: "italic",
  },
  quip: {
    color: "var(--color-text-tertiary)",
    fontStyle: "italic",
    fontSize: "var(--text-sm)",
  },
};
