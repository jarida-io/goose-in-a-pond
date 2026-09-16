// ────────────────────────────────────────────────────────────
// WidgetTrack — the pages of Home, and the gesture that moves between them.
//
// It renders one full-width column per entry in `pages` and pages between them.
// What is inside a page is the caller's business: this component decides where
// the track sits, never what it says.
//
// `page` is controlled because the household's own arrangement lives outside
// here. The arrange sheet moves a widget and then jumps the track to the page
// it just changed; if this component also kept a page number, the two would
// disagree the first time that happened.
//
// The gesture is why this is a component and not a CSS scroller. Snap alone
// gives you paging but not a flick, and a mandatory-snap container re-snaps
// every `scrollLeft` write, so a drag that follows the finger 1:1 cannot render
// a single intermediate frame while snapping is on. The drag therefore turns
// snap off, moves `scrollLeft` itself, and turns it back on before the release
// animates.
//
// Snapping is switched by writing the element's own inline style, not by
// rendering it from state. `pointermove` is a continuous-priority event, so a
// `setDragging(true)` in the same handler as the `scrollLeft` write has not
// committed when that write lands, the container is still mandatory-snap, and
// Chrome discards the write synchronously — the first frame of every drag went
// nowhere. The gesture needs that property at the moment it takes capture, so
// it sets it at the moment it takes capture. The resting value lives in the
// stylesheet, which makes clearing the inline one the way snapping comes back.
// ────────────────────────────────────────────────────────────

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type ReactElement,
  type ReactNode,
} from "react";
import "./widget-track.css";

/**
 * Travel, in px, that counts as a swipe regardless of where it ended.
 *
 * A swipe is judged on intent, not on whether it crossed half the panel: a
 * short, fast flick that let go 30px in still means "next page", and snapping
 * it back to where it started reads as the panel refusing the gesture.
 */
const FLICK_PX = 48;

/**
 * Travel, in px, before a press is treated as a drag at all.
 *
 * This is not a nicety, it is what keeps the widgets usable. Pointer capture
 * redirects the CLICK as well as the pointer events, so capturing on
 * `pointerdown` hands every tap inside the track to this element and the button
 * the finger was actually on never hears about it — a device tile that never
 * toggles, a transport that never plays. Capture is therefore taken on the
 * first move past this threshold and never on the press itself, so a tap is a
 * tap and a drag is still a drag.
 */
const DRAG_START_PX = 6;

export interface WidgetTrackProps {
  /** One entry per page, in the order the household arranged. Each entry is that page's already-framed widgets. */
  pages: ReactNode[];
  /** Which page is showing. Controlled, so the arrange sheet can jump to the page it just changed. */
  page: number;
  onPageChange: (page: number) => void;
}

/**
 * Read per call rather than captured once: the panel's motion setting can
 * change while the app is up, and a captured value would keep animating until
 * something remounted. Guarded because the headless DOMs the tests run in do
 * not all carry a real `matchMedia`.
 */
