// The dots, and the bookkeeping the drag does around them.
//
// The headless DOM has no layout: `clientWidth` is 0 and `scrollTo` is a stub,
// so nothing here can judge how the track LOOKS mid-drag — that is a real
// scroller against a real compositor and belongs in the E2E suite. What it can
// judge is what the gesture decides: which page it reports, and whether it puts
// the element back the way it found it. Both were wrong in ways the dots alone
// could not see, so the drag block below stubs the two measurements the
// component takes (`clientWidth`, `scrollLeft`) and asserts on the decisions.

import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { WidgetTrack } from "./WidgetTrack";

beforeEach(() => {
  cleanup();
});

function pages(n: number) {
  return Array.from({ length: n }, (_, i) => <p key={i}>Page {i + 1} body</p>);
}

/** Page width the stubs pretend the panel has. Any number works; this one keeps the arithmetic readable. */
const PAGE_W = 300;

/**
 * A track whose two layout reads are answered, and whose `scrollLeft` writes
 * are recorded along with the snap setting in force at the instant of each one.
 *
 * That last part is the only way a test can stand inside the handler: by the
 * time `fireEvent` returns, React has committed, so asking afterwards whether
 * snapping was off would answer about the commit rather than about the write.
 */
function mountTrack(count: number, onPageChange = vi.fn()) {
  const { container } = render(
    <WidgetTrack pages={pages(count)} page={0} onPageChange={onPageChange} />,
  );
  const el = container.querySelector<HTMLDivElement>('[data-hook="widget-track"]');
  if (!el) throw new Error("no track");

  Object.defineProperty(el, "clientWidth", { configurable: true, value: PAGE_W });
  const writes: Array<{ left: number; snap: string }> = [];
  let left = 0;
  Object.defineProperty(el, "scrollLeft", {
    configurable: true,
    get: () => left,
    set: (v: number) => {
      writes.push({ left: v, snap: el.style.scrollSnapType });
      left = v;
    },
  });
  el.setPointerCapture = vi.fn();
  el.releasePointerCapture = vi.fn();
  el.scrollTo = vi.fn();

  return { el, writes, onPageChange };
}

function down(el: HTMLElement, pointerId: number, clientX: number) {
  fireEvent.pointerDown(el, { pointerId, clientX, pointerType: "touch", button: 0 });
}

function move(el: HTMLElement, pointerId: number, clientX: number) {
  fireEvent.pointerMove(el, { pointerId, clientX, pointerType: "touch" });
}

function up(el: HTMLElement, pointerId: number, clientX: number) {
  fireEvent.pointerUp(el, { pointerId, clientX, pointerType: "touch" });
}

describe("the page dots", () => {
  it("has one per page, and marks the one showing", () => {
    render(<WidgetTrack pages={pages(3)} page={1} onPageChange={() => {}} />);
    const dots = screen.getAllByRole("button", { name: /^Page \d of 3$/ });
    expect(dots.length).toBe(3);
    expect(dots[1].getAttribute("aria-current")).toBe("true");
    expect(dots[0].getAttribute("aria-current")).toBeNull();
    expect(dots[2].getAttribute("aria-current")).toBeNull();
  });

  it("reports the page it was asked for", () => {
    const onPageChange = vi.fn();
    render(<WidgetTrack pages={pages(3)} page={0} onPageChange={onPageChange} />);
    fireEvent.click(screen.getByRole("button", { name: "Page 3 of 3" }));
    expect(onPageChange).toHaveBeenCalledWith(2);
  });

  /** One page is not a set of pages. Nothing to choose between, so no dots. */
  it("is not drawn at all for a single page", () => {
    const { container } = render(
      <WidgetTrack pages={pages(1)} page={0} onPageChange={() => {}} />,
    );
    expect(container.querySelector(".wtrack__dots")).toBeNull();
  });

  it("renders every page's contents, not only the one showing", () => {
    render(<WidgetTrack pages={pages(2)} page={0} onPageChange={() => {}} />);
    expect(screen.getByText("Page 1 body")).toBeTruthy();
    expect(screen.getByText("Page 2 body")).toBeTruthy();
  });
});

describe("the drag", () => {
  /**
   * The first frame of the drag. A mandatory-snap container discards a
   * `scrollLeft` write synchronously, so snapping has to be off BEFORE the
   * write in the same handler — not after the render that write's own
   * `setDragging(true)` will eventually cause.
   */
  it("has snapping already off at the first write, not a commit later", () => {
    const { el, writes } = mountTrack(3);
    down(el, 1, 200);
    move(el, 1, 80);

    expect(writes.length).toBeGreaterThan(0);
    expect(writes[0]).toEqual({ left: 120, snap: "none" });
  });

  it("hands snapping back to the stylesheet on release", () => {
    const { el } = mountTrack(3);
    down(el, 1, 200);
    move(el, 1, 80);
    expect(el.style.scrollSnapType).toBe("none");

    up(el, 1, 80);
    expect(el.style.scrollSnapType).toBe("");
  });

  /**
   * A second thumb resting on the panel mid-swipe. It used to overwrite the
   * origin, which made the first finger's release read as a tap: the swipe was
   * discarded and the track was left parked mid-page with snapping off and no
   * further gesture able to clear it.
   */
  it("settles the swipe when a second finger lands on the track mid-drag", () => {
    const { el, onPageChange } = mountTrack(3);
    down(el, 1, 200);
    move(el, 1, 80);
    down(el, 2, 400);
    up(el, 1, 80);

    expect(onPageChange).toHaveBeenCalledWith(1);
    expect(el.style.scrollSnapType).toBe("");
  });

  it("ignores a move and a release that belong to the other finger", () => {
    const { el, writes, onPageChange } = mountTrack(3);
    down(el, 1, 200);
    move(el, 1, 80);
    const afterFingerA = writes.length;

    move(el, 2, 40);
    up(el, 2, 40);
    expect(writes.length).toBe(afterFingerA);
    expect(onPageChange).not.toHaveBeenCalled();
    expect(el.style.scrollSnapType).toBe("none");

    up(el, 1, 80);
    expect(onPageChange).toHaveBeenCalledWith(1);
  });

  /**
   * The backstop. A capture taken away without a pointerup or a pointercancel
   * is the one end this component cannot see coming, and leaving the track
   * mid-page with snapping off is the state nothing else can recover from.
   */
  it("settles and restores snapping when capture is lost with no release", () => {
    const { el, onPageChange } = mountTrack(3);
    down(el, 1, 200);
    move(el, 1, 40);

    fireEvent.lostPointerCapture(el, { pointerId: 1, pointerType: "touch", bubbles: true });

    expect(el.style.scrollSnapType).toBe("");
    expect(onPageChange).toHaveBeenCalledWith(1);
  });

  /** A press that never crossed the threshold is a tap on whatever is inside the page. */
  it("reports nothing for a press that never became a drag", () => {
    const { el, writes, onPageChange } = mountTrack(3);
    down(el, 1, 200);
    move(el, 1, 198);
    up(el, 1, 198);

    expect(writes).toEqual([]);
    expect(onPageChange).not.toHaveBeenCalled();
    expect(el.style.scrollSnapType).toBe("");
  });
});
