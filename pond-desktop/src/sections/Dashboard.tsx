// Home's classic-surface routing; the screen is `hub/views/DashboardGrid`. Still `Dashboard` on the
// `dashboard` section id: it's persisted as `giap-section`, so renaming it strands anyone reopening there.

import { useAppState, useAppDispatch } from "../state/AppContext";
import { DashboardGrid } from "../hub/views/DashboardGrid";

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      onNavigate={(section) => dispatch({ type: "SET_SECTION", payload: section })}
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
    />
  );
}
