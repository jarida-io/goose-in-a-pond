import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  PROVIDERS,
  describeHaul,
  describeLastSync,
  describeSourceOutcome,
  describeStatus,
} from "./providers";

const REPO = join(__dirname, "..", "..", "..");

function rustSource(path: string): string {
  return readFileSync(join(REPO, path), "utf8");
}

describe("provider list", () => {
  /** Ids are stored in `context_sources.provider`; one the backend can't parse would never sync. */
  it("offers only calendar providers the CalDAV adapter can rebuild", () => {
    const rust = rustSource("crates/pond-adapters-caldav/src/provider.rs");
    // `"name" =>` matches only from_stored's arms (as_str is `Self::X => "x"`), all five of them.
    const known = [...rust.matchAll(/"([a-z]+)" =>/g)].map((m) => m[1]);
    expect(known.length).toBeGreaterThan(2);
    for (const p of PROVIDERS.filter((p) => p.kind === "calendar")) {
      expect(known, `calendar provider "${p.id}" is not in from_stored`).toContain(p.id);
    }
  });

  it("offers only mail providers the IMAP adapter can rebuild", () => {
    const rust = rustSource("crates/pond-adapters-imap/src/provider.rs");
    const known = [...rust.matchAll(/"([a-z]+)" =>/g)].map((m) => m[1]);
    expect(known.length).toBeGreaterThan(2);
    for (const p of PROVIDERS.filter((p) => p.kind === "mail")) {
      expect(known, `mail provider "${p.id}" is not in from_stored`).toContain(p.id);
    }
  });

  /** A provider whose self-hosted server cannot be supplied cannot connect. */
  it("asks for a server address for exactly the self-hosted providers", () => {
    const selfHosted = PROVIDERS.filter((p) => p.needsServer).map((p) => p.id);
    expect(selfHosted.sort()).toEqual(["custom", "nextcloud"]);
    for (const p of PROVIDERS.filter((p) => p.needsServer)) {
      expect(p.serverPlaceholder, `${p.label} has no placeholder`).toBeTruthy();
    }
  });

  /** `from_stored` still parses `google` for old rows; parsing alone can't justify listing it. */
  it("does not offer a calendar provider the backend refuses to connect", () => {
    const rust = rustSource("crates/pond-adapters-caldav/src/provider.rs");
    const unconnectable = rust.includes("!matches!(self, Self::Google)");
    expect(unconnectable, "is_connectable no longer refuses Google").toBe(true);
    expect(
      PROVIDERS.filter((p) => p.kind === "calendar").map((p) => p.id),
    ).not.toContain("google");
  });

  /** The hint is the whole reason a household gets past the password box. */
  it("gives every provider a setup hint", () => {
    for (const p of PROVIDERS) {
      expect(p.hint.length, `${p.label} has no hint`).toBeGreaterThan(30);
    }
  });
});

describe("status wording", () => {
  it("tells somebody what to do about a refused password", () => {
    const s = describeStatus("needs_reauth");
    expect(s.tone).toBe("warn");
    expect(s.detail).toMatch(/reconnect/i);
  });

  /** Offline is a choice the operator made, not a fault to badge. */
  it("does not present an offline pond as broken", () => {
    const s = describeStatus("paused");
    expect(s.tone).toBe("muted");
    expect(s.detail).toMatch(/nothing is broken/i);
  });

  it("treats an unknown status as needing attention rather than as fine", () => {
    expect(describeStatus("wat").tone).toBe("warn");
  });
});

describe("last sync wording", () => {
  /** "Never checked" and "checked, found nothing" are different problems. */
  it("says so when the pond has never reached the account", () => {
    expect(describeLastSync(null)).toMatch(/not checked yet/i);
    expect(describeLastSync("not-a-date")).toMatch(/not checked yet/i);
  });

  it("reads in the units a person would use", () => {
    const now = Date.parse("2026-08-17T12:00:00Z");
    expect(describeLastSync("2026-08-17T11:58:00Z", now)).toMatch(/2 minutes ago/);
    expect(describeLastSync("2026-08-17T09:00:00Z", now)).toMatch(/3 hours ago/);
    expect(describeLastSync("2026-08-15T12:00:00Z", now)).toMatch(/2 days ago/);
    expect(describeLastSync("2026-08-17T11:59:50Z", now)).toMatch(/just now/i);
  });
});

describe("what a source has brought in", () => {
  /** Two numbers, not a percentage: the unsearchable count explains an empty search. */
  it("separates what was read from what can be found", () => {
    expect(describeHaul(142, 0)).toBe("142 things read, all searchable");
    expect(describeHaul(142, 8)).toBe("142 things read, 8 not searchable yet");
    expect(describeHaul(142, 142)).toBe("142 things read, not searchable yet");
  });

  it("says nothing at all when a source has brought nothing", () => {
    expect(describeHaul(0, 0)).toBeNull();
  });

  it("counts one thing as one thing", () => {
    expect(describeHaul(1, 0)).toBe("1 thing read, all searchable");
  });
});

describe("per-account check results", () => {
  /** With two accounts, a total cannot say which one is broken. */
  it("gives every outcome its own words", () => {
    expect(describeSourceOutcome("ingested", 3)).toBe("3 new things");
    expect(describeSourceOutcome("ingested", 1)).toBe("1 new thing");
    expect(describeSourceOutcome("unchanged", 0)).toBe("nothing new");
    expect(describeSourceOutcome("needs_reauth", 0)).toMatch(/password refused/);
    expect(describeSourceOutcome("paused", 0)).toMatch(/offline/);
    expect(describeSourceOutcome("anything-else", 0)).toMatch(/could not be reached/);
  });
});
