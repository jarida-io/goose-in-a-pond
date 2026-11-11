// What picture a notification carries: what it is about, else where it opens, else its category's.
import { describe, expect, it } from "vitest";
import { noticeIcon } from "./notifications";

describe("noticeIcon", () => {
  it("shows what a notice is about when the Pond names its kind", () => {
    expect(noticeIcon({ kind: "model_downloaded", open: "models", category: "info" })).toBe("download");
    expect(noticeIcon({ kind: "remote_access_down", open: "remote_access", category: "alert" })).toBe("offline");
    expect(noticeIcon({ kind: "disk_low", category: "alert" })).toBe("diskLow");
  });

  it("falls back to the screen it opens, so My Pond unlocked is a padlock and not a warning sign", () => {
    expect(noticeIcon({ open: "my_pond", category: "alert" })).toBe("unlocked");
    expect(noticeIcon({ kind: "something_new", open: "watch", category: "info" })).toBe("watch");
  });

  it("falls back to its category last", () => {
    expect(noticeIcon({ category: "alert" })).toBe("alert");
    expect(noticeIcon({ open: "nowhere", category: "action_required" })).toBe("request");
    expect(noticeIcon({ category: "schedule" })).toBe("schedule");
  });
});
