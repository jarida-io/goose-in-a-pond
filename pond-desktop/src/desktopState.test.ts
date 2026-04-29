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
    expect(normalizeDesktopMode("canvas")).toBe("canvas");
    expect(normalizeDesktopMode("unexpected")).toBe("gui");
    expect(normalizeDesktopMode(null)).toBe("gui");
    expect(normalizeDesktopMode(undefined)).toBe("gui");
    expect(DESKTOP_MODES).toEqual(["gui", "voice", "canvas"]);
  });

  it("normalizes the gui section with a dashboard fallback", () => {
    expect(normalizeGuiSection("dashboard")).toBe("dashboard");
    expect(normalizeGuiSection("chat")).toBe("chat");
    expect(normalizeGuiSection("models")).toBe("models");
    expect(normalizeGuiSection("prompts")).toBe("prompts");
    expect(normalizeGuiSection("settings")).toBe("settings");
    expect(normalizeGuiSection("agent")).toBe("agent");
    expect(normalizeGuiSection("devices")).toBe("devices");
    expect(normalizeGuiSection("schedules")).toBe("schedules");
    expect(normalizeGuiSection("memory")).toBe("memory");
    expect(normalizeGuiSection("skills")).toBe("skills");
    // Unknown sections fall back to "dashboard"
    expect(normalizeGuiSection("unexpected")).toBe("dashboard");
    expect(normalizeGuiSection(null)).toBe("dashboard");
    expect(normalizeGuiSection(undefined)).toBe("dashboard");
  });

  it("DESKTOP_SECTIONS is a flat ordered list of all 12 sidebar items", () => {
    // 13 = dashboard, chat, canvas, devices, schedules, memory, skills,
    // models, prompts, faces, settings, agent, logs.
    expect(DESKTOP_SECTIONS).toHaveLength(13);
    expect(DESKTOP_SECTIONS[0]).toEqual({ section: "dashboard", label: "Dashboard" });
    expect(DESKTOP_SECTIONS[1]).toEqual({ section: "chat", label: "Chat" });
    const sectionKeys = DESKTOP_SECTIONS.map((s) => s.section);
    expect(sectionKeys).toContain("devices");
    expect(sectionKeys).toContain("schedules");
    expect(sectionKeys).toContain("memory");
    expect(sectionKeys).toContain("skills");
    expect(sectionKeys).toContain("models");
    expect(sectionKeys).toContain("prompts");
    expect(sectionKeys).toContain("faces");
    expect(sectionKeys).toContain("settings");
    expect(sectionKeys).toContain("agent");
    expect(sectionKeys).toContain("logs");
  });

  it("derives consistent desktop section labels", () => {
    expect(getDesktopSectionLabel("dashboard")).toBe("Dashboard");
    expect(getDesktopSectionLabel("chat")).toBe("Chat");
    expect(getDesktopSectionLabel("settings")).toBe("Settings");
    expect(getDesktopSectionLabel("models")).toBe("Models");
    expect(getDesktopSectionLabel("agent")).toBe("Agent");
  });

  it("derives initials from assistant name", () => {
    expect(getDesktopInitials("Goose In A Pond")).toBe("GI");
    expect(getDesktopInitials("Pond")).toBe("P");
    expect(getDesktopInitials("A B")).toBe("AB");
    expect(getDesktopInitials("")).toBe("?");
  });
});
