// Connected accounts in the settings hub: the sections UI's panel, so there is one credential form.
import { DetailShell } from "./DetailShell";
import { useAppState } from "../../../state/AppContext";
import { ConnectionsPanel } from "../../../connections/ConnectionsPanel";

interface ConnectionsDetailProps {
  go: (route: string) => void;
}

export function ConnectionsDetail({ go }: ConnectionsDetailProps) {
  const { sessionId } = useAppState();
  return (
    <DetailShell
      title="Accounts"
      subtitle="Calendar and mail the pond can read"
      accent="#1F6F63"
      onBack={() => go("settings")}
    >
      <ConnectionsPanel sessionId={sessionId} />
    </DetailShell>
  );
}
