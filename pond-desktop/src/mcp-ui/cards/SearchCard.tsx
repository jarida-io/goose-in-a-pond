import { ExternalLink, Globe, Loader, MapPin } from "lucide-react";
import type { WebSearchHit, WebSearchPlace } from "../../api/types";
import { registerMcpCard, type McpCardProps } from "../registry";

const SHOWN = { compact: 3, normal: 5, large: 8 } as const;
// The pond sends at most five, so a roomier card has nothing more to show.
const PLACES_SHOWN = { compact: 3, normal: 5, large: 5 } as const;

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

interface Place {
  name: string;
  url: string | null;
  domain: string;
  snippet: string;
}

/** A place without a name has nothing to show, and an empty address is a place with no link. */
function readPlaces(raw: unknown): Place[] {
  if (!Array.isArray(raw)) return [];
  const places: Place[] = [];
  for (const entry of raw as Partial<WebSearchPlace>[]) {
    if (typeof entry !== "object" || entry === null) continue;
    const name = typeof entry.name === "string" ? entry.name.trim() : "";
    if (!name) continue;
    const url = webUrl(entry.url);
    places.push({
      name,
      url,
      domain: domainOf(url),
      snippet: typeof entry.snippet === "string" ? entry.snippet : "",
    });
  }
  return places;
}

function Title({ url, text }: { url: string | null; text: string }) {
  if (!url) return <span className="ui-search__title ui-search__title--plain">{text}</span>;
  return (
    <a
      href={url}
      className="ui-search__title"
      target="_blank"
      rel="noopener noreferrer"
      onClick={(e) => e.stopPropagation()}
    >
      <span>{text}</span>
      <ExternalLink size={10} aria-hidden />
      <span className="ui-search__sr">(opens in your browser)</span>
    </a>
  );
}

function SearchCard({ data, variant }: McpCardProps) {
  const query = typeof data.query === "string" && data.query ? data.query : undefined;
  const hits = readHits(data.results);
  const places = readPlaces(data.places);

  if (!query && hits.length === 0 && places.length === 0) {
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
  const shownPlaces = places.slice(0, PLACES_SHOWN[variant ?? "normal"]);

  const pages =
    shown.length === 0 ? (
      shownPlaces.length === 0 && (
        <p className="ui-search__none">No pages came back for this search.</p>
      )
    ) : (
      <ol className="ui-search__list" role="list">
        {shown.map((hit, i) => (
          <li key={`${hit.url ?? hit.title}-${i}`} className="ui-search__hit">
            <Title url={hit.url} text={hit.title} />
            {hit.domain && hit.domain !== hit.title && (
              <span className="ui-search__domain">{hit.domain}</span>
            )}
            {hit.snippet && <p className="ui-search__snippet">{hit.snippet}</p>}
          </li>
        ))}
      </ol>
    );

  return (
    <div className="ui-search">
      <div className="ui-search__header">
        <Globe size={13} className="ui-search__icon" aria-hidden />
        <span className="ui-search__query" title={query}>
          {query ?? "Web search"}
        </span>
      </div>

      {shownPlaces.length > 0 && (
        <div className="ui-search__group">
          <p className="ui-search__label">
            <MapPin size={11} aria-hidden />
            Nearby places
          </p>
          <ul className="ui-search__list" role="list" aria-label="Nearby places">
            {shownPlaces.map((place, i) => (
              <li key={`${place.url ?? place.name}-${i}`} className="ui-search__hit">
                <Title url={place.url} text={place.name} />
                {place.domain && place.domain !== place.name && (
                  <span className="ui-search__domain">{place.domain}</span>
                )}
                {place.snippet && <p className="ui-search__snippet">{place.snippet}</p>}
              </li>
            ))}
          </ul>
        </div>
      )}

      {shownPlaces.length > 0 && shown.length > 0 ? (
        <div className="ui-search__group">
          <p className="ui-search__label">
            <Globe size={11} aria-hidden />
            Pages
          </p>
          {pages}
        </div>
      ) : (
        pages
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
    places: [
      {
        name: "Example Town Library",
        url: "https://library.example.org/visit",
        snippet: "day: Saturday; hours: 9 am - 1 pm",
      },
    ],
  },
});

export { SearchCard };
