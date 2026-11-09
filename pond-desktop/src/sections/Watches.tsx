import { useAppDispatch } from "../state/useAppContext";
import { WatchesScreen } from "../watches/WatchesScreen";

export function Watches() {
  const dispatch = useAppDispatch();
  return (
    <div className="screen screen--watches">
      <WatchesScreen variant="classic" onOpenChat={() => dispatch({ type: "SET_SECTION", payload: "chat" })} />
    </div>
  );
}
