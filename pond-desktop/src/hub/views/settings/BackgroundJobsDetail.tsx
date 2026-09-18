// ────────────────────────────────────────────────────────────
// Background jobs, on the hub.
//
// The list itself is `settings/BackgroundJobs`, shared with the classic
// surface. Only the shell differs: this one is a DetailShell with a back
// affordance, because the hub reaches its settings one screen at a time.
//
// `bare` because DetailShell already draws the heading and the subtitle; the
// card's own would be a second title saying the same thing.
// ────────────────────────────────────────────────────────────

import { DetailShell } from "./DetailShell";
import { BackgroundJobs } from "../../../settings/BackgroundJobs";

export function BackgroundJobsDetail({ go }: { go: (route: string) => void }) {
  return (
    <DetailShell
      title="Background jobs"
      subtitle="What the pond does while nobody is talking to it"
      onBack={() => go("settings")}
    >
      <BackgroundJobs bare />
    </DetailShell>
  );
}
