// ────────────────────────────────────────────────────────────
// Home — what needs me on the left, what is on to the right of it.
//
// One component, rendered by BOTH surfaces: `hub/views/Home.tsx` (touch panel)
// and `sections/Dashboard.tsx` (desktop). They were already deliberate twins,
// so a redesign landing on one of them would give a household two different
// Homes depending on how they got in. The pages are identical on both; the
// extra room on the desktop widens the widgets rather than adding any, which
// is WidgetTrack's single container query and not a breakpoint here.
//
// THE SHAPE. A fixed 396px column that asks, and a paged track that reports.
// The asking column never pages and never scrolls: one suggestion is open, the
// rest are a peek and a count. The track is the household's own arrangement,
// held in `state/dashboardLayout.ts` — which page, which order, which size.
//
// GONE, and both deliberately:
//
//   SEARCH and the ROOM FILTER. They existed to reach a device kept off Home
//   on purpose. That reach is now the Devices screen the empty state already
//   points at, and a results list has nowhere honest to go in a layout that is
//   396px of prose beside a track of paged cards — it would either cover the
//   suggestion or reflow the pages the household arranged.
//
//   THE CAMERA, SCENE, ROUTINE and TO-DO cards. See the catalogue's own header
//   for why each one could not be made true.
//
// What is deliberately NOT here: any card backed by data the pond does not
// have, and any control labelled with a verb it cannot perform.
// ────────────────────────────────────────────────────────────

