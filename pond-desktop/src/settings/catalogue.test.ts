import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { CATALOGUE, allEntries, inertCount } from "./catalogue";

// Frontend half of pond-core's `every_settings_field_is_dispositioned`: every TS `Settings` field
// needs a catalogue entry. `types.ts` is scanned as source, so a key-count floor guards a vacuous pass.

function settingsTypeKeys(): string[] {
  // From the vitest root: the transform doesn't always give this module a file: URL.
  const src = readFileSync(resolve(process.cwd(), "src/api/types.ts"), "utf8");

  const start = src.indexOf("export interface Settings {");
  expect(start, "the Settings interface moved or was renamed").toBeGreaterThan(-1);
  const end = src.indexOf("\n}", start);
  expect(end, "could not find the end of the Settings interface").toBeGreaterThan(start);

  const body = src.slice(start, end);
  // Two-space indent only, so nested object literals are not mistaken for fields.
  const keys = [...body.matchAll(/^ {2}([a-z_0-9]+)\??:/gm)].map((m) => m[1]);
  return [...new Set(keys)];
}

describe("settings catalogue", () => {
  const typeKeys = settingsTypeKeys();
  const entries = allEntries();
  const entryKeys = entries.map((e) => e.key as string);

  it("scans a plausible number of fields (guards a vacuous pass)", () => {
    expect(typeKeys.length).toBeGreaterThan(100);
  });

  it("lists every setting the app's type names, and nothing else", () => {
    const missing = typeKeys.filter((k) => !entryKeys.includes(k));
    const extra = entryKeys.filter((k) => !typeKeys.includes(k));
    expect(missing, `settings with no home in the catalogue: ${missing.join(", ")}`).toEqual([]);
    expect(extra, `catalogue entries that are not Settings fields: ${extra.join(", ")}`).toEqual([]);
  });

  it("lists each setting exactly once", () => {
    const seen = new Set<string>();
    const dupes = entryKeys.filter((k) => (seen.has(k) ? true : (seen.add(k), false)));
    expect(dupes, `settings in more than one category: ${dupes.join(", ")}`).toEqual([]);
  });

  it("explains every mark that is not 'connected'", () => {
    const unexplained = entries
      .filter((e) => e.consumer !== "live" && !e.note?.trim())
      .map((e) => e.key);
    expect(unexplained, `needs a note: ${unexplained.join(", ")}`).toEqual([]);
  });

  it("does not attach a note to a connected setting", () => {
    // Note styling is reserved for the two exceptional states.
    const spurious = entries.filter((e) => e.consumer === "live" && e.note).map((e) => e.key);
    expect(spurious).toEqual([]);
  });

  it("writes labels for people, not field names", () => {
    const machineish = entries
      .filter((e) => /_/.test(e.label) || e.label === e.key)
      .map((e) => e.key);
    expect(machineish, `labels that read as keys: ${machineish.join(", ")}`).toEqual([]);
  });

  it("gives every category a stable id, a tier and a blurb", () => {
    const ids = CATALOGUE.map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const c of CATALOGUE) {
      expect(c.blurb.length, `${c.id} needs a blurb`).toBeGreaterThan(10);
      expect(c.groups.length, `${c.id} needs at least one group`).toBeGreaterThan(0);
      for (const g of c.groups) expect(g.entries.length, `${c.id}/${g.name} is empty`).toBeGreaterThan(0);
    }
  });

  it("offers only server-accepted options for the validated enums", () => {
    // `update_settings` 422s on any other value; radio and select both carry the option set.
    const optionsFor = (key: string) => {
      const c = entries.find((x) => x.key === key)?.control;
      if (c?.kind === "select") return [...c.options];
      if (c?.kind === "radio") return c.options.map((o) => o.value);
      return null;
    };
    // Sorted: the UI orders by what to reach for first; the server only cares about the set.
    expect(optionsFor("network_mode")?.sort()).toEqual(["allowlist", "offline", "open"]);
    expect(optionsFor("reasoning_effort")?.sort()).toEqual(["balanced", "brief", "thorough"]);
    expect(optionsFor("security_policy_mode")?.sort()).toEqual(["audit", "enforce", "off"]);
    // "pond" is quarantined server-side and 422s, so it is not offered.
    expect(optionsFor("agent_backend")).toEqual(["goose"]);
  });

  it("validates the four fields the server rejects with 422", () => {
    for (const key of ["network_mode", "reasoning_effort", "agent_backend", "matter_ws_url"]) {
      const e = entries.find((x) => x.key === key);
      expect(e?.validate, `${key} must be validated client-side`).toBeTypeOf("function");
      expect(e!.validate!("nonsense-value"), `${key} should reject a bad value`).toBeTruthy();
    }
  });

  it("accepts each offered option as valid", () => {
    for (const e of entries) {
      if (!e.validate) continue;
      const opts =
        e.control.kind === "select" ? [...e.control.options]
        : e.control.kind === "radio" ? e.control.options.map((o) => o.value)
        : [];
      for (const o of opts) {
        expect(e.validate(o), `${e.key} offers "${o}" but rejects it`).toBeNull();
      }
    }
  });

  it("bounds every numeric control it validates", () => {
    // Without min/max the number box renders no native stepper limits.
    const unbounded = entries
      .filter((e) => e.control.kind === "number" && e.validate && e.control.min === undefined)
      .map((e) => e.key);
    expect(unbounded, `numeric controls missing a floor: ${unbounded.join(", ")}`).toEqual([]);
  });

  it("counts the inert settings the audit found", () => {
    // If this moves, something got wired up (mark it "live") or a new inert control shipped.
    const inert = entries.filter((e) => e.consumer === "none");
    expect(inert).toHaveLength(10);
    expect(entries.filter((e) => e.consumer === "app")).toHaveLength(3);

    const flagged = CATALOGUE.reduce((n, c) => n + inertCount(c), 0);
    expect(flagged).toBe(inert.length);
  });
});
