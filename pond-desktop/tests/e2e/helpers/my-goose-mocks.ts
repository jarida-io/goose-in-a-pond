import type { Page, Route } from "@playwright/test";

/** What the My Goose routes were asked to do, so a spec can check the body that was sent. */
export interface MyGooseCalls {
  confirmed: string[];
  stopped: Array<{ id: string; body: unknown }>;
  rejected: Array<{ id: string; body: unknown }>;
  answered: Array<{ id: string; body: unknown }>;
  seen: string[];
}

const now = "2026-11-17T08:00:00Z";

function anchorOf(over: Record<string, unknown>): Record<string, unknown> {
  return {
    kind: "trip",
    state: "active",
    starts_at: null,
    ends_at: null,
    place: null,
    facts: [],
    agent: null,
    origin: "you",
    why: "",
    confirmed_at: now,
    ended_at: null,
    created_at: now,
    updated_at: now,
    mine: true,
    shared_by: null,
    shares: [],
    deliverables: [],
    open_questions: 0,
    job: null,
    ...over,
  };
}

const PLAN_BODY = [
  "# Plan for Diani trip",
  "",
  "## Getting there",
  "",
  "Take the [Madaraka Express](https://metickets.krc.co.ke/) to Mombasa, then a matatu south to Diani.",
  "",
  "## Where to stay",
  "",
  "Three guesthouses near the beach fit a budget of KSh 80,000 for four nights.",
].join("\n");

/**
 * Mocks every `/api/v1/me` route the desktop's My Goose reads, holding state so an action shows
 * in the next read. `unknownCaller` answers every call the way a pond that cannot tell who is
 * asking does. Call after `mockAllApiRoutes`.
 */
