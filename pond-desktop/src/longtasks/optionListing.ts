// A report option kept as a saved listing card.

import type { NewSavedItem, ReportOption } from "../api/types";

const FIELD_NAMES = {
  year: ["year", "model year"],
  mileage: ["mileage", "odometer", "km", "kilometres", "kilometers", "miles"],
  location: ["location", "where", "area", "city"],
} as const;

/** The option as a listing card; the fields a listing has a place for leave the body. */
export function listingOf(option: ReportOption): NewSavedItem {
  const taken = new Set<string>();
  const field = (names: readonly string[]) => {
    const found = option.fields.find(([name]) => names.includes(name.trim().toLowerCase()));
    if (found) taken.add(found[0]);
    return found?.[1];
  };
  const year = field(FIELD_NAMES.year);
  const mileage = field(FIELD_NAMES.mileage);
  const location = field(FIELD_NAMES.location);
  const rest = option.fields.filter(([name]) => !taken.has(name)).map(([name, value]) => `- **${name}:** ${value}`);
  return {
    kind: "listing",
    title: option.title,
    price: option.price,
    link: option.link,
    year,
    mileage,
    location,
    body: [option.why, rest.join("\n")].filter((part) => part && part.trim() !== "").join("\n\n"),
    sources: option.sources,
  };
}
