import { ImageIcon, Loader } from "lucide-react";
import type { WebPicture } from "../../api/types";
import { registerMcpCard, type McpCardProps } from "../registry";
import { braveThumbnail, domainOf, text, webUrl } from "./thumbnails";

// The pond sends at most twelve.
const SHOWN = { compact: 6, normal: 9, large: 12 } as const;

interface Picture {
  title: string;
  url: string;
  domain: string;
  thumbnail: string;
}

/** A picture needs a proxied thumbnail and a page to open; one missing either is not shown. */
function readPictures(raw: unknown): Picture[] {
  if (!Array.isArray(raw)) return [];
  const pictures: Picture[] = [];
  for (const entry of raw as Partial<WebPicture>[]) {
    if (typeof entry !== "object" || entry === null) continue;
    const url = webUrl(entry.url);
    const thumbnail = braveThumbnail(entry.thumbnail);
    if (!url || !thumbnail) continue;
    pictures.push({
      title: text(entry.title),
      url,
      domain: text(entry.domain) || domainOf(url),
      thumbnail,
    });
  }
  return pictures;
}

function PicturesCard({ data, variant }: McpCardProps) {
  const query = text(data.query);
  const pictures = readPictures(data.pictures);

  if (pictures.length === 0) {
    // A finished call with no card of its own (none found, a refused key) carries only text.
    const resultText = text(data.result);
    if (resultText) {
      return (
        <div className="ui-search">
          <div className="ui-search__header">
            <ImageIcon size={13} className="ui-search__icon" aria-hidden />
            <span className="ui-search__query">Pictures</span>
          </div>
          <p className="ui-search__text">{resultText}</p>
        </div>
      );
    }
    return (
      <div className="ui-search ui-search--loading" role="status">
        <Loader size={14} className="ui-search__spinner" aria-hidden />
        <span className="ui-search__working">Finding pictures…</span>
      </div>
    );
  }

  const shown = pictures.slice(0, SHOWN[variant ?? "normal"]);
  return (
    <div className="ui-search">
      <div className="ui-search__header">
        <ImageIcon size={13} className="ui-search__icon" aria-hidden />
        <span className="ui-search__query" title={query || undefined}>
          {query || "Pictures"}
        </span>
      </div>
      <ul className="ui-pictures" role="list" aria-label={query ? `Pictures: ${query}` : "Pictures"}>
        {shown.map((picture, i) => {
          const name = picture.title || picture.domain;
          return (
            <li key={`${picture.url}-${i}`} className="ui-pictures__item">
              <a
                href={picture.url}
                className="ui-pictures__link"
                target="_blank"
                rel="noopener noreferrer"
                title={picture.domain ? `${name} (${picture.domain})` : name}
                onClick={(e) => e.stopPropagation()}
              >
                <img
                  className="ui-pictures__img"
                  src={picture.thumbnail}
                  alt={name}
                  loading="lazy"
                  referrerPolicy="no-referrer"
                  draggable={false}
                />
                <span className="ui-search__sr">
                  {picture.domain ? `, from ${picture.domain}` : ""} (opens in your browser)
                </span>
              </a>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

registerMcpCard({
  key: "pictures",
  label: "Pictures",
  icon: "Image",
  toolPattern: /show_pictures/,
  component: PicturesCard,
  mockTool: "giap-knowledge__show_pictures",
  mockData: {
    query: "pictures of a town library",
    pictures: [
      {
        title: "The reading room",
        url: "https://library.example.org/visit",
        domain: "library.example.org",
        thumbnail: "https://imgs.search.brave.com/example/rs:fit:500:0:0/g:ce/reading-room",
        width: 500,
        height: 333,
      },
    ],
  },
});

export { PicturesCard };
