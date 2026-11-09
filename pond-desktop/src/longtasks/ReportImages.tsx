// A report's pictures, each fetched through the api client with the token (an `<img>` can't send it)
// and drawn from a local object URL, with its credit under it.

import { useEffect, useState } from "react";
import { api } from "../api/PondApiClient";
import type { ReportImage } from "../api/types";
import { siteOf } from "./sourceNumbers";
import { PICTURES_LABEL, PICTURE_FILE_PAGE } from "./reportWords";

type Picture = { status: "loading" } | { status: "ready"; url: string } | { status: "failed" };

/** The pond keeps JPEG, PNG, WebP and GIF only; anything else is not drawn. */
function drawable(blob: Blob): boolean {
  return /^image\/(jpeg|png|webp|gif)$/i.test(blob.type);
}

function usePicture(src: string): Picture {
  const [picture, setPicture] = useState<Picture>({ status: "loading" });
  useEffect(() => {
    let alive = true;
    let made: string | null = null;
    setPicture({ status: "loading" });
    // A partial api mock may lack the method; a sync throw reads as a failed fetch.
    Promise.resolve()
      .then(() => api.getReportImage(src))
      .then((blob) => {
        if (!alive) return;
        if (!drawable(blob)) {
          setPicture({ status: "failed" });
          return;
        }
        made = URL.createObjectURL(blob);
        setPicture({ status: "ready", url: made });
      })
      .catch(() => {
        if (alive) setPicture({ status: "failed" });
      });
    return () => {
      alive = false;
      if (made) URL.revokeObjectURL(made);
    };
  }, [src]);
  return picture;
}

function Figure({ image }: { image: ReportImage }) {
  const picture = usePicture(image.src);
  if (picture.status === "failed") return null;
  const alt = image.caption ?? `Picture from ${siteOf(image.page)}`;
  return (
    <figure className="rimg">
      {picture.status === "ready" ? (
        <img className="rimg__pic" src={picture.url} alt={alt} />
      ) : (
        <span className="rimg__pic rimg__pic--loading" role="img" aria-label={alt} aria-busy="true" />
      )}
      <figcaption className="rimg__caption">
        {image.caption ? <span className="rimg__words">{image.caption}</span> : null}
        <span className="rimg__credit">
          {image.credit}
          {image.file_page && /^https?:\/\//i.test(image.file_page) ? (
            <>
              {" "}
              <a href={image.file_page} target="_blank" rel="noopener noreferrer">
                {PICTURE_FILE_PAGE}
              </a>
            </>
          ) : null}
        </span>
      </figcaption>
    </figure>
  );
}

export function ReportImages({ images }: { images: ReportImage[] }) {
  if (images.length === 0) return null;
  return (
    <section className="rimgs" aria-label={PICTURES_LABEL} data-count={images.length}>
      {images.map((image) => (
        <Figure key={image.src} image={image} />
      ))}
    </section>
  );
}
