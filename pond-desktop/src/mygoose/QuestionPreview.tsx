// What an option leads to: the first lines of a draft, a place from the web, or a comparison.

import { InkLink, InkTable } from "@jarida/ink/react";
import type { QuestionPreview as Preview } from "../api/types";
import { OPEN_PAGE, PREVIEW_HEADING } from "./myGooseWords";

export function QuestionPreview({ preview, label }: { preview: Preview; label: string }) {
  return (
    <section className="mg-q__preview" aria-label={`${PREVIEW_HEADING}: ${label}`}>
      {preview.kind === "draft" ? <p className="mg-q__draft">{preview.text}</p> : null}
      {preview.kind === "place" ? (
        <div className="mg-q__place">
          {preview.image_url ? <img src={preview.image_url} alt={preview.name} className="mg-q__place-img" /> : null}
          <p className="mg-q__place-name">{preview.name}</p>
          {preview.price ? <p className="mg-q__place-price">{preview.price}</p> : null}
          {preview.url ? (
            <InkLink href={preview.url} external>
              {OPEN_PAGE}
            </InkLink>
          ) : null}
        </div>
      ) : null}
      {preview.kind === "table" ? (
        <InkTable
          caption={label}
          columns={preview.columns.map((header, at) => ({ key: `${at}`, header, render: (row: string[]) => row[at] ?? "" }))}
          rows={preview.rows}
          rowKey={(row) => row.join("|")}
        />
      ) : null}
    </section>
  );
}
