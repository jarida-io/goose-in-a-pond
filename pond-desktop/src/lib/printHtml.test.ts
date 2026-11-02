import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { printHtml } from "./printHtml";

// The frame's own five-minute release is a timer; faked, it is never left pending.
beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  document.querySelectorAll("iframe").forEach((f) => f.remove());
  vi.useRealTimers();
});

/** Gives the frame a window of its own, as it has once it has loaded, and says it has. */
function load(frame: HTMLIFrameElement, view: Record<string, unknown>) {
  Object.defineProperty(frame, "contentWindow", { value: view, configurable: true });
  frame.dispatchEvent(new Event("load"));
}

describe("printing a page the pond wrote", () => {
  it("puts the page in a frame nobody sees, that can run nothing", () => {
    void printHtml("<p>hello</p>").catch(() => {});
    const frame = document.querySelector("iframe") as HTMLIFrameElement;
    expect(frame.srcdoc).toBe("<p>hello</p>");
    const sandbox = frame.getAttribute("sandbox") ?? "";
    expect(sandbox).toContain("allow-modals");
    expect(sandbox).toContain("allow-same-origin");
    expect(sandbox).not.toContain("allow-scripts");
    expect(frame.getAttribute("aria-hidden")).toBe("true");
    expect(frame.tabIndex).toBe(-1);
  });

  it("asks for the print window once the page has loaded, then lets the frame go when printing ends", async () => {
    const done = printHtml("<p>hello</p>");
    const frame = document.querySelector("iframe") as HTMLIFrameElement;
    const print = vi.fn();
    const listeners: Record<string, () => void> = {};
    load(frame, {
      focus: vi.fn(),
      print,
      addEventListener: (name: string, fn: () => void) => {
        listeners[name] = fn;
      },
    });

    await done;
    expect(print).toHaveBeenCalledTimes(1);
    expect(document.querySelector("iframe")).not.toBeNull();
    listeners.afterprint();
    expect(document.querySelector("iframe")).toBeNull();
  });

  it("rejects, and takes the frame away, when printing is refused", async () => {
    const done = printHtml("<p>hello</p>");
    const frame = document.querySelector("iframe") as HTMLIFrameElement;
    load(frame, {
      focus: vi.fn(),
      print: () => {
        throw new Error("no printer");
      },
      addEventListener: vi.fn(),
    });
    await expect(done).rejects.toThrow("no printer");
    expect(document.querySelector("iframe")).toBeNull();
  });

  it("rejects when the frame has no window to print", async () => {
    const done = printHtml("<p>hello</p>");
    load(document.querySelector("iframe") as HTMLIFrameElement, null as unknown as Record<string, unknown>);
    await expect(done).rejects.toThrow(/could not be opened/);
    expect(document.querySelector("iframe")).toBeNull();
  });
});
