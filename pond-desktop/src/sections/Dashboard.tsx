// ────────────────────────────────────────────────────────────
// Home (classic surface).
//
// The screen itself lives in `hub/views/DashboardGrid`, shared with
// `hub/views/Home.tsx`. See that file and `hub/state/dashboardLayout.ts` for
// the design reasoning; what remains here is this surface's routing.
//
// Still exported as `Dashboard` and still on the `dashboard` section id: that
// id is persisted in localStorage as `giap-section`, so renaming it would
// strand anyone whose app reopens on the screen they left. What people see is
// "Home"; what the router remembers is unchanged.
// ────────────────────────────────────────────────────────────

import { useAppState, useAppDispatch } from "../state/AppContext";
import { DashboardGrid } from "../hub/views/DashboardGrid";
import { sendTurn } from "../state/chatRunStore";

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      onNavigate={(section) => dispatch({ type: "SET_SECTION", payload: section })}
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
      // Send first, then navigate. `chatRunStore` is a module singleton that
      // deliberately outlives the component tree -- Chat already relies on that
      // to survive a section change mid-stream -- so the turn is already in
      // flight by the time the transcript mounts, and the household lands on
      // their own sentence with the reply arriving under it.
      onAsk={(prompt) => {
        sendTurn({ text: prompt });
        dispatch({ type: "SET_SECTION", payload: "chat" });
      }}
    />
  );
}
