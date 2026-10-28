import { Clapperboard, ExternalLink, Globe, Loader, MapPin, Newspaper } from "lucide-react";
import type { ReactNode } from "react";
import type { WebSearchHit, WebSearchNews, WebSearchPlace, WebSearchVideo } from "../../api/types";
import { registerMcpCard, type McpCardProps } from "../registry";
import { braveThumbnail, domainOf, text, webUrl } from "./thumbnails";

const SHOWN = { compact: 3, normal: 5, large: 8 } as const;
// The pond sends at most five, so a roomier card has nothing more to show.
const PLACES_SHOWN = { compact: 3, normal: 5, large: 5 } as const;
// The pond sends at most three of each.
const NEWS_SHOWN = { compact: 2, normal: 3, large: 3 } as const;
const VIDEOS_SHOWN = { compact: 2, normal: 3, large: 3 } as const;

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
  details: string[];
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
    const hours = text(entry.hours);
    places.push({
      name,
      url,
      domain: domainOf(url),
      snippet: typeof entry.snippet === "string" ? entry.snippet : "",
      details: [
        hours ? `Open ${hours}` : "",
        text(entry.address),
        text(entry.phone),
        text(entry.rating),
      ].filter(Boolean),
    });
  }
  return places;
}

interface NewsRow {
  title: string;
  url: string;
  meta: string;
  thumbnail: string | null;
  breaking: boolean;
}

function readNews(raw: unknown): NewsRow[] {
  if (!Array.isArray(raw)) return [];
  const rows: NewsRow[] = [];
  for (const entry of raw as Partial<WebSearchNews>[]) {
    if (typeof entry !== "object" || entry === null) continue;
    const url = webUrl(entry.url);
    const title = text(entry.title);
    if (!url || !title) continue;
    rows.push({
      title,
      url,
      meta: [text(entry.publisher) || domainOf(url), text(entry.date)].filter(Boolean).join(" · "),
      thumbnail: braveThumbnail(entry.thumbnail),
      breaking: entry.breaking === true,
    });
  }
  return rows;
}

interface VideoRow {
  title: string;
  url: string;
  meta: string;
  duration: string;
  thumbnail: string | null;
}

