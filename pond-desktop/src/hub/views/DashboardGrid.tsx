
// Home, shared by `hub/views/Home.tsx` (panel) and `sections/Dashboard.tsx` (desktop) so the two
// can't drift. Search reaches devices kept off Home; the layout lives in `state/dashboardLayout.ts`.
import { Fragment, useState, type ReactElement, type ReactNode } from "react";
import { InkBudget, InkButton, InkSegmented, InkSheet, InkStack, InkText } from "@jarida/ink/react";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS, micEl } from "../primitives/icons";
import { SuggestionChips } from "../primitives/SuggestionChips";
import { SuggestionQueue } from "../primitives/SuggestionQueue";
import { WeatherWidget } from "../primitives/WeatherWidget";
import { HomeAlerts } from "./HomeAlerts";
import { HomeComposer } from "./HomeComposer";
import { HomeStatusBar } from "./HomeStatusBar";
import { HomeControlsCard } from "./widgets/HomeControlsCard";
import { MediaCard } from "./widgets/MediaCard";
import { RemoteAccessCard } from "./widgets/RemoteAccessCard";
import { TonightCard } from "./widgets/TonightCard";
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
  /** Where the empty state sends people. */
  onNavigate: (section: GuiSection) => void;
  /** Starts a voice turn. The surfaces reach voice mode differently. */
  onTalk: () => void;
  /** The chat session the suggestions belong to. Null before one is opened. */
  sessionId: string | null;
  /** Asks the pond and goes where the answer appears; without it the offers fold away. */
  onAsk?: (prompt: string) => void;
}

export function DashboardGrid({ onNavigate, onTalk, sessionId, onAsk }: DashboardGridProps): ReactElement {
  const home = useHomeData();
  const now = useNow();
  const layout = useDashboardLayout();

  // Separate flags: closing the sheet keeps the frames' toolbars, whose arrows reorder faster.
  const [arranging, setArranging] = useState(false);
  const [sheetOpen, setSheetOpen] = useState(false);
  const [requestedPage, setRequestedPage] = useState(0);

  const pageCount = layout.pages.length;
  // The store won't hide the last card on Home, so the frame's x must be disabled for it too.
  const placedCount = placedCards(layout).length;
  // The weather card already states the temperature; the header copy would duplicate it.
  const weatherPlaced = placedCards(layout).some((c) => c.id === "weather");
  // Clamped at render, not in an effect, so the track never points past its end for a frame.
  const page = Math.min(requestedPage, Math.max(0, pageCount - 1));

  function toggleArrange(): void {
    const next = !arranging;
    setArranging(next);
    setSheetOpen(next);
  }

  const devices = home.devicesAreReal ? home.devices : [];

  // homeLine falls back to the weather, which is zeroed when off (", 0° out."); greet instead.
  const quietLine =
    devices.length === 0 && !home.weatherEnabled
      ? `${greetingForHour(now.getHours())}, ${home.user}.`
      : homeLine({ user: home.user, devices, weather: home.weather, now });

  function renderCard(card: PlacedCard): ReactNode {
    switch (card.id) {
      case "weather":
        // Not the design's #60A5FA gradient: weatherSky.test.ts proves these skies reach 4.5:1.
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
        return <MediaCard size={card.size} onOpenSettings={() => onNavigate("settings")} />;

      case "tonight":
        return <TonightCard onNavigate={onNavigate} />;

      case "remoteAccess":
        return <RemoteAccessCard onNavigate={onNavigate} />;
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
    <InkBudget max={2} label="Home">
    <div className="dash" data-arranging={arranging || undefined}>
      <HomeStatusBar
        arranging={arranging}
        onToggleArrange={toggleArrange}
        temp={home.weatherEnabled ? home.weather.temp : null}
        hideTemp={weatherPlaced}
        userName={home.user}
        subtitle={quietLine}
      />

      {/* A plain stacking wrapper below 1101px (matches the three standing alone before this
          existed); at >=1101px it becomes the row that puts alerts beside the composer
          instead of under it, in the empty top-right the composer alone left behind.
          The composer and its chips stack as one left column against that row; the
          chips are SuggestionQueue's own offers, repeated here (not reused) because
          they need to sit under the composer, not in the aside column it renders in
          below 1101px -- see SuggestionChips.tsx's own header comment. */}
      <div className="dash__top-row">
        <div className="dash__top-left">
          <HomeComposer onAsk={onAsk} onTalk={onTalk} />
          <SuggestionChips sessionId={sessionId} onAsk={onAsk} />
        </div>
        <HomeAlerts onNavigate={onNavigate} />
      </div>

      <div className="dash__body">
        {/* The asking side: what the house said, what you could ask it, and the
            two ways to ask. One block, in flow.

            The dock used to float over the whole screen, positioned against the
            bottom edge of `.dash`. That is what put it 20px BELOW the fold on
            the 800x480 panel, where `.dash` carried a 520px minimum against a
            420px box -- the panel could not reach its own microphone. It also
            landed it on top of the widget track mid-scroll, and, on a 900px
            desktop window, 500px below the questions it belongs to.

            In flow under the column it is none of those things, at any height,
            and the failure mode it had is not expressible any more. */}
        <div className="dash__aside">
          <SuggestionQueue sessionId={sessionId} onAsk={onAsk} />
          <div className="dash__dock">
            <button type="button" className="dash__voice" aria-label="Talk to Goose" onClick={onTalk}>
              {/* micEl, not HP_PATHS.mic: that entry is a compound sentinel
                  string and renders nothing at all as a path. */}
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
        </div>

        <div className="dash__track">
          <WidgetTrack pages={pages} page={page} onPageChange={setRequestedPage} />
        </div>
      </div>

      <ArrangeSheet
        open={sheetOpen}
        onClose={() => setSheetOpen(false)}
        onGoToPage={setRequestedPage}
      />
    </div>
    </InkBudget>
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
 * Arranging Home with buttons, not drag (DESIGN.md §6 keyboard floor; drag may only be added on
 * top). Arrows never cross pages: moving to a page is its own explicit control.
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
  // Available cards are added to page 1; the row says so.
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
                  // Plus one new page past the last, instead of an "add a page" control.
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
                              // Follow the card, not `p`: an emptied page shifts later ones.
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
