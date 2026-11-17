// My Goose: the member's plans and what Goose made for them, its questions, what it knows about
// them, its specialists and its activity. Both shells draw this; `variant` sets the density.

import { useCallback, useEffect, useRef, useState } from "react";
import { CloudOff, Smartphone } from "lucide-react";
import { InkEmptyState, InkTabs } from "@jarida/ink/react";
import "./mygoose.css";
import type { Anchor, DeliverableSummary } from "../api/types";
import {
  MY_GOOSE_INTRO,
  MY_GOOSE_TITLE,
  NOT_SERVED_BODY,
  NOT_SERVED_TITLE,
  PERSONAL_BODY,
  PERSONAL_TITLE,
  TABS,
  TABS_LABEL,
  type MyGooseTab,
  type Variant,
} from "./myGooseWords";
import { RefusalContext, useSaid, type Refusal } from "./useLoaded";
import { Said } from "./MyGooseParts";
import { PlansPanel } from "./PlansPanel";
import { AnchorPage } from "./AnchorPage";
import { DeliverableView } from "./DeliverableView";
import { QuestionsPanel } from "./QuestionsPanel";
import { AboutYouPanel } from "./AboutYouPanel";
import { SpecialistsPanel } from "./SpecialistsPanel";
import { ActivityPanel } from "./ActivityPanel";
import { CardDeck } from "./CardDeck";

type Open =
  | { view: "home" }
  | { view: "anchor"; anchorId: string }
  | { view: "deliverable"; anchorId: string; anchorTitle: string; deliverableId: string };

export function MyGooseScreen({ variant, onAsk }: { variant: Variant; onAsk?: (prompt: string) => void }) {
  const [tab, setTab] = useState<MyGooseTab>("plans");
  const [open, setOpen] = useState<Open>({ view: "home" });
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const { said, say } = useSaid();
  const headingRef = useRef<HTMLHeadingElement>(null);
  const wasOpen = useRef(false);
  const anchorTitles = useRef(new Map<string, string>());

  const refuse = useCallback((why: Refusal) => setRefusal((current) => current ?? why), []);

  // Back on the home view, focus returns to its heading rather than the top of the document.
  useEffect(() => {
    if (wasOpen.current && open.view === "home") headingRef.current?.focus();
    wasOpen.current = open.view !== "home";
  }, [open.view]);

  function openAnchor(anchor: Anchor) {
    anchorTitles.current.set(anchor.id, anchor.title);
    setOpen({ view: "anchor", anchorId: anchor.id });
  }

  function openDeliverable(anchorId: string, deliverable: DeliverableSummary) {
    setOpen({
      view: "deliverable",
      anchorId,
      anchorTitle: anchorTitles.current.get(anchorId) ?? MY_GOOSE_TITLE,
      deliverableId: deliverable.id,
    });
  }

  if (refusal) {
    return (
      <div className={`mg mg--${variant}`}>
        <InkEmptyState
          icon={refusal === "unknown_caller" ? <Smartphone size={32} aria-hidden="true" /> : <CloudOff size={32} aria-hidden="true" />}
          title={refusal === "unknown_caller" ? PERSONAL_TITLE : NOT_SERVED_TITLE}
          className="mg-closed"
        >
          {refusal === "unknown_caller" ? PERSONAL_BODY : NOT_SERVED_BODY}
        </InkEmptyState>
      </div>
    );
  }

  return (
    <RefusalContext.Provider value={refuse}>
      <div className={`mg mg--${variant}`}>
        {open.view === "deliverable" ? (
          <DeliverableView
            key={open.deliverableId}
            id={open.deliverableId}
            anchorTitle={open.anchorTitle}
            variant={variant}
            say={say}
            onBack={() => setOpen({ view: "anchor", anchorId: open.anchorId })}
          />
        ) : null}
        {open.view === "anchor" ? (
          <AnchorPage
            key={open.anchorId}
            id={open.anchorId}
            variant={variant}
            say={say}
            onBack={() => setOpen({ view: "home" })}
            onOpenDeliverable={(d) => openDeliverable(open.anchorId, d)}
          />
        ) : null}
        {open.view === "home" ? (
          <>
            <header className="mg__head">
              <h1 ref={headingRef} className="mg__title" tabIndex={-1}>
                {MY_GOOSE_TITLE}
              </h1>
              <p className="mg__intro">{MY_GOOSE_INTRO}</p>
            </header>
            <CardDeck variant={variant} place="mygoose" onAsk={onAsk} />
            <InkTabs label={TABS_LABEL} value={tab} tabs={TABS} onChange={setTab} className="mg-tabs" />
            {tab === "plans" ? (
              <PlansPanel variant={variant} say={say} onOpen={openAnchor} onAnswerQuestions={() => setTab("questions")} />
            ) : null}
            {tab === "questions" ? <QuestionsPanel variant={variant} say={say} onSeePlans={() => setTab("plans")} /> : null}
            {tab === "about" ? <AboutYouPanel variant={variant} say={say} /> : null}
            {tab === "specialists" ? <SpecialistsPanel variant={variant} say={say} /> : null}
            {tab === "activity" ? <ActivityPanel variant={variant} say={say} /> : null}
          </>
        ) : null}
        <Said text={said} />
      </div>
    </RefusalContext.Provider>
  );
}