import { Fragment, useState, type ReactElement, type ReactNode } from "react";
import { InkButton, InkSegmented, InkSheet, InkStack, InkText } from "@jarida/ink/react";
import { HubIco, micEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { SuggestionQueue } from "../primitives/SuggestionQueue";
import { WeatherWidget } from "../primitives/WeatherWidget";
import { HomeStatusBar } from "./HomeStatusBar";
import { HomeControlsCard } from "./widgets/HomeControlsCard";
import { MediaCard } from "./widgets/MediaCard";
import { WidgetTrack } from "./widgets/WidgetTrack";
import { AddWidgetButton, WidgetFrame } from "./widgets/WidgetFrame";
import { useHomeData } from "../state/hubDataStore";
import { homeLine } from "../state/homeLine";
import { greetingForHour, useNow } from "../state/useNow";
import {
  CARDS,
  MAX_PAGES,
  hideCard,
  moveCard,
  moveCardToPage,
  placedCards,
  resetLayout,
  setCardSize,
  showCard,
  useDashboardLayout,
  type CardId,
  type CardSize,
  type PlacedCard,
} from "../state/dashboardLayout";
import type { GuiSection } from "../../desktopState";
import "./dashboard-grid.css";

/** Tiles a device card shows at each size. Beyond six it is a list, not a glance. */
const TILE_LIMIT: Record<CardSize, number> = { s: 2, m: 4, l: 6 };

export interface DashboardGridProps {
  /**
   * Where the empty states send people. Typed as `GuiSection` rather than
   * `string` on purpose: the first draft used `string` and pointed two cards at
   * "routines" and "cameras", neither of which is a section. The union caught
   * it; a looser type would have shipped two buttons that navigate nowhere.
   */
  onNavigate: (section: GuiSection) => void;
  /** Starts a voice turn. The surfaces reach voice mode differently. */
  onTalk: () => void;
  /** The chat session the suggestions belong to. Null before one is opened. */
  sessionId: string | null;
}

export function DashboardGrid({ onNavigate, onTalk, sessionId }: DashboardGridProps): ReactElement {
  const home = useHomeData();
  const now = useNow();
  const layout = useDashboardLayout();

  // Two flags, not one. `arranging` is the frames' toolbars; `sheetOpen` is the
  // panel. Turning Arrange on opens both, but closing the sheet leaves the
  // toolbars up, because the arrows on the widgets themselves are the faster
  // way to reorder once the household can see what they are moving.
  const [arranging, setArranging] = useState(false);
  const [sheetOpen, setSheetOpen] = useState(false);
  const [requestedPage, setRequestedPage] = useState(0);

  const pageCount = layout.pages.length;
  // The store refuses to hide the last card left anywhere on Home, and the
  // sheet's own remove is disabled for it. The frame's x has to agree, or one
  // Arrange session shows two controls of the same name disagreeing about
  // whether the act is available and one of them silently does nothing.
  const placedCount = placedCards(layout).length;
  // Clamped at render rather than corrected in an effect: a page that stopped
  // existing (the household emptied it) must not leave the track pointing past
  // its own end for a frame.
  const page = Math.min(requestedPage, Math.max(0, pageCount - 1));

  function toggleArrange(): void {
    const next = !arranging;
    setArranging(next);
    setSheetOpen(next);
  }

  // Before the first load lands, `devices` is this file's demo house. Passing it
  // to homeLine would put a sentence about ten invented lamps on the screen for
  // as long as the load takes.
  const devices = home.devicesAreReal ? home.devices : [];

  // homeLine's last resort is a sentence about the sky, and there is no sky to
  // report when weather is off — the slice is zeroed, so it would read ", 0°
  // out.". The hour and the name are true without either.
  const quietLine =
    devices.length === 0 && !home.weatherEnabled
      ? `${greetingForHour(now.getHours())}, ${home.user}.`
      : homeLine({ user: home.user, devices, weather: home.weather, now });

  function renderCard(card: PlacedCard): ReactNode {
    switch (card.id) {
      case "weather":
        // Reused untouched rather than rebuilt to the design's literal
        // #60A5FA gradient. Its skies are contrast-tested at 4.5:1 by
        // weatherSky.test.ts against a scrim whose alpha that test proves is no
        // more opaque than it has to be; an untested gradient would trade a
        // measured floor for a mockup.
        return home.weatherEnabled ? (
          <WeatherWidget variant={card.size === "s" ? "card" : "hero"} />
        ) : (
          <div className="dash__gap">
            <span className="dash__gap-line">Set your location to see weather</span>
            <button type="button" className="dash__gap-btn" onClick={() => onNavigate("settings")}>
              Open Settings
            </button>
          </div>
        );

      case "devices":
        return (
          <HomeControlsCard
            limit={TILE_LIMIT[card.size]}
            onManageDevices={() => onNavigate("devices")}
          />
        );

      case "nowPlaying":
        return <MediaCard onOpenSettings={() => onNavigate("settings")} />;
    }
  }

  const pages: ReactNode[] = layout.pages.map((cards, pageIndex) => (
    <Fragment key={pageIndex}>
      {cards.map((card, i) => (
        <WidgetFrame
          key={card.id}
          title={titleOf(card.id)}
          size={card.size}
          arranging={arranging}
          onSize={(size) => setCardSize(card.id, size)}
          onRemove={() => hideCard(card.id)}
          onMoveUp={() => moveCard(card.id, -1)}
          onMoveDown={() => moveCard(card.id, 1)}
          canMoveUp={i > 0}
          canMoveDown={i < cards.length - 1}
          canRemove={placedCount > 1}
        >
          {renderCard(card)}
        </WidgetFrame>
      ))}

      {/* Only on the last page, and only when there is something to add. The
          design's label names cameras, scenes and to-do; this build has none of
          the three, so it names nothing. It reopens the sheet rather than
          picking a card on the household's behalf. */}
      {arranging && layout.hidden.length > 0 && pageIndex === pageCount - 1 && (
        <AddWidgetButton label="Add a widget" onClick={() => setSheetOpen(true)} />
      )}
    </Fragment>
  ));

  return (
    <div className="dash" data-arranging={arranging || undefined}>
      <HomeStatusBar
        arranging={arranging}
        onToggleArrange={toggleArrange}
        temp={home.weatherEnabled ? home.weather.temp : null}
        userName={home.user}
      />

      <div className="dash__body">
        <SuggestionQueue sessionId={sessionId} quietLine={quietLine} />
        <div className="dash__track">
          <WidgetTrack pages={pages} page={page} onPageChange={setRequestedPage} />
        </div>
      </div>

      {/* Floats over both columns. The track's pages carry 88px of bottom
          padding for exactly this, and the left column clears it by being
          vertically centred in a taller box than its own content. */}
      <div className="dash__dock">
        <button type="button" className="dash__voice" aria-label="Talk to Goose" onClick={onTalk}>
          {/* micEl, not HP_PATHS.mic: that entry is a compound sentinel string
              and renders nothing at all as a path. */}
          <HubIco d={micEl} size={26} color="#fff" sw={2} />
        </button>
        <button
          type="button"
          className="dash__chat"
          aria-label="Type to Goose"
          onClick={() => onNavigate("chat")}
        >
          <HubIco d={HP_PATHS.railChat} size={22} color="var(--color-text)" sw={2} />
        </button>
      </div>

      <ArrangeSheet
        open={sheetOpen}
        onClose={() => setSheetOpen(false)}
        onGoToPage={setRequestedPage}
      />
    </div>
  );
}

function titleOf(id: CardId): string {
  return CARDS.find((c) => c.id === id)?.title ?? id;
}

const SIZE_OPTIONS: { value: CardSize; label: string }[] = [
  { value: "s", label: "Small" },
  { value: "m", label: "Medium" },
  { value: "l", label: "Large" },
];

/**
 * Arranging Home.
 *
 * Buttons, not a drag. A drag is fewer taps for someone holding a mouse and
 * unusable for everyone else: DESIGN.md §6 makes keyboard operability a floor,
 * and a thumb dragging a card on a 480px-tall panel is a worse gesture than two
 * taps. Drag can be added over this later; it cannot replace it.
 *
 * "Move to page" is its own control rather than a side effect of the arrows.
 * With pages in the model, "up" past the top of a page could silently mean "the
 * previous page" — a move nobody asked for and nobody can see happen from the
 * sheet. Crossing a page is an explicit act with its own button and its own
 * name.
 */
function ArrangeSheet({
  open,
  onClose,
  onGoToPage,
}: {
  open: boolean;
  onClose: () => void;
  /** Follows a card that just crossed a page, so the household sees where it went. */
  onGoToPage: (page: number) => void;
}): ReactElement {
  const layout = useDashboardLayout();
  const spec = (id: CardId) => CARDS.find((c) => c.id === id);
  const placed = placedCards(layout);
  // The page an Available card is added to. Adding to page 1 from a sheet that
  // cannot show the track is the least surprising of the options, and the row
  // says which page it means.
  const addTo = 0;

  return (
    <InkSheet open={open} onClose={onClose} title="Arrange Home" side="right">
      <InkStack gap={4}>
        {layout.pages.map((cards, pageIndex) => (
          <section key={pageIndex}>
            <h3 className="dash__sheet-label">Page {pageIndex + 1}</h3>
            <ul className="dash__arrange">
              {cards.map((card, i) => {
                const c = spec(card.id);
                if (!c) return null;
                const others = layout.pages
                  .map((_, p) => p)
                  .filter((p) => p !== pageIndex)
                  // One page beyond the last, so a household can spread out
                  // without hunting for an "add a page" control.
                  .concat(layout.pages.length < MAX_PAGES ? [layout.pages.length] : []);
                return (
                  <li key={card.id} className="dash__arrange-row">
                    <div className="dash__arrange-text">
                      <InkText weight="semibold">{c.title}</InkText>
                      <InkText tone="secondary">{c.hint}</InkText>
                    </div>

                    <div className="dash__arrange-acts">
                      <button
                        type="button"
                        className="dash__icon-btn"
                        onClick={() => moveCard(card.id, -1)}
                        disabled={i === 0}
                        aria-label={`Move ${c.title} up`}
                      >
                        <HubIco
                          d={HP_PATHS.chevD}
                          size={18}
                          color="var(--color-text)"
                          sw={2.4}
                          className="dash__up"
                        />
                      </button>
                      <button
                        type="button"
                        className="dash__icon-btn"
                        onClick={() => moveCard(card.id, 1)}
                        disabled={i === cards.length - 1}
                        aria-label={`Move ${c.title} down`}
                      >
                        <HubIco d={HP_PATHS.chevD} size={18} color="var(--color-text)" sw={2.4} />
                      </button>
                      <button
                        type="button"
                        className="dash__icon-btn"
                        onClick={() => hideCard(card.id)}
                        disabled={placed.length === 1}
                        aria-label={`Remove ${c.title} from Home`}
                      >
                        <HubIco d={HP_PATHS.x} size={16} color="var(--color-text)" sw={2.4} />
                      </button>
                    </div>

                    {/* A radiogroup named for the card, so a segment reads
                        "Large" inside "Weather size" rather than a bare "L"
                        belonging to nothing. InkSegmented's `label` is the
                        group's accessible name and is never drawn. */}
                    <div className="dash__arrange-size">
                      <InkSegmented
                        label={`${c.title} size`}
                        value={card.size}
                        onChange={(size) => setCardSize(card.id, size)}
                        shape="pill"
                        options={SIZE_OPTIONS}
                      />
                    </div>

                    {others.length > 0 && (
                      <div className="dash__arrange-pages">
                        {others.map((p) => (
                          <button
                            key={p}
                            type="button"
                            className="dash__page-btn"
                            aria-label={`Move ${c.title} to page ${p + 1}`}
                            onClick={() => {
                              // Follow the card, not the button. Emptying this
                              // page drops it and shifts every later page down,
                              // so `p` is the page that was asked for and the
                              // return is the page the card is actually on.
                              const landedOn = moveCardToPage(card.id, p);
                              if (landedOn !== null) onGoToPage(landedOn);
                            }}
                          >
                            Page {p + 1}
                          </button>
                        ))}
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        ))}

        {layout.hidden.length > 0 && (
          <section>
            <h3 className="dash__sheet-label">Available</h3>
            <ul className="dash__arrange">
              {layout.hidden.map((id) => {
                const c = spec(id);
                if (!c) return null;
                return (
                  <li key={id} className="dash__arrange-row">
                    <div className="dash__arrange-text">
                      <InkText weight="semibold">{c.title}</InkText>
                      <InkText tone="secondary">{c.hint}</InkText>
                    </div>
                    <button
                      type="button"
                      className="dash__page-btn"
                      aria-label={`Add ${c.title} to page ${addTo + 1}`}
                      onClick={() => {
                        showCard(id, addTo);
                        onGoToPage(addTo);
                      }}
                    >
                      Add to page {addTo + 1}
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        )}

        <InkButton variant="quiet" onPress={resetLayout} block>
          Reset to the default Home
        </InkButton>
      </InkStack>
    </InkSheet>
  );
}
