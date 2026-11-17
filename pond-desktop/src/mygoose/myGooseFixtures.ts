// Wire-shaped My Goose rows for tests, so each test names only what it is about.

import type { Anchor, Deliverable, FactLine, IdeaCard } from "../api/types";

export function anchor(over: Partial<Anchor> = {}): Anchor {
  return {
    id: "a-1",
    kind: "trip",
    title: "Diani trip",
    state: "active",
    starts_at: null,
    ends_at: null,
    place: null,
    facts: [],
    agent: "trip",
    origin: "you",
    why: "",
    confirmed_at: "2026-11-10T08:00:00Z",
    ended_at: null,
    created_at: "2026-11-10T08:00:00Z",
    updated_at: "2026-11-10T08:00:00Z",
    mine: true,
    shared_by: null,
    shares: [],
    deliverables: [],
    open_questions: 0,
    job: null,
    ...over,
  };
}

export function deliverable(over: Partial<Deliverable> = {}): Deliverable {
  return {
    id: "d-1",
    anchor_id: "a-1",
    anchor_title: "Diani trip",
    kind: "plan",
    slot: "plan",
    title: "Plan for Diani trip",
    state: "ready",
    hook: "Four days by the sea",
    body: "# Plan for Diani trip\n\n## Day one\n\nTake the [SGR train](https://metickets.krc.co.ke/) to Mombasa.\n",
    facts: [{ label: "Budget", value: "KSh 80,000", source: "your answer" }],
    sources: [{ title: "Madaraka Express", url: "https://metickets.krc.co.ke/" }],
    objections: [],
    changed: "",
    version: 1,
    task_id: "engine-1",
    refresh_at: null,
    unseen: false,
    mine: true,
    created_at: "2026-11-12T08:00:00Z",
    updated_at: "2026-11-12T08:00:00Z",
    job: null,
    ...over,
  };
}

export function factLine(over: Partial<FactLine> = {}): FactLine {
  return {
    id: "f-1",
    category: "food",
    fact: "Vegetarian",
    confidence: "likely",
    since: null,
    source: { kind: "memory", ref: "m-1" },
    tools: [],
    pinned: false,
    score: 2,
    used: 0,
    state: "active",
    created_at: "2026-11-10T08:00:00Z",
    updated_at: "2026-11-10T08:00:00Z",
    ...over,
  };
}

export function ideaCard(over: Partial<IdeaCard> = {}): IdeaCard {
  return {
    id: "c-1",
    kind: "do",
    title: "Walk the Karura Forest waterfall trail",
    why: { text: "You said you want more time outdoors on weekends.", source: { kind: "interest", label: "Hiking" } },
    when: "Saturday 9 am",
    where: "Karura Forest",
    cost: "KSh 600 entry",
    effort: null,
    action: { kind: "todo", label: "Add to-do", text: "Walk the Karura waterfall trail" },
    sources: [
      { kind: "web", title: "Karura Forest: visiting", url: "https://www.karurafriends.org/visit" },
      { kind: "interest", title: "Hiking" },
    ],
    notes: { found: "The waterfall trail is about 5 km.", next: "Check the weather on Friday.", caveats: "Entry prices may have changed." },
    anchor: null,
    state: "new",
    batch_id: "b-1",
    created_at: "2026-11-17T06:00:00Z",
    fades_at: "2026-11-18T18:00:00Z",
    ...over,
  };
}
