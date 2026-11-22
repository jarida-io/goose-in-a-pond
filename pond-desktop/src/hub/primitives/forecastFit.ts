/** Narrowest a forecast day tile is laid out, in px: the weekday, the icon and "-12°C" fit. */
const DAY_TILE_MIN_W = 46;

/** Gap between day tiles, in px; `.wx__fc`'s `gap` in hub.css. */
const DAY_TILE_GAP = 6;

/** How many of the forecast's days the strip shows, and in how many columns. */
export interface ForecastFit {
  shown: number;
  columns: number;
}

/**
 * Fits `total` day tiles of height `tileHeight` into a `width` x `height` strip: as many as the
 * box holds in whole rows and columns, never fewer than one row. A taller card shows more days
 * rather than a band of empty sky; a shorter one drops the furthest days rather than overflowing.
 *
 * The shown tiles are spread over the fewest rows they need and then as evenly as those rows
 * allow, so seven days in two rows read as four and three, not six and a lone one.
 */
export function forecastFit(
  total: number,
  width: number,
  height: number,
  tileHeight: number,
): ForecastFit {
  const maxColumns = Math.max(1, Math.floor((width + DAY_TILE_GAP) / (DAY_TILE_MIN_W + DAY_TILE_GAP)));
  const maxRows = Math.max(1, Math.floor((height + DAY_TILE_GAP) / (tileHeight + DAY_TILE_GAP)));
  const shown = Math.max(0, Math.min(total, maxColumns * maxRows));
  if (shown === 0) return { shown: 0, columns: 1 };

  const rows = Math.ceil(shown / maxColumns);
  return { shown, columns: Math.ceil(shown / rows) };
}
