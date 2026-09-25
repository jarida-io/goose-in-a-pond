// Home, shared by `hub/views/Home.tsx` (panel) and `sections/Dashboard.tsx` (desktop) so the two
// can't drift. Search reaches devices kept off Home; the layout lives in `state/dashboardLayout.ts`.

import { useMemo, useState } from "react";
import { Mic, Pencil, Search as SearchIcon, X } from "lucide-react";
import {
  InkBudget,
  InkButton,
  InkCard,
  InkInput,
  InkSegmented,
  InkSheet,
  InkStack,
  InkText,
} from "@jarida/ink/react";
import { DeviceTile } from "../primitives/DeviceTile";
import { WeatherWidget } from "../primitives/WeatherWidget";
import { NowPlaying } from "../primitives/NowPlaying";
import { Suggestion } from "../primitives/Suggestion";
import { useHomeData, useRoutines } from "../state/hubDataStore";
import { homeLine } from "../state/homeLine";
import { formatHubDate, greetingForHour, useNow } from "../state/useNow";
import {
  CARDS,
  hideCard,
  moveCard,
  resetLayout,
  showCard,
  useDashboardLayout,
  type CardId,
} from "../state/dashboardLayout";
import type { DeviceData } from "../data/mockHome";
import type { GuiSection } from "../../desktopState";
import "./dashboard-grid.css";

/** Devices shown before the household has narrowed anything. More than this is a list, not a glance. */
const GLANCE_LIMIT = 8;

/** At or below this many devices the report cards spread out; four is one tile row at every size. */
const SPARSE_LIMIT = 4;

/** The room filter's "everything" option. Not a room id, so it cannot collide with one. */
const ALL_ROOMS = "__all__";

export interface DashboardGridProps {
  /** Where the empty state sends people. */
  onNavigate: (section: GuiSection) => void;
  /** Starts a voice turn. The surfaces reach voice mode differently. */
  onTalk: () => void;
  /** The chat session the suggestion belongs to. Null before one is opened. */
  sessionId: string | null;
}

export function DashboardGrid({ onNavigate, onTalk, sessionId }: DashboardGridProps) {
  const home = useHomeData();
  const now = useNow();
  const layout = useDashboardLayout();

  const [query, setQuery] = useState("");
  const [room, setRoom] = useState<string>(ALL_ROOMS);
  const [editing, setEditing] = useState(false);

  const searching = query.trim().length > 0;

  // Only rooms holding a device: an option that always yields nothing looks broken.
  const rooms = useMemo(() => {
    const populated = new Set(home.devices.map((d) => d.room).filter(Boolean));
    return home.rooms.filter((r) => populated.has(r.name) || populated.has(r.id));
  }, [home.rooms, home.devices]);

  const devices = useMemo(
    () => filterDevices(home.devices, query, searching ? ALL_ROOMS : room),
    [home.devices, query, room, searching],
  );

  const greeting = greetingForHour(now.getHours());

  // "Few", not "none": two lamps leave a wide devices card as empty as zero do.
  const sparse = home.devices.length <= SPARSE_LIMIT;
  const playing = home.nowPlaying.connected && home.nowPlaying.playing;
  const line = homeLine({
    user: home.user,
    devices: home.devices,
    weather: home.weather,
    now,
  });

  return (
    // Two raised surfaces per screen (DESIGN.md §3); InkBudget warns in development when a third mounts.
    <InkBudget max={2}>
      <div className="dash" data-editing={editing || undefined}>
        <header className="dash__head">
          <div className="dash__greet-block">
            <h1 className="dash__greet">
              {greeting}, <span>{home.user}</span>
            </h1>
            <p className="dash__sub">
              {formatHubDate(now)} · {home.weather.cond}, {home.weather.temp}°
            </p>
          </div>

          {/*
            No `aria-label` here: InkButton does not forward it. The button's
            accessible name is the text below, which is why the small-panel rule
            clips that text rather than removing it.
          */}
          <InkButton variant="quiet" onPress={() => setEditing(true)}>
            <Pencil size={18} strokeWidth={2.2} aria-hidden="true" />
            <span className="dash__btn-label">Arrange</span>
          </InkButton>
        </header>

        {/*
          Chrome, so it stays on a hairline and never takes the ink edge
          (DESIGN.md §3, "the edge marks content, never chrome"). If the filters
          start shouting, nothing on the page is loud any more.
        */}
        <div className="dash__toolbar" role="search">
          <div className="dash__search">
            <InkInput
              label="Search your home"
              placeholder="Search devices, rooms, scenes"
              value={query}
              onChange={setQuery}
              type="search"
              lead={<SearchIcon size={18} strokeWidth={2.2} aria-hidden="true" />}
              trail={
                query ? (
                  <button
                    type="button"
                    className="dash__clear"
                    onClick={() => setQuery("")}
                    aria-label="Clear search"
                  >
                    <X size={16} strokeWidth={2.4} aria-hidden="true" />
                  </button>
                ) : undefined
              }
            />
          </div>

          {/*
            Hidden while searching, deliberately: a search is a question about
            the whole house, and leaving a room filter applied to it silently
            hides matches the household can see are missing.
          */}
          {!searching && rooms.length > 1 && (
            <InkSegmented
              label="Room"
              value={room}
              onChange={setRoom}
              shape="pill"
              options={[
                { value: ALL_ROOMS, label: "All" },
                ...rooms.map((r) => ({ value: r.name, label: r.name })),
              ]}
            />
          )}
        </div>

        <div className="dash__grid">
          {layout.order.map((id) => (
            <DashCard
              key={id}
              id={id}
              devices={devices}
              playing={playing}
              sparse={sparse}
              line={line}
              searching={searching}
              query={query}
              sessionId={sessionId}
              onNavigate={onNavigate}
            />
          ))}
        </div>

        {/* Last, because it is how you answer everything above it. */}
        <button className="dash__talk" onClick={onTalk}>
          <Mic size={16} strokeWidth={2.2} aria-hidden="true" />
          Start talking
        </button>

        <ArrangeSheet open={editing} onClose={() => setEditing(false)} />
      </div>
    </InkBudget>
  );
}

