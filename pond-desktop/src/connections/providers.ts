// ─── The accounts a household can connect ───────────────────────────────────
// Mirrors `CalDavProvider`/`ImapProvider`. `id` is stored in `context_sources.provider`, so
// renaming one orphans sources. Hints copy the crates' `setup_hint()`: the form renders first.

export type ConnectionKind = "calendar" | "mail";

export interface ProviderOption {
  /** Stored in `context_sources.provider`. */
  id: string;
  label: string;
  kind: ConnectionKind;
  /** What the household has to go and do first. */
  hint: string;
  /** Whether this provider needs a server address as well. */
  needsServer?: boolean;
  /** Placeholder for the server field, when there is one. */
  serverPlaceholder?: string;
}

// No Google Calendar: its CalDAV requires OAuth 2.0 and 401s Basic auth. Gmail (IMAP) works.
export const PROVIDERS: ProviderOption[] = [
  {
    id: "gmail",
    label: "Gmail",
    kind: "mail",
    hint: "Turn on 2-Step Verification, then create an app password on your Google account's Security page. Also switch IMAP on in Gmail's own settings, under Forwarding and POP/IMAP — that second step is the one people miss.",
  },
  {
    id: "icloud",
    label: "iCloud Calendar",
    kind: "calendar",
    hint: "Create an app-specific password under Sign-In and Security in your Apple account.",
  },
  {
    id: "icloud",
    label: "iCloud Mail",
    kind: "mail",
    hint: "Use the same app-specific password, and your full iCloud address as the username.",
  },
  {
    id: "fastmail",
    label: "Fastmail Calendar",
    kind: "calendar",
    hint: "Create an app password with calendar access under Settings, Privacy & Security, Connected apps.",
  },
  {
    id: "fastmail",
    label: "Fastmail Mail",
    kind: "mail",
    hint: "Create an app password with mail access under Settings, Privacy & Security, Connected apps.",
  },
  {
    id: "nextcloud",
    label: "Nextcloud Calendar",
    kind: "calendar",
    hint: "Create a device password under Settings, Security. The server address is the one you use in a browser.",
    needsServer: true,
    serverPlaceholder: "https://cloud.example.org/remote.php/dav",
  },
  {
    id: "custom",
    label: "Another mail server",
    kind: "mail",
    hint: "Use your provider's IMAP address. The pond connects over TLS on port 993 and will not fall back to an unencrypted connection.",
    needsServer: true,
    serverPlaceholder: "imap.example.org",
  },
];

/** How a status reads to a person, and how urgently. Uses `lastSync` too: sources are created
 *  `connected`, before anything has run. */
export function describeStatus(
  status: string,
  lastSync?: string | null,
): {
  label: string;
  tone: "ok" | "warn" | "muted";
  detail: string;
} {
  switch (status) {
    case "connected":
      return lastSync
        ? {
            label: "Connected",
            tone: "ok",
            detail: "Working. The pond read this account and found what it expected.",
          }
        : {
            label: "Not checked yet",
            tone: "muted",
            detail:
              "Saved, but the pond has not read this account yet, so the password is still unproven. Check now to find out.",
          };
    case "needs_reauth":
      return {
        label: "Needs attention",
        tone: "warn",
        detail:
          "The password was refused. Usually it was revoked, or an ordinary account password was used. Reconnect to fix it — the pond has stopped trying.",
      };
    case "paused":
      return {
        label: "Paused",
        tone: "muted",
        detail: "This pond is offline, so it is not reaching out. Nothing is broken.",
      };
    default:
      return {
        label: "Not working",
        tone: "warn",
        detail: "The last check failed. The pond will try again on its own.",
      };
  }
}

/** "2 hours ago", or the honest answer when it has never run. */
export function describeLastSync(iso: string | null, now = Date.now()): string {
  if (!iso) return "Not checked yet";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "Not checked yet";
  const mins = Math.max(0, Math.round((now - then) / 60000));
  if (mins < 1) return "Checked just now";
  if (mins < 60) return `Checked ${mins} minute${mins === 1 ? "" : "s"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `Checked ${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `Checked ${days} day${days === 1 ? "" : "s"} ago`;
}

/** What a source has brought in and how much is not yet searchable; two numbers, not a %. */
export function describeHaul(items: number, awaitingIndex: number): string | null {
  if (items === 0) return null;
  const noun = items === 1 ? "1 thing" : `${items} things`;
  if (awaitingIndex === 0) return `${noun} read, all searchable`;
  if (awaitingIndex === items) return `${noun} read, not searchable yet`;
  return `${noun} read, ${awaitingIndex} not searchable yet`;
}

/** One account's line in the result of a check. */
export function describeSourceOutcome(outcome: string, ingested: number): string {
  switch (outcome) {
    case "ingested":
      return ingested === 1 ? "1 new thing" : `${ingested} new things`;
    case "unchanged":
      return "nothing new";
    case "needs_reauth":
      return "password refused";
    case "paused":
      return "not checked, pond is offline";
    default:
      return "could not be reached";
  }
}
