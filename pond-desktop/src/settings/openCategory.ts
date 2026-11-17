/**
 * Opening the settings catalogue at one category from another screen, such as the pairing page's
 * link to Connections. The catalogue reads the request once, when it opens, and the request is
 * spent: opening Settings again later starts where it always does.
 */

let pending: string | null = null;

/** Ask the next opening of the settings catalogue to start at `categoryId`. */
export function openSettingsAt(categoryId: string): void {
  pending = categoryId;
}

/** The category asked for, once; `null` when none was. */
export function takeSettingsCategory(): string | null {
  const id = pending;
  pending = null;
  return id;
}
