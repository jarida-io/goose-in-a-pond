import { useAppDispatch } from "../state/useAppContext";
import { NotificationsView } from "../hub/views/Notifications";
import type { GuiSection } from "../desktopState";
import type { ScheduleRunNotification } from "../api/types";

export function Notifications() {
  const dispatch = useAppDispatch();

  // A run card's "View on Canvas" carries its run, so Canvas opens with the debrief it is being
  // asked for; a household notice's action names its own section and carries no run.
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
