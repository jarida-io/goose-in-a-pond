import { ExternalLink, Globe, Loader } from "lucide-react";
import type { WebSearchHit } from "../../api/types";
import { registerMcpCard, type McpCardProps } from "../registry";

const SHOWN = { compact: 3, normal: 5, large: 8 } as const;

/** Only a web address is made a link; a page's own address never reaches `javascript:` or a file. */
function webUrl(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  try {
    const { protocol } = new URL(raw);
    return protocol === "https:" || protocol === "http:" ? raw : null;
  } catch {
    return null;
  }
}

function domainOf(url: string | null): string {
  if (!url) return "";
  return new URL(url).hostname.replace(/^www\./, "");
}

interface Hit {
  title: string;
  url: string | null;
  domain: string;
  snippet: string;
}

/** The server's result, tolerating a partial one: it renders what it has rather than blanking. */
function readHits(raw: unknown): Hit[] {
  if (!Array.isArray(raw)) return [];
  const hits: Hit[] = [];
  for (const entry of raw as Partial<WebSearchHit>[]) {
    if (typeof entry !== "object" || entry === null) continue;
    const url = webUrl(entry.url);
    const domain =
      typeof entry.domain === "string" && entry.domain ? entry.domain : domainOf(url);
    const title = typeof entry.title === "string" && entry.title ? entry.title : domain;
    if (!title) continue;
    hits.push({
      title,
      url,
      domain,
      snippet: typeof entry.snippet === "string" ? entry.snippet : "",
    });
  }
  return hits;
}

function SearchCard({ data, variant }: McpCardProps) {
  const query = typeof data.query === "string" && data.query ? data.query : undefined;
  const hits = readHits(data.results);

  if (!query && hits.length === 0) {
    // A finished call with no card of its own (a miss, a refused key) carries only text.
    const text = typeof data.result === "string" ? data.result.trim() : "";
    if (text) {
      return (
        <div className="ui-search">
          <div className="ui-search__header">
            <Globe size={13} className="ui-search__icon" aria-hidden />
            <span className="ui-search__query">Web search</span>
          </div>
          <p className="ui-search__text">{text}</p>
        </div>
      );
    }
    return (
      <div className="ui-search ui-search--loading" role="status">
        <Loader size={14} className="ui-search__spinner" aria-hidden />
        <span className="ui-search__working">Searching the web…</span>
      </div>
    );
  }

  const shown = hits.slice(0, SHOWN[variant ?? "normal"]);

  return (
    <div className="ui-search">
      <div className="ui-search__header">
        <Globe size={13} className="ui-search__icon" aria-hidden />
        <span className="ui-search__query" title={query}>
          {query ?? "Web search"}
        </span>
      </div>

      {shown.length === 0 ? (
        <p className="ui-search__none">No pages came back for this search.</p>
      ) : (
        <ol className="ui-search__list" role="list">
          {shown.map((hit, i) => (
            <li key={`${hit.url ?? hit.title}-${i}`} className="ui-search__hit">
              {hit.url ? (
                <a
                  href={hit.url}
                  className="ui-search__title"
                  target="_blank"
                  rel="noopener noreferrer"
                  onClick={(e) => e.stopPropagation()}
                >
                  <span>{hit.title}</span>
                  <ExternalLink size={10} aria-hidden />
                  <span className="ui-search__sr">(opens in your browser)</span>
                </a>
              ) : (
                <span className="ui-search__title ui-search__title--plain">{hit.title}</span>
              )}
              {hit.domain && hit.domain !== hit.title && (
                <span className="ui-search__domain">{hit.domain}</span>
              )}
              {hit.snippet && <p className="ui-search__snippet">{hit.snippet}</p>}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

registerMcpCard({
  key: "search",
  label: "Web search",
  icon: "Globe",
  toolPattern: /search_web/,
  component: SearchCard,
  mockTool: "giap-knowledge__search_web",
  mockData: {
    query: "when does the town library open on saturdays",
    results: [
      {
        title: "Opening hours - Example Town Library",
        url: "https://library.example.org/visit/hours",
        domain: "library.example.org",
        snippet: "Saturday 9:00 to 13:00. Closed on public holidays.",
      },
      {
        title: "Visiting the library",
        url: "https://www.example.com/council/library",
        domain: "example.com",
        snippet: "The reading room and the lending desk keep the same hours on weekends.",
      },
    ],
  },
});

export { SearchCard };