export async function mockMyGoose(page: Page, opts: { unknownCaller?: boolean } = {}): Promise<MyGooseCalls> {
  const calls: MyGooseCalls = { confirmed: [], stopped: [], rejected: [], answered: [], seen: [] };

  const anchors = new Map<string, Record<string, unknown>>([
    [
      "a-summit",
      anchorOf({
        id: "a-summit",
        kind: "event",
        title: "AI Summit Nairobi",
        state: "candidate",
        starts_at: "2026-12-03T06:00:00Z",
        place: "KICC, Nairobi",
        why: "You said you are going on 3 December.",
        confirmed_at: null,
      }),
    ],
    [
      "a-diani",
      anchorOf({
        id: "a-diani",
        title: "Diani trip",
        starts_at: "2026-12-20T00:00:00Z",
        ends_at: "2026-12-24T20:59:00Z",
        place: "Diani",
        agent: "trip",
        facts: [{ label: "Travelling", value: "Four of us", source: "you said" }],
        deliverables: [
          { id: "d-plan", kind: "plan", slot: "plan", title: "Plan for Diani trip", state: "unsure", hook: "Four days by the sea", version: 1, changed: "", unseen: true },
          { id: "d-stays", kind: "shortlist", slot: "stays", title: "Stays in Diani", state: "making", hook: "", version: 0, changed: "", unseen: false },
        ],
        open_questions: 1,
        job: { kind: "deliverable", subject: "Stays in Diani", state: "running", rounds: 1 },
      }),
    ],
  ]);
  const questions: Array<Record<string, unknown>> = [
    {
      id: "q-budget",
      header: "Budget",
      text: "What is the budget for stays?",
      options: [
        { label: "Under KSh 50,000", description: "" },
        { label: "KSh 50,000 to 100,000", description: "" },
      ],
      multi: false,
    },
  ];

  const whole = (id: string) => {
    const anchor = anchors.get(id);
    if (!anchor) return null;
    const job = anchor.job as Record<string, unknown> | null;
    return {
      ...anchor,
      open_questions: id === "a-diani" ? questions.length : 0,
      questions: id === "a-diani" ? questions : [],
      job: job
        ? {
            ...job,
            steps: [
              { key: "plan", role: "Point Gander", label: "Planning the work", state: "completed", summary: null },
              { key: "research-1", role: "Forager Gosling", label: "Researching", state: "running", summary: null },
              { key: "compose", role: "Nestmaker Goose", label: "Writing it up", state: "pending", summary: null },
            ],
          }
        : null,
    };
  };

  const plan = {
    id: "d-plan",
    anchor_id: "a-diani",
    anchor_title: "Diani trip",
    kind: "plan",
    slot: "plan",
    title: "Plan for Diani trip",
    state: "unsure",
    hook: "Four days by the sea",
    body: PLAN_BODY,
    facts: [
      { label: "Budget", value: "KSh 80,000", source: "your answer" },
      { label: "Nights", value: "4", source: "the dates" },
    ],
    sources: [{ title: "Madaraka Express", url: "https://metickets.krc.co.ke/" }],
    objections: [{ kind: "grounding", about: "The matatu fare to Diani", fix: "Cite a current fare" }],
    changed: "",
    version: 1,
    task_id: "engine-1",
    refresh_at: null,
    unseen: true,
    mine: true,
    created_at: now,
    updated_at: now,
    job: null,
  };

  const json = (route: Route, body: unknown, status = 200) => route.fulfill({ status, json: body });
  const empty = (route: Route) => route.fulfill({ status: 204, body: "" });

  await page.route("**/api/v1/me/**", async (route) => {
    if (opts.unknownCaller) {
      return json(route, { error: "who_are_you: this phone is not linked to a member yet" }, 409);
    }
    const request = route.request();
    const method = request.method();
    const path = new URL(request.url()).pathname.replace("/api/v1/me", "");
    const parts = path.split("/").filter(Boolean);

    if (parts[0] === "anchors") {
      if (parts.length === 1 && method === "GET") {
        return json(route, { anchors: [...anchors.keys()].map((id) => ({ ...whole(id), questions: undefined })) });
      }
      if (parts.length === 1 && method === "POST") {
        const body = request.postDataJSON() as Record<string, unknown>;
        const id = `a-${anchors.size + 1}`;
        anchors.set(id, anchorOf({ ...body, id }));
        return json(route, whole(id), 201);
      }
      const id = parts[1];
      if (!anchors.has(id)) return json(route, { error: "no such anchor" }, 404);
      if (parts.length === 2) return json(route, whole(id));
      if (parts[2] === "confirm") {
        calls.confirmed.push(id);
        anchors.set(id, { ...anchors.get(id), state: "active" });
        return json(route, whole(id));
      }
      if (parts[2] === "dismiss") {
        anchors.delete(id);
        return empty(route);
      }
      if (parts[2] === "stop") {
        calls.stopped.push({ id, body: request.postDataJSON() });
        anchors.delete(id);
        return empty(route);
      }
    }

    if (parts[0] === "deliverables") {
      const id = parts[1];
      if (id !== "d-plan") return json(route, { error: "no such deliverable" }, 404);
      if (parts.length === 2) return json(route, plan);
      if (parts[2] === "seen") {
        calls.seen.push(id);
        plan.unseen = false;
        return empty(route);
      }
      if (parts[2] === "reject") {
        calls.rejected.push({ id, body: request.postDataJSON() });
        plan.state = "rejected";
        return empty(route);
      }
      if (parts[2] === "refresh") return json(route, { state: "queued" }, 202);
      if (parts[2] === "file") {
        return route.fulfill({
          status: 200,
          headers: { "Content-Type": "text/markdown; charset=utf-8", "Content-Disposition": 'attachment; filename="plan-for-diani-trip.md"' },
          body: PLAN_BODY,
        });
      }
    }

    if (parts[0] === "questions") {
      if (parts.length === 1) {
        return json(route, {
          batches: questions.length > 0 ? [{ id: "b-1", purpose: "anchor", anchor_id: "a-diani", title: "Diani trip", questions }] : [],
        });
      }
      const at = questions.findIndex((q) => q.id === parts[1]);
      if (at < 0) return json(route, { error: "this question is no longer open" }, 409);
      if (parts[2] === "answer") calls.answered.push({ id: parts[1], body: request.postDataJSON() });
      questions.splice(at, 1);
      return empty(route);
    }

    if (parts[0] === "facts") {
      return json(route, {
        facts: [
          {
            id: "f-veg",
            category: "food",
            fact: "Vegetarian",
            confidence: "likely",
            since: null,
            source: { kind: "memory", ref: "m-1" },
            tools: ["giap-places__find_restaurants"],
            pinned: false,
            score: 2,
            used: 3,
            state: "active",
            created_at: now,
            updated_at: now,
          },
        ],
        limit: 100,
      });
    }
    if (parts[0] === "dont") {
      return json(route, {
        rules: [
          { id: "r-1", rule: "Never: recipes built on a colour I like", strength: "never", reasons: ["not_relevant"], said: null, origin: "reject", origin_ref: "d-9", created_at: now },
        ],
      });
    }
    if (parts[0] === "agents") {
      return json(route, {
        agents: [
          { id: "trip", title: "Trip", about: "Plans trips and events: getting there, stays and what to bring.", state: "on", why: "" },
          { id: "work", title: "Work", about: "Keeps up with your projects.", state: "on", why: "" },
          { id: "hunt", title: "Hunt", about: "Finds listings worth a look for something you want to buy.", state: "on", why: "" },
          { id: "budget", title: "Budget", about: "Keeps a record of what you spend from pictures of receipts.", state: "offered", why: "You have sent three pictures of receipts or payments" },
        ],
      });
    }
    // An empty deck by default; `mockCards` in card-mocks.ts serves a full one.
    if (parts[0] === "idea-cards") return json(route, { cards: [], set: { state: "none", made_at: null } });
    if (parts[0] === "intentions") return json(route, { intentions: [] });
    if (parts[0] === "engine") return json(route, { jobs: [] });

    return json(route, { error: "not mocked" }, 404);
  });

  return calls;
}
