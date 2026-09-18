// ────────────────────────────────────────────────────────────
// Home (hub surface).
//
// The screen itself now lives in `DashboardGrid`, rendered by this surface and
// by `sections/Dashboard.tsx`. The two were already deliberate twins — same
// primitives, same order, same header comment — and keeping the body in one
// place is what stops them drifting into two different Homes depending on how
// the household got in.
//
// What stays here is the part that is genuinely per-surface: how this shell
// routes, and how it reaches voice mode.
//
// The screen's own reasoning — why it is pared back, and why it is now
// arrangeable rather than fixed — is in `state/dashboardLayout.ts`.
// ────────────────────────────────────────────────────────────

import { useAppState, useAppDispatch } from "../../state/AppContext";
import type { GuiSection } from "../../desktopState";
import { DashboardGrid } from "./DashboardGrid";
import { sendTurn } from "../../state/chatRunStore";

interface HomeViewProps {
  /**
   * Take the household to a section of the app.
   *
   * A GuiSection and not a hub route, because that is what the screen below
   * emits, and the two vocabularies overlap only by accident. This used to be
   * typed `(route: string) => void`, which let the hub hand a section straight
   * to its own router: "devices" matched no hub screen, fell through to the
   * fallback, and re-rendered Home. The shell that supplies this is responsible
   * for the translation -- see `Hub`'s `navigate`.
   */
  go?: (section: GuiSection) => void;
}

export function HomeView({ go }: HomeViewProps) {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      // The hub routes through `go` when its shell supplied one, and falls back
      // to the shared section reducer when it did not.
      onNavigate={(section) =>
        go ? go(section) : dispatch({ type: "SET_SECTION", payload: section })
      }
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
      // Same two steps as the classic surface, routed the hub's way. See
      // `sections/Dashboard.tsx` for why the send comes first.
      onAsk={(prompt) => {
        sendTurn({ text: prompt });
        if (go) go("chat");
        else dispatch({ type: "SET_SECTION", payload: "chat" });
      }}
    />
  );
}
