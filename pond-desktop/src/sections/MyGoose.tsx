import { MyGooseScreen } from "../mygoose/MyGooseScreen";
import { useAppDispatch } from "../state/useAppContext";
import { sendTurn } from "../state/chatRunStore";

export function MyGoose() {
  const dispatch = useAppDispatch();
  return (
    <div className="screen screen--mygoose">
      <MyGooseScreen
        variant="classic"
        // Send first, then navigate, as in `sections/Dashboard.tsx` (which says why).
        onAsk={(prompt) => {
          sendTurn({ text: prompt });
          dispatch({ type: "SET_SECTION", payload: "chat" });
        }}
      />
    </div>
  );
}
