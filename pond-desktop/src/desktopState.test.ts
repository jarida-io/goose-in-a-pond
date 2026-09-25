import { describe, expect, it } from "vitest";
import {
  DESKTOP_MODES,
  DESKTOP_SECTIONS,
  getDesktopInitials,
  getDesktopSectionLabel,
  normalizeDesktopMode,
  normalizeGuiSection,
} from "./desktopState";

describe("desktopState", () => {
  it("normalizes the desktop mode with a gui fallback", () => {
    expect(normalizeDesktopMode("gui")).toBe("gui");
    expect(normalizeDesktopMode("voice")).toBe("voice");
    // A stale persisted "canvas" mode self-heals to "gui".
    expect(normalizeDesktopMode("canvas")).toBe("gui");
    expect(normalizeDesktopMode("unexpected")).toBe("gui");
    expect(normalizeDesktopMode(null)).toBe("gui");
    expect(normalizeDesktopMode(undefined)).toBe("gui");
    expect(DESKTOP_MODES).toEqual(["gui", "voice"]);
  });

  it("normalizes the gui section with a dashboard fallback", () => {
    expect(normalizeGuiSection("dashboard")).toBe("dashboard");
    expect(normalizeGuiSection("chat")).toBe("chat");
    expect(normalizeGuiSection("models")).toBe("models");
    expect(normalizeGuiSection("prompts")).toBe("prompts");
    expect(normalizeGuiSection("settings")).toBe("settings");
    expect(normalizeGuiSection("devices")).toBe("devices");
    expect(normalizeGuiSection("pairing")).toBe("pairing");
    expect(normalizeGuiSection("schedules")).toBe("schedules");
    // Renamed ids land on their new section, not the dashboard.
    expect(normalizeGuiSection("memory")).toBe("context");
    expect(normalizeGuiSection("connections")).toBe("context");
    expect(normalizeGuiSection("not-a-section")).toBe("dashboard");
    expect(normalizeGuiSection("skills")).toBe("skills");
    expect(normalizeGuiSection("unexpected")).toBe("dashboard");
    expect(normalizeGuiSection(null)).toBe("dashboard");
    expect(normalizeGuiSection(undefined)).toBe("dashboard");
  });

  it("DESKTOP_SECTIONS is a flat ordered list of all sidebar items", () => {
    // 14 sidebar items; canvas, faces and hub are routable but hidden (HIDDEN_SECTIONS).
    expect(DESKTOP_SECTIONS).toHaveLength(14);
    expect(DESKTOP_SECTIONS[0]).toEqual({ section: "dashboard", label: "Home" });
    expect(DESKTOP_SECTIONS[1]).toEqual({ section: "chat", label: "Chat" });
    const sectionKeys = DESKTOP_SECTIONS.map((s) => s.section);
    expect(sectionKeys).toContain("devices");
    expect(sectionKeys).toContain("mesh");
    expect(sectionKeys).toContain("pairing");
    expect(sectionKeys).toContain("schedules");
    expect(sectionKeys).toContain("context");
    expect(sectionKeys).toContain("skills");
    expect(sectionKeys).toContain("recipes");
    expect(sectionKeys).toContain("extensions");
    expect(sectionKeys).toContain("models");
    expect(sectionKeys).toContain("prompts");
    expect(sectionKeys).toContain("settings");
    expect(sectionKeys).toContain("logs");
  });

  it("hidden sections are still valid for normalizeGuiSection", () => {
    expect(normalizeGuiSection("faces")).toBe("faces");
    expect(normalizeGuiSection("canvas")).toBe("canvas");
  });

  it("derives consistent desktop section labels", () => {
    // The id stays `dashboard` (persisted as `giap-section`); only its label is "Home".
    expect(getDesktopSectionLabel("dashboard")).toBe("Home");
    expect(getDesktopSectionLabel("chat")).toBe("Chat");
    expect(getDesktopSectionLabel("settings")).toBe("Settings");
    expect(getDesktopSectionLabel("models")).toBe("Models");
  });

  it("derives initials from assistant name", () => {
    expect(getDesktopInitials("Goose In A Pond")).toBe("GI");
    expect(getDesktopInitials("Pond")).toBe("P");
    expect(getDesktopInitials("A B")).toBe("AB");
    expect(getDesktopInitials("")).toBe("?");
  });
});
