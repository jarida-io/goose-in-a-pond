// Where the strip's items sit, not just that they render.
//
// This file exists because the one defect it pins was invisible to every
// existing test: `DashboardGrid.test.tsx` asserts that `.hbar__temp` is absent
// when weather is off and nothing else about this strip, so the arrange pill
// could sit at the panel's far left -- stacked under the shell's hamburger, in
// the one header position the design keeps empty -- through a green suite.
//
// Placement is the assertion, so these tests read the DOM's own ordering
// rather than a class name. No CSS is loaded under happy-dom, so nothing here
// can claim a pixel; what it can prove is which cluster owns which child, and
// in what order, which is what the defect got wrong.

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
  it("leaves the strip's left edge empty, where the shell's hamburger already is", () => {
    const { strip } = renderStrip(false);

    // One cluster, and it is the right-hand one. A second child here is a
    // control at x=20 directly beneath the shell bar's own left cluster.
    expect(strip.children).toHaveLength(1);
    expect(strip.firstElementChild?.classList.contains("hbar__right")).toBe(true);
    expect(strip.querySelector(".hbar__left")).toBeNull();
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

    // Immediately left of the pill, not a left-edge sibling of the cluster --
    // the collision the pill itself was moved out of.
    expect(strip.children).toHaveLength(1);
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
