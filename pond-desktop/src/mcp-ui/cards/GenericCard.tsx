import type { McpCardProps } from "../registry";

export function GenericCard({ data }: McpCardProps) {
  const text = typeof data === "string"
    ? data
    : JSON.stringify(data ?? {}, null, 2);

  return (
    <div className="ui-card ui-generic">
      <pre className="ui-generic__pre">{text}</pre>
    </div>
  );
}

// Not auto-registered: Canvas and ContextCard use it explicitly when no registered renderer matches.