/** Case-insensitive substring match on name, room and kind; never fuzzy, as a near-miss picks the wrong lamp. */
function filterDevices(devices: DeviceData[], query: string, room: string): DeviceData[] {
  const q = query.trim().toLowerCase();
  return devices.filter((d) => {
    if (room !== ALL_ROOMS && d.room !== room) return false;
    if (!q) return true;
    return (
      d.name.toLowerCase().includes(q) ||
      (d.room ?? "").toLowerCase().includes(q) ||
      d.kind.toLowerCase().includes(q)
    );
  });
}

interface DashCardProps {
  id: CardId;
  devices: DeviceData[];
  /** Something is actually playing, so the music card earns its width. */
  playing: boolean;
  /** Few or no devices — the report cards spread into the space instead. */
  sparse: boolean;
  /** The one sentence about this house, computed where the data lives. */
  line: string;
  searching: boolean;
  query: string;
  sessionId: string | null;
  onNavigate: (section: GuiSection) => void;
}

/** One card; every branch is backed by `HomeData` the pond actually populates. */
function DashCard({ id, devices, playing, sparse, line, searching, query, sessionId, onNavigate }: DashCardProps) {
  switch (id) {
    case "suggestion":
      // The only card that asks, and one of the two that may spend the offset.
      return searching ? null : (
        <section className="dash__cell dash__cell--wide">
          <Suggestion
            sessionId={sessionId}
            quiet={<p className="dash__line">{line}</p>}
          />
        </section>
      );

    case "devices":
      return (
        <section className="dash__cell dash__cell--wide" aria-labelledby="dash-devices">
          <h2 className="dash__label" id="dash-devices">
            {searching ? `Matching "${query.trim()}"` : "Devices"}
          </h2>
          {devices.length > 0 ? (
            <div className="dash__tiles">
              {devices.slice(0, searching ? devices.length : GLANCE_LIMIT).map((d) => (
                <DeviceTile key={d.id} device={d} />
              ))}
            </div>
          ) : searching ? (
            // Not an error, and no offer to fix it: the household knows what they typed.
            <InkCard raised={false}>
              <InkText>Nothing here matches that. Try a room, or part of a name.</InkText>
            </InkCard>
          ) : (
            <button className="dash__empty" onClick={() => onNavigate("devices")}>
              Add your first device
            </button>
          )}
        </section>
      );

    case "weather":
      // With few devices the weather takes the spare width: it is the one card that is always true.
      return searching ? null : (
        <section className={`dash__cell${sparse ? " dash__cell--wide" : ""}`}>
          <WeatherWidget />
        </section>
      );

    case "nowPlaying":
      // Wide only while something plays: form carries data (DESIGN.md §3).
      return searching ? null : (
        <section
          className={`dash__cell${playing ? " dash__cell--wide" : ""}`}
          data-playing={playing || undefined}
        >
          <NowPlaying variant="tile" />
        </section>
      );

    // Cards the household can add.
    case "scenes":
    case "cameras":
    case "routines":
    case "todos":
      return searching ? null : (
        <section className="dash__cell">
          <ExtraCard id={id} />
        </section>
      );

    default:
      return null;
  }
}

