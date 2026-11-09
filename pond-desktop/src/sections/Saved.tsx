import { useAppDispatch } from "../state/useAppContext";
import { SavedScreen } from "../saved/SavedScreen";

export function Saved() {
  const dispatch = useAppDispatch();
  return (
    <div className="screen screen--saved">
      <SavedScreen variant="classic" onOpenChat={() => dispatch({ type: "SET_SECTION", payload: "chat" })} />
    </div>
  );
}
