// ────────────────────────────────────────────────────────────
// useSuggestedPrompts — the chat composer's chips, from the suggestion engine.
//
// Both chat surfaces shipped a hardcoded array. `ChatHub`'s five were the
// worse pair, because three of them asserted hardware: "Lock everything",
// "Bedroom to 67" and "Show the driveway" each name a device class a given
// pond may simply not own, and tapping one on a pond without it reaches a
// model that can only say so. That is the case DESIGN.md section 3 is about --
// an interface teaching people to expect something the data does not support.
//
// The engine already answers exactly this question, so the chips become its
// prompts. What a pond cannot do, it is not invited to ask for.
//
// The fallback is the honest part. When the engine offers nothing -- a pond
// with no devices, no accounts and no memories, which is every pond on its
// first evening -- the chips fall back to questions about the ASSISTANT rather
// than about the house. "What can you help me with?" is true of every pond
// that exists; "Lock everything" is not.
// ────────────────────────────────────────────────────────────

import { useEffect, useState } from "react";
import { api } from "../api/PondApiClient";

/**
 * Questions that need nothing but the assistant itself.
 *
 * Deliberately not about the house: no lock, no thermostat, no camera, no
 * calendar. A claim about hardware is the thing these replaced.
 */
const ABOUT_THE_ASSISTANT = [
  "What can you help me with?",
  "What do you remember about me?",
  "What can you see in this house?",
];

/**
 * The composer's chips.
 *
 * `sessionId` only sharpens the audience -- the fetch works without one, which
 * is the whole reason the suggestions route takes it as optional.
 */
export function useSuggestedPrompts(sessionId?: string | null): string[] {
  const [prompts, setPrompts] = useState<string[]>(ABOUT_THE_ASSISTANT);

  useEffect(() => {
    let cancelled = false;

    // The audience just changed, so whatever is on screen belongs to the last
    // one. Back to the neutral fallback NOW, not when the fetch lands: the
    // chips can be personal -- composed from one member's own notes -- and a
    // "New conversation" on a shared panel is exactly when the next person
    // walks up. This used to keep the previous prompts until something
    // replaced them, and an empty or failed fetch never did, so one member's
    // dentist question sat on an empty chat for a guest to read.
    setPrompts(ABOUT_THE_ASSISTANT);

    async function load(): Promise<void> {
      try {
        const list = await api.listSuggestions(sessionId ?? null);
        if (cancelled) return;
        const offered = (list.suggestions ?? []).map((s) => s.prompt);
        // An engine with nothing to say leaves the fallback in place rather
        // than emptying the row. A composer with no chips at all reads as a
        // loading state that never finishes.
        if (offered.length > 0) setPrompts(offered);
      } catch {
        // The fallback stays. These chips are a convenience, and a convenience
        // that renders an error is worse than one that renders something true.
      }
    }

    void load();
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  return prompts;
}