function prefersReducedMotion(): boolean {
  try {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
}

export function WidgetTrack({ pages, page, onPageChange }: WidgetTrackProps): ReactElement {
  const track = useRef<HTMLDivElement | null>(null);
  // The whole truth about the gesture, and the only truth: which pointer owns
  // it, where it began, and whether it has become a drag (see DRAG_START_PX).
  // `dragging` below is a render of this, never a second opinion about it — the
  // release used to ask the state instead and stranded the track whenever the
  // two disagreed.
  const start = useRef<{ pointerId: number; x: number; left: number; captured: boolean } | null>(
    null,
  );
  // The last page THIS component asked for. An external change — the arrange
  // sheet — has to scroll the track; an echo of our own emit must not, or a
  // drag that has just settled gets a second scrollTo fighting the first.
  const emitted = useRef(page);
  const [dragging, setDragging] = useState(false);
  // Set by the end of the gesture and consumed one render later, once
  // `dragging: false` has actually painted. See `endGesture`.
  const [settleTo, setSettleTo] = useState<number | null>(null);

  const count = pages.length;

  // 1 rather than 0 when the track has no layout yet: every caller divides by
  // this, and a zero would hand `onPageChange` a NaN instead of a page number.
  const pageWidth = useCallback(() => {
    const width = track.current?.clientWidth ?? 0;
    return width > 0 ? width : 1;
  }, []);

  /** Moves the track only. Returns the page it clamped to, so callers can report it. */
  const scrollToPage = useCallback(
    (i: number): number => {
      const n = Math.max(0, Math.min(count - 1, i));
      track.current?.scrollTo?.({
        left: n * pageWidth(),
        behavior: prefersReducedMotion() ? "auto" : "smooth",
      });
      return n;
    },
    [count, pageWidth],
  );

  /** Reports a page the track has already reached, and records that we are the ones who said it. */
  const emit = useCallback(
    (n: number) => {
      emitted.current = n;
      onPageChange(n);
    },
    [onPageChange],
  );

  const goToPage = useCallback(
    (i: number) => {
      emit(scrollToPage(i));
    },
    [emit, scrollToPage],
  );

  // A page number that did not come from here — the arrange sheet jumping to
  // the page it changed. Scroll to it, and do not report it back: the caller
  // already knows, and echoing it would fight whatever it does next.
  useEffect(() => {
    if (page === emitted.current) return;
    emitted.current = page;
    scrollToPage(page);
  }, [page, scrollToPage]);

  const onScroll = useCallback(() => {
    // Ignored mid-drag: the drag writes `scrollLeft` directly, so every frame of
    // it would otherwise be reported as a page change. Asked of the ref as well
    // as the state, because the first write of a drag lands before the state
    // that describes it has rendered.
    if (!track.current || dragging || start.current?.captured) return;
    const p = Math.round(track.current.scrollLeft / pageWidth());
    if (p !== page) emit(p);
  }, [dragging, emit, page, pageWidth]);

  const onPointerDown = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    if (!track.current) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    // A second finger landing on a track that is already being dragged is not a
    // new gesture. It used to overwrite the origin with `captured: false`, which
    // made the first finger's release read as a tap and left the track parked
    // mid-page with snapping off. A press that has not become a drag has nothing
    // to protect, so that one is allowed to hand the track over rather than
    // holding it hostage until a pointerup that may never arrive.
    if (start.current?.captured) return;
    // Recorded, not captured. Nothing about this press is a drag yet.
    start.current = {
      pointerId: e.pointerId,
      x: e.clientX,
      left: track.current.scrollLeft,
      captured: false,
    };
  }, []);

  const onPointerMove = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    const s = start.current;
    if (!s || !track.current || e.pointerId !== s.pointerId) return;
    const dx = e.clientX - s.x;
    if (!s.captured) {
      if (Math.abs(dx) < DRAG_START_PX) return;
      s.captured = true;
      track.current.setPointerCapture?.(e.pointerId);
      // Before the write below, and by hand: see the header. Rendering this
      // from `dragging` puts it a commit later than the write that needs it.
      track.current.style.scrollSnapType = "none";
      setDragging(true);
    }
    // 1:1 and inverted — the page follows the finger, not the other way round.
    track.current.scrollLeft = s.left - dx;
  }, []);

  /**
   * Every way a gesture can end arrives here: the release, a cancel, and a
   * capture lost to something this component never hears about.
   *
   * `endX` is null when the end carries no position — a lost capture — in which
   * case the flick rescue is skipped rather than measured against a fabricated
   * origin, and the track settles to whichever page it is nearest.
   *
   * Unconditional and idempotent on purpose. Whatever else it decides, it
   * always clears the gesture, always restores snapping, and always reports the
   * drag as over; a second call with nothing left to end does no harm.
   */
  const endGesture = useCallback(
    (pointerId: number, endX: number | null) => {
      const s = start.current;
      // Another finger lifting is not the end of this drag.
      if (s && pointerId !== s.pointerId) return;

      start.current = null;
      // Clearing the inline value hands the property back to the stylesheet,
      // which is where mandatory snap lives.
      if (track.current) track.current.style.scrollSnapType = "";
      setDragging(false);

      // A press that never became a drag is a tap on whatever is inside the
      // page. Nothing to settle, and nothing to report.
      if (!s || !s.captured || !track.current) return;

      const w = pageWidth();
      const from = Math.round(s.left / w);
      const dx = endX === null ? 0 : endX - s.x;
      let target = Math.round(track.current.scrollLeft / w);
      if (Math.abs(dx) > FLICK_PX && target === from) {
        target = from + (dx < 0 ? 1 : -1);
      }
      // Recorded, not scrolled. Turning mandatory snap back on part-way through
      // a smooth scrollTo lets the container jump to a snap point mid-animation
      // — the exact fight the drag turned snapping off to avoid — so the
      // animation starts only once the track has been at rest for a render.
      // The effect below is the single place any settle runs, whichever of the
      // three ends got here.
      setSettleTo(target);
    },
    [pageWidth],
  );

  const onPointerEnd = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => endGesture(e.pointerId, e.clientX),
    [endGesture],
  );

  // The backstop. If capture goes away without a pointerup or pointercancel —
  // the browser taking it back, the node being moved — this is the only event
  // that still fires, and without it the track would sit mid-page with snapping
  // off and no gesture able to clear it. On an ordinary release it arrives
  // after the pointerup that already ended the gesture, and does nothing.
  const onLostCapture = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => endGesture(e.pointerId, null),
    [endGesture],
  );

  useEffect(() => {
    if (settleTo === null || dragging) return;
    setSettleTo(null);
    goToPage(settleTo);
  }, [dragging, goToPage, settleTo]);

  // The one property that renders from state, and the only one that can afford
  // to: a cursor a frame late is cosmetic. Snapping is not, which is why the
  // gesture writes that one itself.
  const trackStyle: CSSProperties = {
    cursor: dragging ? "grabbing" : "grab",
  };

  return (
    <>
      <div
        ref={track}
        className="wtrack"
        data-hook="widget-track"
        style={trackStyle}
        onScroll={onScroll}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
        onLostPointerCapture={onLostCapture}
      >
        {pages.map((contents, i) => (
          <div className="wtrack__page" key={i}>
            {contents}
          </div>
        ))}
      </div>

      {/* One page is not a set of pages. Nothing to choose between, so no dots. */}
      {count > 1 ? (
        <div className="wtrack__dots">
          {pages.map((_, i) => (
            <button
              key={i}
              type="button"
              className="wtrack__dot"
              aria-label={`Page ${i + 1} of ${count}`}
              aria-current={i === page ? "true" : undefined}
              onClick={() => goToPage(i)}
            >
              <span className="wtrack__dot-bar" />
            </button>
          ))}
        </div>
      ) : null}
    </>
  );
}
