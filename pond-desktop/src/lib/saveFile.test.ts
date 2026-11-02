import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fileNameFor, filenameFromDisposition, saveBlob } from "./saveFile";

describe("the file name a header carries", () => {
  it("reads a quoted name, as the pond sends it", () => {
    expect(filenameFromDisposition('attachment; filename="trip-plan.md"')).toBe("trip-plan.md");
  });

  it("reads an unquoted name and an extended one", () => {
    expect(filenameFromDisposition("attachment; filename=trip-plan.html")).toBe("trip-plan.html");
    expect(filenameFromDisposition("attachment; filename*=UTF-8''Kenya%20tax%20rules.md")).toBe("Kenya tax rules.md");
  });

  it("is null when there is no header, or no name in it", () => {
    expect(filenameFromDisposition(null)).toBeNull();
    expect(filenameFromDisposition("")).toBeNull();
    expect(filenameFromDisposition("attachment")).toBeNull();
  });

  it("keeps a name to one plain file name: no folders, no characters a disk refuses", () => {
    expect(filenameFromDisposition('attachment; filename="../../etc/passwd"')).toBe("passwd");
    expect(filenameFromDisposition('attachment; filename="C:\\\\Users\\\\me\\\\plan.md"')).toBe("plan.md");
    expect(filenameFromDisposition('attachment; filename="a<b>:c|d?.md"')).toBe("abcd.md");
    expect(filenameFromDisposition('attachment; filename="../"')).toBeNull();
  });

  it("reads an escaped quote, and falls back when an extended name is malformed", () => {
    expect(filenameFromDisposition('attachment; filename="say \\"hi\\".md"')).toBe('say hi.md');
    expect(filenameFromDisposition('attachment; filename*=UTF-8\'\'%E0%A4%A; filename="plain.md"')).toBe("plain.md");
  });
});

describe("a name made from a title", () => {
  it("is lower case with hyphens, and no punctuation", () => {
    expect(fileNameFor("Plan a trip to Lamu, 3 days!", "md")).toBe("plan-a-trip-to-lamu-3-days.md");
  });

  it("drops accents", () => {
    expect(fileNameFor("Café Crème", "html")).toBe("cafe-creme.html");
  });

  it("is report when the title has nothing a file name can keep", () => {
    expect(fileNameFor("   ", "ics")).toBe("report.ics");
    expect(fileNameFor("!!!", "md")).toBe("report.md");
  });

  it("is cut short, and never ends on a hyphen", () => {
    const name = fileNameFor(`${"word ".repeat(30)}end`, "md");
    expect(name.endsWith("-.md")).toBe(false);
    expect(name.length).toBeLessThanOrEqual(60 + ".md".length);
  });
});

describe("saving a blob", () => {
  const created = vi.fn(() => "blob:pond/report");
  const revoked = vi.fn();
  let originals: { create: typeof URL.createObjectURL; revoke: typeof URL.revokeObjectURL };

  beforeEach(() => {
    vi.useFakeTimers();
    originals = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
    Object.assign(URL, { createObjectURL: created, revokeObjectURL: revoked });
    created.mockClear();
    revoked.mockClear();
  });

  afterEach(() => {
    Object.assign(URL, { createObjectURL: originals.create, revokeObjectURL: originals.revoke });
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("clicks a download link for the blob, named as asked, and takes the link away", () => {
    const seen: { href: string; download: string; inPage: boolean }[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      seen.push({ href: this.href, download: this.download, inPage: document.body.contains(this) });
    });
    const blob = new Blob(["# Plan"], { type: "text/markdown" });

    saveBlob(blob, "trip-plan.md");

    expect(created).toHaveBeenCalledWith(blob);
    expect(seen).toEqual([{ href: "blob:pond/report", download: "trip-plan.md", inPage: true }]);
    expect(document.querySelector("a[download]")).toBeNull();
  });

  it("lets the blob go only after the browser has had time to read it", () => {
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    saveBlob(new Blob(["x"]), "a.md");
    expect(revoked).not.toHaveBeenCalled();
    vi.advanceTimersByTime(10_000);
    expect(revoked).toHaveBeenCalledWith("blob:pond/report");
  });
});