function readVideos(raw: unknown): VideoRow[] {
  if (!Array.isArray(raw)) return [];
  const rows: VideoRow[] = [];
  for (const entry of raw as Partial<WebSearchVideo>[]) {
    if (typeof entry !== "object" || entry === null) continue;
    const url = webUrl(entry.url);
    const title = text(entry.title);
    if (!url || !title) continue;
    rows.push({
      title,
      url,
      meta: [text(entry.creator), text(entry.domain) || domainOf(url), text(entry.date)]
        .filter(Boolean)
        .join(" · "),
      duration: text(entry.duration),
      thumbnail: braveThumbnail(entry.thumbnail),
    });
  }
  return rows;
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

/** A thumbnail beside the text that names it, so it carries no words of its own. */
function Thumb({ src, kind }: { src: string; kind: "news" | "video" }) {
  return (
    <img
      className={`ui-search__thumb ui-search__thumb--${kind}`}
      src={src}
      alt=""
      loading="lazy"
      referrerPolicy="no-referrer"
      draggable={false}
    />
  );
}

function Group({
  label,
  icon,
  labelled,
  children,
}: {
  label: string;
  icon: ReactNode;
  labelled: boolean;
  children: ReactNode;
}) {
  if (!labelled) return <>{children}</>;
  return (
    <div className="ui-search__group">
      <p className="ui-search__label">
        {icon}
        {label}
      </p>
      {children}
    </div>
  );
}

function SearchCard({ data, variant }: McpCardProps) {
  const query = typeof data.query === "string" && data.query ? data.query : undefined;
  const hits = readHits(data.results);
  const places = readPlaces(data.places);
  const news = readNews(data.news);
  const videos = readVideos(data.videos);

  if (!query && hits.length === 0 && places.length === 0 && news.length === 0) {
    // A finished call with no card of its own (a miss, a refused key) carries only text.
    const resultText = typeof data.result === "string" ? data.result.trim() : "";
    if (resultText) {
      return (
        <div className="ui-search">
          <div className="ui-search__header">
            <Globe size={13} className="ui-search__icon" aria-hidden />
            <span className="ui-search__query">Web search</span>
          </div>
          <p className="ui-search__text">{resultText}</p>
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

  const size = variant ?? "normal";
  const shown = hits.slice(0, SHOWN[size]);
  const shownPlaces = places.slice(0, PLACES_SHOWN[size]);
  const shownNews = news.slice(0, NEWS_SHOWN[size]);
  const shownVideos = videos.slice(0, VIDEOS_SHOWN[size]);
  // Labels say which group is which only when there is more than one to tell apart.
  const groups = [shown, shownPlaces, shownNews, shownVideos].filter((g) => g.length > 0).length;
  const labelled = groups > 1;

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
                {place.details.length > 0 ? (
                  <ul className="ui-search__details" role="list">
                    {place.details.map((detail) => (
                      <li key={detail}>{detail}</li>
                    ))}
                  </ul>
                ) : (
                  place.snippet && <p className="ui-search__snippet">{place.snippet}</p>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {shown.length > 0 ? (
        <Group label="Pages" icon={<Globe size={11} aria-hidden />} labelled={labelled}>
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
        </Group>
      ) : (
        shownPlaces.length === 0 &&
        shownNews.length === 0 && (
          <p className="ui-search__none">No pages came back for this search.</p>
        )
      )}

      {shownNews.length > 0 && (
        <Group label="News" icon={<Newspaper size={11} aria-hidden />} labelled={labelled}>
          <ul className="ui-search__list" role="list" aria-label="News">
            {shownNews.map((row, i) => (
              <li key={`${row.url}-${i}`} className="ui-search__media">
                {row.thumbnail && <Thumb src={row.thumbnail} kind="news" />}
                <div className="ui-search__hit">
                  <Title url={row.url} text={row.title} />
                  <span className="ui-search__domain">
                    {row.breaking && <span className="ui-search__breaking">Breaking</span>}
                    {row.meta}
                  </span>
                </div>
              </li>
            ))}
          </ul>
        </Group>
      )}

      {shownVideos.length > 0 && (
        <Group label="Videos" icon={<Clapperboard size={11} aria-hidden />} labelled={labelled}>
          <ul className="ui-search__list" role="list" aria-label="Videos">
            {shownVideos.map((row, i) => (
              <li key={`${row.url}-${i}`} className="ui-search__media">
                {row.thumbnail && (
                  <span className="ui-search__frame">
                    <Thumb src={row.thumbnail} kind="video" />
                    {row.duration && (
                      <span className="ui-search__duration" aria-hidden>
                        {row.duration}
                      </span>
                    )}
                  </span>
                )}
                <div className="ui-search__hit">
                  <Title url={row.url} text={row.title} />
                  <span className="ui-search__domain">
                    {[row.duration && !row.thumbnail ? row.duration : "", row.meta]
                      .filter(Boolean)
                      .join(" · ")}
                    {row.duration && row.thumbnail && (
                      <span className="ui-search__sr">, {row.duration} long</span>
                    )}
                  </span>
                </div>
              </li>
            ))}
          </ul>
        </Group>
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
        hours: "Saturday 09:00 to 13:00",
        phone: "+254 700 000 000",
      },
    ],
    news: [
      {
        title: "The library opens a new reading room",
        url: "https://news.example.org/library-reading-room",
        domain: "news.example.org",
        publisher: "Example News",
        date: "2026-10-26",
        thumbnail: null,
        breaking: false,
      },
    ],
  },
});

export { SearchCard };