/** An addable card reporting from `HomeData`; with nothing to say it renders nothing, not an empty frame. */
function ExtraCard({ id }: { id: CardId }) {
  const home = useHomeData();
  const routines = useRoutines();

  if (id === "scenes") {
    if (home.scenes.length === 0) return null;
    return (
      <InkCard raised={false}>
        <InkStack gap={2}>
          <h2 className="dash__label">Scenes</h2>
          <div className="dash__chips">
            {home.scenes.map((sc) => (
              <span key={sc.id} className="dash__chip" data-on={sc.active || undefined}>
                {sc.name}
              </span>
            ))}
          </div>
        </InkStack>
      </InkCard>
    );
  }

  if (id === "cameras") {
    if (home.cameras.length === 0) return null;
    return (
      <InkCard raised={false}>
        <InkStack gap={1}>
          <h2 className="dash__label">Cameras</h2>
          <InkText>
            {home.cameras.length} {home.cameras.length === 1 ? "camera" : "cameras"}
          </InkText>
          <InkText tone="secondary">{home.cameras.map((c) => c.name).join(", ")}</InkText>
        </InkStack>
      </InkCard>
    );
  }

  if (id === "routines") {
    if (routines.length === 0) return null;
    return (
      <InkCard raised={false}>
        <InkStack gap={1}>
          <h2 className="dash__label">Routines</h2>
          {routines.slice(0, 3).map((r) => (
            <InkText key={r.id} tone="secondary">
              {r.name}
            </InkText>
          ))}
        </InkStack>
      </InkCard>
    );
  }

  if (id === "todos") {
    if (home.todos.length === 0) return null;
    return (
      <InkCard raised={false}>
        <InkStack gap={1}>
          <h2 className="dash__label">To-do</h2>
          {home.todos.slice(0, 4).map((t, i) => (
            <InkText key={i} tone="secondary">
              {typeof t === "string" ? t : (t as { text?: string }).text ?? ""}
            </InkText>
          ))}
        </InkStack>
      </InkCard>
    );
  }

  return null;
}

/** Arranging Home with move/show/hide buttons: drag alone fails keyboards (DESIGN.md §6) and thumbs. */
function ArrangeSheet({ open, onClose }: { open: boolean; onClose: () => void }) {
  const layout = useDashboardLayout();
  const spec = (id: CardId) => CARDS.find((c) => c.id === id);

  return (
    <InkSheet open={open} onClose={onClose} title="Arrange Home" side="right">
      <InkStack gap={4}>
        <section>
          <h3 className="dash__sheet-label">On Home</h3>
          <ul className="dash__arrange">
            {layout.order.map((id, i) => {
              const c = spec(id);
              if (!c) return null;
              return (
                <li key={id} className="dash__arrange-row">
                  <div className="dash__arrange-text">
                    <InkText weight="semibold">{c.title}</InkText>
                    <InkText tone="secondary">{c.hint}</InkText>
                  </div>
                  <div className="dash__arrange-acts">
                    <button
                      className="dash__icon-btn"
                      onClick={() => moveCard(id, -1)}
                      disabled={i === 0}
                      aria-label={`Move ${c.title} up`}
                    >
                      ↑
                    </button>
                    <button
                      className="dash__icon-btn"
                      onClick={() => moveCard(id, 1)}
                      disabled={i === layout.order.length - 1}
                      aria-label={`Move ${c.title} down`}
                    >
                      ↓
                    </button>
                    <button
                      className="dash__icon-btn"
                      onClick={() => hideCard(id)}
                      disabled={layout.order.length === 1}
                      aria-label={`Remove ${c.title} from Home`}
                    >
                      <X size={16} strokeWidth={2.4} aria-hidden="true" />
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        </section>

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
                    <InkButton variant="quiet" onPress={() => showCard(id)}>
                      Add
                    </InkButton>
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
