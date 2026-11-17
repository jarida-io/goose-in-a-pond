// Home's one open set of questions, with its plan or chat as its title; nothing while there is none.

import { QuestionsPanel } from "./QuestionsPanel";
import { RefusalContext, useSaid } from "./useLoaded";
import { Said } from "./MyGooseParts";
import type { Variant } from "./myGooseWords";

// A pond without the question routes must not close the rest of Home.
const REFUSE_NOTHING = () => {};

export function HomeQuestions({ variant, onSeePlans }: { variant: Variant; onSeePlans: () => void }) {
  const { said, say } = useSaid();
  return (
    <RefusalContext.Provider value={REFUSE_NOTHING}>
      <QuestionsPanel variant={variant} say={say} onSeePlans={onSeePlans} place="home" />
      <Said text={said} />
    </RefusalContext.Provider>
  );
}
