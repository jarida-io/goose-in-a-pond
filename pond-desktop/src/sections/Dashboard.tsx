// Home's classic-surface routing; the screen is `hub/views/DashboardGrid`. Still `Dashboard` on the
// `dashboard` section id: it's persisted as `giap-section`, so renaming it strands anyone reopening there.

import { useAppState, useAppDispatch } from "../state/useAppContext";
import { DashboardGrid } from "../hub/views/DashboardGrid";
import { sendTurn } from "../state/chatRunStore";

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      variant="classic"
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
