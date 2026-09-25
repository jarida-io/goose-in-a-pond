import { Sigma, ExternalLink, Loader } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

// Mirrors the hint from `crates/pond-mcp-server/src/wolfram.rs`; all optional so a partial one renders.
interface Pod {
  title?: string;
  text?: string;
}

interface Suggestion {
  id?: string;
  label?: string;
  kind?: string;
  verb?: string;
}

function WolframCard({ data, onAction, variant }: McpCardProps) {
  const isCompact = variant === "compact";
  const query = data.query as string | undefined;
  const primary = data.primary as string | undefined;
  const primaryTitle = data.primary_title as string | undefined;
  const pods = (data.pods ?? []) as Pod[];
  const explore = (data.explore ?? []) as Suggestion[];
  const sourceUrl = data.source_url as string | undefined;

  // Nothing yet, but the tool call is on screen: show progress, not an empty box.
  if (!primary && pods.length === 0 && explore.length === 0) {
    return (
      <div className="ui-wolfram ui-wolfram--loading">
        <Loader size={14} className="ui-wolfram__spinner" aria-hidden />
        <span className="ui-wolfram__working">Working it out…</span>
      </div>
    );
  }

  const visiblePods = isCompact ? pods.slice(0, 2) : pods.slice(0, 5);

  return (
    <div className="ui-wolfram">
      <div className="ui-wolfram__header">
        <Sigma size={13} className="ui-wolfram__icon" aria-hidden />
        {query && <span className="ui-wolfram__query">{query}</span>}
      </div>

      {primary && (
        <div className="ui-wolfram__answer">
          {primaryTitle && primaryTitle.toLowerCase() !== "result" && (
            <span className="ui-wolfram__answer-label">{primaryTitle}</span>
          )}
          <span className="ui-wolfram__answer-value">{primary}</span>
        </div>
      )}

      {visiblePods.length > 0 && (
        <dl className="ui-wolfram__pods">
          {visiblePods.map((pod, i) => (
            <div key={i} className="ui-wolfram__pod">
              <dt className="ui-wolfram__pod-title">{pod.title}</dt>
              <dd className="ui-wolfram__pod-text">{pod.text}</dd>
            </div>
          ))}
        </dl>
      )}

      {explore.length > 0 && (
        <div className="ui-wolfram__explore">
          <span className="ui-wolfram__explore-label">
            {explore.length === 1 ? "Also available" : "Also available"}
          </span>
          <div className="ui-wolfram__chips" role="group" aria-label="Other readings of this question">
            {explore.map((s, i) => (
              <SuggestionChip key={s.id ?? i} suggestion={s} query={query} onAction={onAction} />
            ))}
          </div>
        </div>
      )}

      {sourceUrl && !isCompact && (
        <a
          href={sourceUrl}
          className="ui-wolfram__source"
          target="_blank"
          rel="noopener noreferrer"
          onClick={(e) => e.stopPropagation()}
        >
          <ExternalLink size={10} aria-hidden />
          <span>Wolfram|Alpha</span>
        </a>
      )}
    </div>
  );
}

/** Follow-ups go through `onAction`, never a direct tool call; see `McpCardProps.onAction`. */
function SuggestionChip({
  suggestion,
  query,
  onAction,
}: {
  suggestion: Suggestion;
  query?: string;
  onAction?: (prompt: string) => void;
}) {
  const label = suggestion.label ?? suggestion.id ?? "";
  if (!label) return null;

  const verb = suggestion.verb ?? "open";
  const title = `${verb} ${label}`;

  if (!onAction || !suggestion.id) {
    return (
      <span className="ui-wolfram__chip ui-wolfram__chip--static" title={title}>
        {label}
      </span>
    );
  }

  // Nothing resolves an id, so the text must stand alone as something a person would say.
  const prompt = query
    ? `For "${query}", ${verb} ${label}.`
    : `${verb.charAt(0).toUpperCase()}${verb.slice(1)} ${label}.`;

  return (
    <button
      type="button"
      className="ui-wolfram__chip"
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onAction(prompt);
      }}
    >
      {label}
    </button>
  );
}

registerMcpCard({
  key: "wolfram",
  label: "Computed",
  icon: "Sigma",
  toolPattern: /compute_answer|wolfram/,
  component: WolframCard,
  mockTool: "giap-knowledge__compute_answer",
  mockData: {
    query: "mercury",
    primary: "Mercury (planet)",
    primary_title: "Result",
    pods: [
      { title: "Orbital period", text: "87.97 days" },
      { title: "Mean radius", text: "2439.7 km" },
    ],
    explore: [
      { id: "w1", label: "a chemical element", kind: "assumption", verb: "interpret as" },
      { id: "w2", label: "a Roman god", kind: "assumption", verb: "interpret as" },
      { id: "w3", label: "Surface temperature", kind: "pod", verb: "show section" },
    ],
    source_url: "https://www.wolframalpha.com/input?i=mercury",
  },
});

export { WolframCard };
