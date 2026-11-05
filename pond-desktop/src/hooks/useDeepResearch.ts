// Whether the pond offers deep research to this caller, and whether the composer is in research mode.
// Asked once per mount: the answer moves with household settings and sign-in, both of which remount.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import type { DeepResearchAvailability, ResearchAnswer } from "../api/types";

/** `hidden` covers off, an older pond, and any answer that could not be read; the item is not shown. */
export type DeepResearchAccess = "unknown" | "available" | "unidentified" | "hidden";

export type DeepResearchRefusal = "deep_research_off" | "deep_research_unidentified";

export interface UseDeepResearch {
  access: DeepResearchAccess;
  /** The composer is in research mode: the next message it sends is the question. */
  on: boolean;
  /** How long the answer should be. Kept while this composer is mounted: it is the person's habit. */
  answer: ResearchAnswer;
  setAnswer: (answer: ResearchAnswer) => void;
  /** No-op unless the pond offers it. */
  enter: () => void;
  leave: () => void;
  /** The pond refused a research turn; the menu follows its answer instead of the last read. */
  refused: (reason: DeepResearchRefusal) => void;
}

function accessOf(answer: DeepResearchAvailability | null): DeepResearchAccess {
  if (!answer) return "hidden";
  if (answer.available) return "available";
  return answer.reason === "unidentified" ? "unidentified" : "hidden";
}

export function useDeepResearch(serverOnline: boolean): UseDeepResearch {
  const [access, setAccess] = useState<DeepResearchAccess>("unknown");
  const [on, setOn] = useState(false);
  const [answer, setAnswer] = useState<ResearchAnswer>("auto");
  const accessRef = useRef(access);
  accessRef.current = access;

  useEffect(() => {
    if (!serverOnline) return;
    let live = true;
    // A partial api mock may lack the method; a sync throw reads as a failed request.
    Promise.resolve()
      .then(() => api.getDeepResearchAvailability())
      .then((answer) => {
        if (live) setAccess(accessOf(answer));
      })
      .catch(() => {
        if (live) setAccess("hidden");
      });
    return () => {
      live = false;
    };
  }, [serverOnline]);

  const enter = useCallback(() => {
    if (accessRef.current === "available") setOn(true);
  }, []);
  const leave = useCallback(() => setOn(false), []);
  const refused = useCallback((reason: DeepResearchRefusal) => {
    setAccess(reason === "deep_research_unidentified" ? "unidentified" : "hidden");
    setOn(false);
  }, []);

  return useMemo(
    () => ({ access, on, answer, setAnswer, enter, leave, refused }),
    [access, on, answer, enter, leave, refused],
  );
}
