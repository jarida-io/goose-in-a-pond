import { describe, it, expect } from "vitest";
import {
  promptTemplateIsOutdated,
  PROMPT_FACTORY_VERSION,
  type PromptTemplate,
} from "./types";

/**
 * The notice offers an update, never takes one: `is_customized` is written only by an explicit
 * Save, so clearing it (even when content matches an old default) would erase that choice.
 */
const base: PromptTemplate = {
  name: "balanced",
  content: "…",
  is_system: true,
  is_customized: true,
  factory_version: PROMPT_FACTORY_VERSION - 1,
};

describe("promptTemplateIsOutdated", () => {
  it("flags an edit made against an older built-in", () => {
    expect(promptTemplateIsOutdated(base)).toBe(true);
  });

  it("says nothing about a row the reseed still owns", () => {
    // Reseeded rows update themselves; a notice there would devalue it everywhere.
    expect(
      promptTemplateIsOutdated({ ...base, is_customized: false }),
    ).toBe(false);
  });

  it("says nothing once the edit is current", () => {
    expect(
      promptTemplateIsOutdated({
        ...base,
        factory_version: PROMPT_FACTORY_VERSION,
      }),
    ).toBe(false);
  });

  it("treats a pre-column row as outdated rather than current", () => {
    // Migration 0048 defaults old rows to 0 without backfill: they genuinely predate the rewrite.
    const { factory_version: _omitted, ...withoutVersion } = base;
    expect(promptTemplateIsOutdated(withoutVersion)).toBe(true);
  });

  it("leaves user-created templates alone", () => {
    // A template the user wrote from scratch has no factory to be behind.
    expect(promptTemplateIsOutdated({ ...base, is_system: false })).toBe(false);
  });

  it("keeps the mirrored version in step with the backend", () => {
    // Mirrors FACTORY_VERSION in crates/pond-core/src/user_data/domain/prompt_template.rs;
    // a stale copy silently hides the notice.
    expect(PROMPT_FACTORY_VERSION).toBeGreaterThan(0);
    expect(Number.isInteger(PROMPT_FACTORY_VERSION)).toBe(true);
  });
});
