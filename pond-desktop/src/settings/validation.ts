// ─── Settings validation ───────────────────────────────────────────────────
// The server rejects only `network_mode`, `reasoning_effort`, `agent_backend` and `matter_ws_url`;
// other bad values are stored and silently defaulted. Reject only what is surely wrong.

/** `null` means valid. A string is the message shown under the control. */
export type Validator = (value: unknown) => string | null;

const isBlank = (v: unknown): boolean =>
  v == null || (typeof v === "string" && v.trim() === "");

/** Passes anything empty. Compose with `required` when a value is mandatory. */
export function optional(v: Validator): Validator {
  return (value) => (isBlank(value) ? null : v(value));
}

export function required(message = "This cannot be empty."): Validator {
  return (value) => (isBlank(value) ? message : null);
}

/** Local 24-hour clock time, `HH:MM`. */
export const hhmm: Validator = (value) => {
  const s = String(value ?? "").trim();
  if (!/^\d{1,2}:\d{2}$/.test(s)) return "Use a 24-hour time like 22:00.";
  const [h, m] = s.split(":").map(Number);
  if (h > 23) return "Hours run from 00 to 23.";
  if (m > 59) return "Minutes run from 00 to 59.";
  return null;
};

/** A URL restricted to the schemes a given field can actually open. */
export function url(schemes: string[], example: string): Validator {
  return (value) => {
    const s = String(value ?? "").trim();
    if (!schemes.some((p) => s.startsWith(p))) {
      return `Must start with ${schemes.join(" or ")} — for example ${example}`;
    }
    try {
      new URL(s);
    } catch {
      return `That is not a complete address. Try ${example}`;
    }
    return null;
  };
}

export function range(min: number, max: number, unit = ""): Validator {
  return (value) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return "Enter a number.";
    if (n < min || n > max) return `Must be between ${min} and ${max}${unit ? ` ${unit}` : ""}.`;
    return null;
  };
}

export function atLeast(min: number, unit = ""): Validator {
  return (value) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return "Enter a number.";
    if (n < min) return `Must be ${min}${unit ? ` ${unit}` : ""} or more.`;
    return null;
  };
}

export const integer: Validator = (value) => {
  const n = Number(value);
  if (!Number.isFinite(n)) return "Enter a number.";
  if (!Number.isInteger(n)) return "Enter a whole number.";
  return null;
};

/** Every validator in order; the first complaint wins. */
export function all(...vs: Validator[]): Validator {
  return (value) => {
    for (const v of vs) {
      const msg = v(value);
      if (msg) return msg;
    }
    return null;
  };
}

export function oneOf(options: readonly string[]): Validator {
  return (value) =>
    options.includes(String(value)) ? null : `Choose one of ${options.join(", ")}.`;
}

/** `network 14, sensor 7`: category and whole days; typed, since the server's category set is open. */
export const retentionMap: Validator = (value) => {
  if (value == null) return null;
  if (typeof value === "object" && !Array.isArray(value)) {
    for (const [k, n] of Object.entries(value as Record<string, unknown>)) {
      if (!/^[a-z_]+$/.test(k)) return `“${k}” is not a category name.`;
      if (!Number.isInteger(Number(n)) || Number(n) < 0) {
        return `“${k}” needs a whole number of days.`;
      }
    }
    return null;
  }
  return "Write pairs like: network 14, sensor 7";
};

/** Comma-separated categories. Empty is rejected: server-side it matches nothing, so the pond never speaks. */
export const speechCategories: Validator = (value) => {
  const s = String(value ?? "").trim();
  if (!s) return "Name at least one category, such as alert.";
  const parts = s.split(",").map((p) => p.trim()).filter(Boolean);
  if (!parts.length) return "Name at least one category, such as alert.";
  const bad = parts.filter((p) => !/^[a-z_]+$/.test(p));
  if (bad.length) return `Not a category: ${bad.join(", ")}. Use lower-case names like alert, info.`;
  return null;
};

/** Latitude / longitude, in decimal degrees. */
export const latitude = range(-90, 90, "degrees");
export const longitude = range(-180, 180, "degrees");

/** An IANA zone this platform's `Intl` recognises, not one from a bundled list. */
export const ianaTimezone: Validator = (value) => {
  const s = String(value ?? "").trim();
  if (!s) return "Choose a time zone.";
  try {
    new Intl.DateTimeFormat("en", { timeZone: s });
    return null;
  } catch {
    return `“${s}” is not a time zone this device knows.`;
  }
};
