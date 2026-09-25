import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { McpAppHost } from "./McpAppHost";

afterEach(() => cleanup());

/**
 * The frame runs untrusted MCP-server HTML: with `allow-same-origin` a `srcdoc` iframe gets the host's
 * origin and can read the session tokens in `parent.localStorage`. SEP-1865 also requires distinct origins.
 */
describe("McpAppHost sandbox", () => {
  const APP_HTML = "<!doctype html><title>t</title><body>hi</body>";

  function frame(): HTMLIFrameElement {
    const { container } = render(<McpAppHost html={APP_HTML} toolName="giap-weather__get_current_weather" />);
    const el = container.querySelector("iframe");
    if (!el) throw new Error("no iframe rendered — the host did not mount");
    return el as HTMLIFrameElement;
  }

  it("never grants allow-same-origin to the app frame", () => {
    const sandbox = frame().getAttribute("sandbox") ?? "";
    // Checking allow-scripts too, so a missing sandbox attribute (the least sandboxed state) fails.
    expect(sandbox).toContain("allow-scripts");
    expect(sandbox).not.toContain("allow-same-origin");
  });

  it("grants nothing beyond allow-scripts", () => {
    // Enumerated, so any new token has to be argued for here.
    const tokens = (frame().getAttribute("sandbox") ?? "").split(/\s+/).filter(Boolean);
    expect(tokens).toEqual(["allow-scripts"]);
  });

  it("renders the app HTML into srcdoc, never into src", () => {
    const el = frame();
    expect(el.getAttribute("srcdoc")).toBe(APP_HTML);
    // A `src` would be a navigation the sandbox reasoning above does not cover.
    expect(el.getAttribute("src")).toBeNull();
  });
});

/** An opaque origin serialises as "null"; `"*"` would deliver to whatever origin the frame has later. */
describe("McpAppHost postMessage targeting", () => {
  it("addresses the app frame as the null origin", () => {
    const { container } = render(<McpAppHost html="<p>x</p>" toolName="t" />);
    const el = container.querySelector("iframe") as HTMLIFrameElement;
    const frameWindow = el.contentWindow;
    expect(frameWindow, "jsdom gave the frame no contentWindow to spy on").not.toBeNull();

    const posted: string[] = [];
    // Spy on the frame's window: the component never posts to the host's.
    const spy = vi
      .spyOn(frameWindow!, "postMessage")
      .mockImplementation(((_msg: unknown, origin: string) => {
        posted.push(origin);
      }) as typeof window.postMessage);

    // Drive the handshake the way a real app does.
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { jsonrpc: "2.0", id: 1, method: "ui/initialize" },
        source: frameWindow,
      } as MessageEventInit),
    );

    spy.mockRestore();

    // Asserted first, or a handshake that never arrived would let the loop below pass vacuously.
    expect(posted.length, "the ui/initialize handshake produced no reply").toBeGreaterThan(0);
    for (const origin of posted) {
      expect(origin).toBe("null");
    }
  });
});
