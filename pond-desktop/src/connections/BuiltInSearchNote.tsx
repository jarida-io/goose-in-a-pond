// A catalogue entry the built-in web search already covers: why it is not offered, and the way to
// the built-in search instead. Shared by both Extensions pages.

import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { BuiltInSearchState } from "../api/types";
import { WebSearchPanel } from "./WebSearchPanel";

/** The one-line reason a marketplace card shows in place of Install. */
function builtInSearchReason(state: BuiltInSearchState): string {
  switch (state) {
    case "on": return "extensions.builtInOn";
    case "needs_key": return "extensions.builtInNeedsKey";
    case "knowledge_off": return "extensions.builtInKnowledgeOff";
  }
}

/**
 * The reason line, and with no key stored a "Turn on web search" button that opens the web search
 * panel in place, so the household adds its key to the built-in search rather than an extension.
 */
export function BuiltInSearchNote({ state, className }: { state: BuiltInSearchState; className: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <>
      <p className={className}>{t(builtInSearchReason(state))}</p>
      {state === "needs_key" && (
        <button
          type="button"
          className="mrow__btn"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
        >
          {t(open ? "extensions.hideWebSearch" : "extensions.turnOnWebSearch")}
        </button>
      )}
      {open && <WebSearchPanel />}
    </>
  );
}
