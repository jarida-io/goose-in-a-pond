import { useAppState, useAppDispatch } from "./state/AppContext";
import { GuiMode } from "./modes/GuiMode";
import { VoiceMode } from "./modes/VoiceMode";
import { OnboardingWizard } from "./components/OnboardingWizard";
import { api } from "./api/PondApiClient";

export function App() {
  const { mode, needsOnboarding } = useAppState();
  const dispatch = useAppDispatch();

  // Show onboarding wizard when the backend reports the device is not yet onboarded.
  if (needsOnboarding) {
    return (
      <OnboardingWizard
        onComplete={async () => {
          try {
            await api.completeOnboarding();
          } catch (err) {
            console.warn("completeOnboarding failed (non-fatal):", err);
          }
          dispatch({ type: "SET_NEEDS_ONBOARDING", payload: false });
        }}
      />
    );
  }

  // Canvas mode uses a separate Tauri window (canvas.html / canvas-main.tsx).
  // The main window shows GUI or Voice.
  if (mode === "voice") return <VoiceMode />;
  return <GuiMode />;
}
