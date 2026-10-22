// Where the strip's items sit. happy-dom loads no CSS, so these assert DOM order: which cluster
// owns which child, and in what order.

import { render, screen, cleanup } from "@testing-library/react";
import { describe, expect, it, afterEach } from "vitest";
import { HomeStatusBar } from "./HomeStatusBar";

afterEach(cleanup);

function renderStrip(arranging: boolean) {
  const { container } = render(
    <HomeStatusBar
      arranging={arranging}
      onToggleArrange={() => {}}
      temp={12}
      userName="Jerry"
      subtitle="All quiet."
    />,
  );
  const strip = container.querySelector('[data-hook="home-status"]');
  if (!strip) throw new Error("the strip did not render");
  const right = strip.querySelector(".hbar__right");
  if (!right) throw new Error("the strip has no right-hand cluster");
  return { strip, right };
}

/** Position of an element among its parent's element children, or -1. */
function indexIn(parent: Element, el: Element | null): number {
  return el ? Array.from(parent.children).indexOf(el) : -1;
}

describe("HomeStatusBar placement (design 2a header)", () => {
  it("puts the time-of-day greeting at the strip's left edge, ahead of the right cluster", () => {
    const { strip } = renderStrip(false);

    // Left greeting, then the right cluster -- the left slot now carries the
    // greeting, stacked under the shell bar's hamburger rather than empty.
    expect(strip.children).toHaveLength(2);
    expect(strip.firstElementChild?.classList.contains("hbar__left")).toBe(true);
    expect(strip.querySelector(".hbar__greeting")).not.toBeNull();
  });

  it("puts the arrange control in the right-hand cluster, ahead of the temperature", () => {
    const { right } = renderStrip(false);

    const pill = right.querySelector(".hbar__arrange");
    expect(pill).not.toBeNull();
    expect(indexIn(right, pill)).toBeGreaterThanOrEqual(0);
    expect(indexIn(right, pill)).toBeLessThan(
      indexIn(right, right.querySelector(".hbar__temp")),
    );
    expect(indexIn(right, pill)).toBeLessThan(
      indexIn(right, right.querySelector(".hbar__avatar")),
    );
  });

  it("keeps the arranging caption inside that cluster, next to the pill it explains", () => {
    const { strip, right } = renderStrip(true);

    const caption = right.querySelector(".hbar__caption");
    expect(caption).not.toBeNull();
    expect(caption?.textContent).toContain("use the arrows to reorder");

    // Just left of the pill, within the right cluster -- not the strip's own left slot,
    // which now holds the greeting rather than the arranging caption.
    expect(strip.children).toHaveLength(2);
    expect(indexIn(right, caption)).toBeLessThan(
      indexIn(right, right.querySelector(".hbar__arrange")),
    );
  });

  it("renames the control between the two states", () => {
    renderStrip(false);
    expect(screen.getByRole("button", { name: "Arrange" })).toBeTruthy();
    expect(document.querySelector(".hbar__caption")).toBeNull();

    cleanup();

    renderStrip(true);
    expect(screen.getByRole("button", { name: "Done arranging" })).toBeTruthy();
  });
});
