import { describe, expect, it } from "vitest";
import { isOpenWhenUsed, openStateLabel } from "./clientPresence";

describe("clients that are only heard from while open", () => {
  it("are the dashboard, the desktop app and the CLI, not a phone or a device", () => {
    expect(isOpenWhenUsed({ device_type: "desktop" })).toBe(true);
    expect(isOpenWhenUsed({ device_type: "cli" })).toBe(true);
    expect(isOpenWhenUsed({ device_type: "gotg" })).toBe(false);
    expect(isOpenWhenUsed({ device_type: "light" })).toBe(false);
    expect(isOpenWhenUsed({})).toBe(false);
  });

  it("word the dashboard and the app as open or not open, and nothing else", () => {
    expect(openStateLabel({ device_type: "desktop", is_online: true })).toBe("Open");
    expect(openStateLabel({ device_type: "desktop", is_online: false })).toBe("Not open");
    expect(openStateLabel({ device_type: "gotg", is_online: false })).toBeNull();
    expect(openStateLabel({ device_type: "sensor", is_online: true })).toBeNull();
  });
});
