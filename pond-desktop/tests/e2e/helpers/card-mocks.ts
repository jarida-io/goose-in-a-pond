import type { Page, Route } from "@playwright/test";

/** What the card routes were asked to do, so a spec can check the body that was sent. */
export interface CardCalls {
  done: Array<{ id: string; body: unknown }>;
  saved: string[];
  dismissed: Array<{ id: string; body: unknown }>;
  deeper: string[];
  refreshed: number;
}

const now = "2026-11-17T06:00:00Z";

function cardOf(over: Record<string, unknown>): Record<string, unknown> {
  return {
    kind: "do",
    when: null,
    where: null,
    cost: null,
    effort: null,
    action: null,
    sources: [],
    notes: { found: "", next: "", caveats: "" },
    anchor: null,
    state: "new",
    batch_id: "b-1",
    created_at: now,
    fades_at: "2026-11-18T18:00:00Z",
    ...over,
  };
}

/**
 * Mocks `/api/v1/me/idea-cards` and its actions, holding state so an action shows in the next read.
 * Call after `mockMyGoose`, whose catch-all this one outranks.
 */
export async function mockCards(page: Page): Promise<CardCalls> {
  const calls: CardCalls = { done: [], saved: [], dismissed: [], deeper: [], refreshed: 0 };
  let setState = "ready";
  const cards = new Map<string, Record<string, unknown>>([
    [
      "c-walk",
      cardOf({
        id: "c-walk",
        title: "Walk the Karura Forest waterfall trail",
        why: { text: "You said you want more time outdoors on weekends.", source: { kind: "interest", label: "Hiking" } },
        when: "Saturday 9 am",
        where: "Karura Forest",
        cost: "KSh 600 entry",
        action: { kind: "todo", label: "Add to-do", text: "Walk the Karura waterfall trail" },
        sources: [{ kind: "web", title: "Karura Forest: visiting", url: "https://www.karurafriends.org/visit" }],
        notes: { found: "The waterfall trail is about 5 km.", next: "Check the weather on Friday.", caveats: "Entry prices may have changed." },
      }),
    ],
    [
      "c-pack",
      cardOf({
        id: "c-pack",
        kind: "ask",
        title: "What should we pack for Diani?",
        why: { text: "You leave for Diani on 20 December.", source: { kind: "plan", label: "Diani trip" } },
        when: "Before 20 December",
        action: { kind: "chat", label: "Ask Goose", prompt: "What should we pack for Diani?" },
        sources: [{ kind: "plan", title: "Diani trip" }],
        notes: { found: "Four of you are going.", next: "Make a packing list.", caveats: "" },
        anchor: { id: "a-diani", title: "Diani trip" },
      }),
    ],
  ]);

  const json = (route: Route, body: unknown, status = 200) => route.fulfill({ status, json: body });

  await page.route("**/api/v1/me/idea-cards**", async (route) => {
    const request = route.request();
    const parts = new URL(request.url()).pathname.replace("/api/v1/me/idea-cards", "").split("/").filter(Boolean);

    if (parts.length === 0) {
      return json(route, { cards: [...cards.values()], set: { state: setState, made_at: now } });
    }
    if (parts[0] === "refresh") {
      calls.refreshed += 1;
      setState = "queued";
      return json(route, { state: "queued" }, 202);
    }
    const card = cards.get(parts[0]);
    if (!card) return json(route, { error: "no such card" }, 404);
    if (parts[1] === "do") {
      calls.done.push({ id: parts[0], body: request.postDataJSON() });
      cards.delete(parts[0]);
      const action = card.action as { kind: string; prompt?: string } | null;
      return json(route, { card: { ...card, state: "done" }, result: { kind: action?.kind ?? "go_deeper", prompt: action?.prompt } });
    }
    if (parts[1] === "deeper") {
      calls.deeper.push(parts[0]);
      cards.delete(parts[0]);
      return json(route, { card: { ...card, state: "done" }, anchor_id: "a-diani" });
    }
    if (parts[1] === "save" || parts[1] === "unsave") {
      if (parts[1] === "save") calls.saved.push(parts[0]);
      const next = { ...card, state: parts[1] === "save" ? "saved" : "new" };
      cards.set(parts[0], next);
      return json(route, next);
    }
    if (parts[1] === "dismiss") {
      calls.dismissed.push({ id: parts[0], body: request.postDataJSON() });
      cards.delete(parts[0]);
      return route.fulfill({ status: 204, body: "" });
    }
    return json(route, { error: "not mocked" }, 404);
  });

  return calls;
}
