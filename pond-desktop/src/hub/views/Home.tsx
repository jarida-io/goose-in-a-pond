// Hub Home: the screen is `DashboardGrid` (shared with `sections/Dashboard.tsx`);
// only routing and the way into voice mode are per-surface.

import { useAppState, useAppDispatch } from "../../state/AppContext";
import { DashboardGrid } from "./DashboardGrid";

interface HomeViewProps {
  go?: (route: string) => void;
}

export function HomeView({ go }: HomeViewProps) {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      onNavigate={(section) =>
        go ? go(section) : dispatch({ type: "SET_SECTION", payload: section })
      }
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
    />
  );
}
