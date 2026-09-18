import { useAppDispatch } from "../state/AppContext";
import { NotificationsView } from "../hub/views/Notifications";
import type { GuiSection } from "../desktopState";
import type { ScheduleRunNotification } from "../api/types";

export function Notifications() {
  const dispatch = useAppDispatch();

  // Every notification this feed can raise is built from a schedule run, so the
  // run is always here and Canvas always opens with the debrief it is being
  // asked for. It was not always: the fabricated security and camera items
  // carried an "Open device" action with no run behind it, and following one
  // landed the household on an empty Canvas with nothing to explain itself.
  function go(route: string, run?: ScheduleRunNotification) {
    if (route === "canvas" && run) {
      dispatch({ type: "SET_DEBRIEF_CONTEXT", payload: { type: "debrief", run } });
    }
    dispatch({ type: "SET_SECTION", payload: route as GuiSection });
  }

  return (
    <div className="screen screen--notifications">
      <NotificationsView go={go} />
    </div>
  );
}
