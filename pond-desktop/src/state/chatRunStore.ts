/**
 * The live chat turn, owned by the module rather than by a component.
 *
 * `GuiMode` renders sections through a `switch`, not a router, so pressing
 * anything in the sidebar UNMOUNTS `<Chat />`. While the turn driver lived
 * inside that component, every sidebar press threw away the transcript, the
 * queued follow-ups and the streaming bubble -- the stream itself kept running
 * and kept decoding tokens into `setMessages` calls on a dead component, which
 * React drops silently. So the answer arrived, was written to the database, and
 * was invisible to the person who asked for it.
 *
 * A turn is not a property of whichever screen happens to be showing. Its
 * lifetime is the window's, so it lives here, at module scope, and the screens
 * subscribe. Two of them do: the Chat section and the Hub's chat view, which
 * are two renderings of one conversation rather than two conversations.
 *
 * The shape -- a private `state` object, a `subs` set, `commit()`, and a hook
 * over `useSyncExternalStore` -- is the one `hub/state/hubDataStore.ts` and
 * `hubStore.ts` already use for state that has to outlive a view.
 *
 * Chosen over the app-wide reducer in `AppContext` for one measured reason:
 * `messages` changes once per token, and `AppContext` sits above `GuiMode`, so
 * every token would repaint the sidebar, the toasts and whatever section is
 * open. A store with its own snapshot repaints only what subscribed to it.
 */

import { useSyncExternalStore } from "react";
import { api } from "../api/PondApiClient";
import { nextCardId } from "./reducer";
import type {
  ContextCard as ContextCardType,
  LastResponseMeta,
} from "./reducer";
import { filterThinking } from "../lib/thinkFilter";
import { applySubagentProgress } from "../components/SubagentTree";
import type { SubagentRun } from "../components/SubagentTree";
import type {
  ChatEvent,
  ContextWarning,
  ImageAttachment,
  SessionMessage,
  TurnStats,
} from "../api/types";

// ── The message model ─────────────────────────────────────────────────────────

// Module-level counter -- shared across session loads and live sends.
let _msgId = 0;

export interface Message {
  id: number;
  role: "user" | "agent";
  text: string;
  streaming?: boolean;
  status?: string;
  cards?: ContextCardType[];
  thinkingBlocks?: string[];
  /** Wall clock around the reasoning stream, so the disclosure can say how
   *  long it took rather than showing an open-ended "Thinking…". */
  thinkingStartedAt?: number;
  thinkingEndedAt?: number;
  modelRole?: string;
  tokenUsage?: { prompt_tokens: number; completion_tokens: number };
  turnStats?: TurnStats;
  error?: boolean;
  historyToolNames?: string[];
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** PAI-4 P7b. Set when the turn's `context_warning` frame said the window is
   *  filling — renders the pressure line and the "Compact now" control. */
  contextWarning?: ContextWarning;
  /** PAI-6 P6. Delegations this turn started, folded from `subagent_progress`
   *  frames. Rendered WHILE streaming, unlike every other note here: a tree
   *  nobody sees until the turn ends is the spinner it replaces. */
  delegations?: SubagentRun[];
  /** Image preview URLs — either a live send's local previewUrl, or a
   *  built `${apiBase}${url}` for images replayed from session history. */
  images?: string[];
  /** The persisted session_messages.id this bubble corresponds to. Absent
   *  for a just-sent live turn until the "done" event backfills it (see
   *  `runTurn`) — copy/edit/refresh/like/dislike are disabled until then,
   *  since they all act against this id. */
  backendId?: string;
  /** Agent messages only: current like/dislike vote, mirrors the backend's
   *  `liked` column. `null`/absent = no vote. */
  liked?: boolean | null;
}

/** The status line under a streaming bubble, from a tool's raw name. */
function friendlyToolStatus(rawName: string): string {
  const bare = rawName.includes("__") ? rawName.split("__").pop()! : rawName;
  const map: Record<string, string> = {
    get_current_weather: "Checking the weather…",
    list_registered_devices: "Looking up your devices…",
    recall_memories: "Recalling what I know…",
    save_memory: "Saving that for later…",
    list_schedules: "Looking up your schedules…",
    get_user_profile: "Looking up your profile…",
    list_skills: "Checking my skills…",
  };
  if (map[bare]) return map[bare];
  return `Working on: ${bare.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase())}…`;
}

function sessionMessagesToMessages(raw: SessionMessage[]): Message[] {
  const out: Message[] = [];
  for (const m of raw) {
    if (m.role === "tool") continue;
    const images = m.images?.length
      ? m.images.map((img) => api.sessionAttachmentUrl(m.session_id, img.id))
      : undefined;
    if (m.role === "assistant") {
      const hasContent = m.content.trim().length > 0;
      const hasToolCalls = (m.tool_calls?.length ?? 0) > 0;
      if (!hasContent && hasToolCalls) continue;
      const historyToolNames = hasToolCalls
        ? m.tool_calls!.map((tc) =>
            tc.name.includes("__") ? tc.name.split("__").pop()! : tc.name,
          )
        : undefined;
      // PAI-5 P6. The panel below already renders `thinkingBlocks` and is
      // already gated on `!streaming`, which is exactly right for replayed
      // history. All that was missing was the refill: before this, reasoning
      // existed only for the lifetime of the SSE connection that produced it,
      // so reloading a conversation showed every answer with the thinking that
      // led to it silently gone.
      const thinkingBlocks = m.thinking?.length ? m.thinking : undefined;
      out.push({
        id: ++_msgId,
        role: "agent",
        text: m.content,
        historyToolNames,
        images,
        thinkingBlocks,
        // The persisted id and the vote ride the SAME row as the reasoning.
        // Pushing them as a second entry renders every assistant turn twice on
        // reload, which is what a keep-both merge of these two changes does if
        // nobody looks — `Chat.test.tsx`'s PAI-5 replay tests caught it as
        // "found multiple elements with the text".
        backendId: m.id,
        liked: m.liked ?? null,
      });
    } else {
      out.push({
        id: ++_msgId,
        role: "user",
        text: m.content,
        images,
        backendId: m.id,
      });
    }
  }
  return out;
}

// ── What a subscriber sees ────────────────────────────────────────────────────

export interface ChatRunSnapshot {
  readonly messages: readonly Message[];
  readonly busy: boolean;
  readonly queued: readonly string[];
  /** Reseeded per turn, so the working quip differs between turns but holds
   *  still while one runs -- including across a remount, which is the whole
   *  point of it living here. */
  readonly turnSeed: number;
  readonly loadingSession: boolean;
  readonly sessionId: string | undefined;
  /** Monotonic. Anything that must happen once per finished turn keys on it. */
  readonly completedTurns: number;
}

/**
 * The one seam between a module-scope driver and React.
 *
 * The driver cannot read app state or call `dispatch`; both belong to a mounted
 * provider. `AppContextProvider` installs this and keeps it current, for the
 * same reason the schedule listener lives there rather than in a section: a
 * turn started in Chat keeps arriving while you are looking at Devices, and its
 * `done` frame still has to reach the reducer.
 */
export interface ChatRunBridge {
  readonly sessionToken: string | null;
  readonly serverOnline: boolean;
  onSessionId(sessionId: string): void;
  onResponseMeta(meta: LastResponseMeta): void;
  onContextCard(card: ContextCardType): void;
}

type Subscriber = () => void;

interface InternalState {
  messages: Message[];
  busy: boolean;
  queued: string[];
  turnSeed: number;
  loadingSession: boolean;
  sessionId: string | undefined;
  completedTurns: number;
  /** Turns a mounted surface has actually shown. `hasLiveThread` is the gap. */
  acknowledgedTurns: number;
  /** Bumped per run. Every write inside a stream loop checks it, so a
   *  conversation switched mid-turn cannot be written into by the old turn. */
  runSeq: number;
  /** The `<think>` parser's carry bit, owned by whichever run is streaming. */
  inThinkBlock: boolean;
  /** Object URLs this store created and is therefore allowed to revoke. */
  ownedPreviews: Set<string>;
  /** The server-side run driving the current turn, once it has named itself. */
  runId: string | null;
  /** The server process that run belongs to. A different one means it is gone. */
  epoch: string | null;
  /** How far this client has read. What a reattach resumes from. */
  lastSeq: number;
  bridge: ChatRunBridge | null;
  snapshot: ChatRunSnapshot;
  subs: Set<Subscriber>;
}

const state: InternalState = {
  messages: [],
  busy: false,
  queued: [],
  turnSeed: Date.now(),
  loadingSession: false,
  sessionId: undefined,
  completedTurns: 0,
  acknowledgedTurns: 0,
  runSeq: 0,
  inThinkBlock: false,
  ownedPreviews: new Set(),
  runId: null,
  epoch: null,
  lastSeq: 0,
  bridge: null,
  snapshot: {
    messages: [],
    busy: false,
    queued: [],
    turnSeed: 0,
    loadingSession: false,
    sessionId: undefined,
    completedTurns: 0,
  },
  subs: new Set(),
};

/**
 * Rebuild the snapshot once, then tell everyone.
 *
 * `useSyncExternalStore` compares snapshots by identity and throws if
 * `getSnapshot` returns a fresh object on every call, so the object is built
 * HERE, on mutation, and `getSnapshot` only hands back the field.
 */
function commit(): void {
  state.snapshot = {
    messages: state.messages,
    busy: state.busy,
    queued: state.queued,
    turnSeed: state.turnSeed,
    loadingSession: state.loadingSession,
    sessionId: state.sessionId,
    completedTurns: state.completedTurns,
  };
  state.subs.forEach((f) => f());
}

function subscribe(f: Subscriber): () => void {
  state.subs.add(f);
  return () => {
    state.subs.delete(f);
  };
}

function getSnapshot(): ChatRunSnapshot {
  return state.snapshot;
}

/** Replace the message list and publish. The only writer of `state.messages`. */
function mutate(fn: (prev: Message[]) => Message[]): void {
  const next = fn(state.messages);
  if (next === state.messages) return;
  state.messages = next;
  commit();
}

/** Fold a patch into the last bubble, but only while it is the agent's. */
function patchLastAgent(fn: (last: Message) => Message): void {
  mutate((prev) => {
    const last = prev[prev.length - 1];
    if (!last || last.role !== "agent") return prev;
    return [...prev.slice(0, -1), fn(last)];
  });
}

// ── Reading ───────────────────────────────────────────────────────────────────

export function useChatRun(): ChatRunSnapshot {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export function getChatRun(): ChatRunSnapshot {
  return state.snapshot;
}

/**
 * Should opening a chat surface land in the thread rather than on the wall?
 *
 * `Chat.tsx` argues at length that the wall, not the last conversation, is the
 * right landing, and that argument is about arriving with nothing in flight.
 * It does not cover arriving to find your own answer already written and never
 * seen -- that is not a choice about where to steer, it is the thing you came
 * back for. So exactly two conditions, and no timer: a timer would make where
 * you land depend on how long you were away, which is not something anyone can
 * predict from the outside.
 *
 * Both self-clear. Once a mounted surface has shown the finished turn it calls
 * `acknowledgeCompletion`, and the next visit is the wall again, as designed.
 */
export function hasLiveThread(): boolean {
  if (state.busy) return true;
  if (state.completedTurns > state.acknowledgedTurns) return true;
  // A pointer left behind by the last window, read synchronously.
  //
  // The timing is load-bearing: a surface decides which screen to open on while
  // it is mounting, and `resumeActiveRun` cannot answer by then — it has a
  // round trip to make. Without this the app lands on the wall and the turn it
  // is about to resume into appears a second later behind it, which is the
  // exact failure this whole change exists to remove.
  return readRunPointer() !== null;
}

/** A surface has shown the finished turn; stop resuming into it. */
export function acknowledgeCompletion(): void {
  if (state.acknowledgedTurns === state.completedTurns) return;
  state.acknowledgedTurns = state.completedTurns;
}

// ── The bridge ────────────────────────────────────────────────────────────────

export function setChatRunBridge(bridge: ChatRunBridge): () => void {
  state.bridge = bridge;
  // A bridge arriving with the server up is also the signal that a queue held
  // through an outage can move again -- see `drainQueue`.
  drainQueue();
  return () => {
    // Identity-checked: StrictMode runs mount, unmount, mount, so a blind clear
    // here would null the bridge that the second mount had already installed.
    if (state.bridge === bridge) state.bridge = null;
  };
}

// ── Object URLs ───────────────────────────────────────────────────────────────

/**
 * Revoke previews this store created that no surviving message still shows.
 *
 * Never touches a URL from `api.sessionAttachmentUrl`: history images are
 * ordinary http URLs, and revoking one is a no-op that would still be a lie
 * about who owns what.
 */
function revokeOwnedPreviews(surviving: readonly Message[]): void {
  if (state.ownedPreviews.size === 0) return;
  const stillShown = new Set<string>();
  for (const m of surviving)
    for (const url of m.images ?? []) stillShown.add(url);
  for (const url of [...state.ownedPreviews]) {
    if (stillShown.has(url)) continue;
    state.ownedPreviews.delete(url);
    if (url.startsWith("blob:")) URL.revokeObjectURL(url);
  }
}

// ── Driving a turn ────────────────────────────────────────────────────────────

export interface SendTurn {
  text: string;
  images?: ImageAttachment[];
  /** Composer preview object URLs. The store takes ownership of revoking them:
   *  the bubble outlives the tray now, so the tray must not. */
  previewUrls?: string[];
}

/**
 * Send, or queue if a reply is still streaming.
 *
 * The composer stays live throughout, so a thought does not have to wait for
 * the model. Attachments are deliberately NOT queued -- they belong to the turn
 * they were attached to, and silently re-binding them to a later message would
 * send an image with the wrong question.
 */
export function sendTurn(turn: SendTurn): void {
  if (state.busy) {
    if (!turn.text) return;
    state.queued = [...state.queued, turn.text];
    commit();
    return;
  }
  void runTurn(turn);
}

/**
 * Drain one queued message.
 *
 * Lives here rather than in an effect keyed on `busy`, which is where it used
 * to live: an effect belongs to a mounted component, so leaving Chat with two
 * follow-ups queued meant they were never sent. Reading `state.queued` at call
 * time also removes the stale-closure hazard that effect was written around.
 */
function drainQueue(): void {
  if (state.busy || state.queued.length === 0) return;
  // Held, never dropped. The bridge reinstalling with the server back up calls
  // this again, so an outage delays the queue instead of eating it.
  if (!state.bridge?.serverOnline) return;
  const [next, ...rest] = state.queued;
  state.queued = rest;
  commit();
  void runTurn({ text: next });
}

/**
 * One turn's frames, whichever stream they arrive on.
 *
 * Extracted so that a turn STARTED here and a turn REATTACHED to after a reload
 * fold identically. Two copies of this would be two chances to disagree about
 * what a frame means, which is the same reason the Hub renders a projection of
 * one message model rather than keeping its own.
 */
interface TurnCtx {
  /** True once the conversation has moved on and this run must stop writing. */
  stale: () => boolean;
  userMsgId: number;
  agentMsgId: number;
}

async function consume(
  stream: AsyncGenerator<unknown>,
  ctx: TurnCtx,
): Promise<void> {
  for await (const event of stream) {
    if (ctx.stale()) return;
    const ev = event as ChatEvent;

    // Where this client has got to, so a reattach can ask for the rest and
    // nothing arrives twice.
    if (typeof ev.seq === "number") {
      state.lastSeq = ev.seq;
      rememberRun();
    }

    if (ev.type === "run_started") {
      // The turn now has a name, and so does its conversation: the server mints
      // the session id up front rather than at the end, so this is the first
      // moment the client can know it. Adopting it here is what makes the
      // pointer below writable at all -- waiting for `done` would mean a reload
      // one second into a turn had nothing to come back to.
      state.runId = ev.run_id ?? null;
      state.epoch = ev.epoch ?? null;
      if (ev.session_id && !state.sessionId) {
        state.sessionId = ev.session_id;
        state.bridge?.onSessionId(ev.session_id);
        commit();
      }
      rememberRun();
      continue;
    }
    if (ev.type === "reattached" || ev.type === "cancelled") {
      // Bookkeeping frames, not content. `cancelled` is followed by a `done`
      // that carries `interrupted`, which is what the bubble reads.
      continue;
    }
    if (ev.type === "replay_gap" || ev.type === "run_evicted") {
      // The server cannot hand back what this client missed. Say so on the
      // bubble rather than stitching a partial answer together and presenting
      // it as whole -- and reload the session, which is authoritative for
      // everything that actually committed.
      console.warn(
        "Chat run lost frames; reloading the conversation:",
        ev.type,
      );
      const sessionId = state.sessionId;
      if (sessionId) void openSession(sessionId);
      return;
    }

    if (ev.type === "text" && (ev.content ?? ev.token)) {
      const raw = ev.content ?? ev.token ?? "";
      const [visible, newInBlock] = filterThinking(raw, state.inThinkBlock);
      state.inThinkBlock = newInBlock;
      if (visible) {
        mutate((prev) => {
          const last = prev[prev.length - 1];
          if (!last || last.role !== "agent") return prev;
          // A bubble already showing an error is finished. The error arm
          // OVERWRITES `text` while this one APPENDS to it, so text arriving
          // after an error ran straight onto the end of the error sentence --
          // "…missing providerI could not produce a response". The server sends
          // these as two separate frames and deliberately keeps streaming past an
          // error, so the honest rendering is two messages, not one string.
          if (last.error) {
            return [
              ...prev,
              { id: ++_msgId, role: "agent", text: visible, streaming: true },
            ];
          }
          return [
            ...prev.slice(0, -1),
            { ...last, text: last.text + visible, status: undefined },
          ];
        });
      }
    } else if (ev.type === "thinking" && ev.content) {
      const now = Date.now();
      patchLastAgent((last) => ({
        ...last,
        thinkingBlocks: [...(last.thinkingBlocks ?? []), ev.content as string],
        // First chunk opens the span; every chunk moves the close, so the
        // duration is how long reasoning actually streamed rather than
        // how long the whole turn took.
        thinkingStartedAt: last.thinkingStartedAt ?? now,
        thinkingEndedAt: now,
      }));
    } else if (ev.type === "status" && ev.content) {
      patchLastAgent((last) => ({ ...last, status: ev.content }));
    } else if (ev.type === "tool_call" && ev.tool) {
      const card: ContextCardType = {
        id: nextCardId(),
        tool: ev.tool,
        callId: ev.id as string | undefined,
        data: (ev.result as Record<string, unknown>) ?? {},
        timestamp_ms: Date.now(),
      };
      state.bridge?.onContextCard(card);
      patchLastAgent((last) => ({
        ...last,
        cards: [...(last.cards ?? []), card],
        status: friendlyToolStatus(ev.tool ?? ""),
      }));
    } else if (ev.type === "tool_result" && ev.id) {
      mutate((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent" || !last.cards) return prev;
        const cardData = ev.ui?.data ?? { result: ev.content };
        const renderHint = ev.ui?.card_type;
        const evId = ev.id as string;
        const evTool = ev.tool as string | undefined;
        const newCards = last.cards.map((c) =>
          (c.callId && c.callId === evId) || (evTool && c.tool === evTool)
            ? { ...c, data: cardData, ...(renderHint ? { renderHint } : {}) }
            : c,
        );
        return [
          ...prev.slice(0, -1),
          { ...last, cards: newCards, status: undefined },
        ];
      });
    } else if (ev.type === "review_status" && ev.content) {
      patchLastAgent((last) => ({ ...last, status: ev.content }));
    } else if (
      (ev.type === "review_revision" || ev.type === "tool_revision") &&
      ev.content
    ) {
      state.inThinkBlock = false;
      patchLastAgent((last) => ({
        ...last,
        text: ev.content!,
        status: undefined,
      }));
    } else if (ev.type === "error" || ev.error) {
      const errMsg = ev.error ?? "Unknown error from agent";
      patchLastAgent((last) => ({
        ...last,
        text: `Error: ${errMsg}`,
        streaming: false,
        error: true,
      }));
    } else if (ev.done && ev.session_id) {
      state.sessionId = ev.session_id;
      state.bridge?.onSessionId(ev.session_id);
      patchLastAgent((last) => ({
        ...last,
        ...(ev.model_role ? { modelRole: ev.model_role } : {}),
        ...(ev.usage && ev.usage.completion_tokens > 0
          ? { tokenUsage: ev.usage }
          : {}),
      }));
      if (ev.model_name && ev.model_role) {
        state.bridge?.onResponseMeta({
          modelName: ev.model_name,
          modelRole: ev.model_role,
          completionTokens: ev.usage?.completion_tokens ?? 0,
        });
      }
      // The stream never carries the persisted message ids, so copy/edit/
      // refresh/like/dislike (which all act on a real backend id) have
      // nothing to target yet. Fetch the small tail of the session and
      // match by id, not array position — same reasoning as turn_stats
      // below, a session switch mid-fetch must not misattribute this.
      const doneSessionId = ev.session_id;
      const forUser = ctx.userMsgId;
      const forAgent = ctx.agentMsgId;
      void (async () => {
        try {
          const recent = await api.getSessionMessages(doneSessionId, 10);
          if (ctx.stale()) return;
          const nonTool = recent.filter((m) => m.role !== "tool");
          const lastUser = [...nonTool]
            .reverse()
            .find((m) => m.role === "user");
          const lastAgent = [...nonTool]
            .reverse()
            .find((m) => m.role === "assistant");
          mutate((prev) =>
            prev.map((m) => {
              if (m.id === forAgent && lastAgent)
                return {
                  ...m,
                  backendId: lastAgent.id,
                  liked: lastAgent.liked ?? null,
                };
              if (m.id === forUser && lastUser)
                return { ...m, backendId: lastUser.id };
              return m;
            }),
          );
        } catch {
          // Non-fatal: the turn already rendered; only the action icons
          // stay disabled until the next successful history load.
        }
      })();
    } else if (ev.type === "turn_stats") {
      // Attach by id, not array position — a mid-stream session switch
      // replaces `messages` with another conversation's history, and the
      // stats must never land on one of those messages.
      const stats = ev as unknown as TurnStats;
      mutate((prev) =>
        prev.map((m) =>
          m.id === ctx.agentMsgId ? { ...m, turnStats: stats } : m,
        ),
      );
    } else if (ev.type === "turn_limit_reached") {
      // The agent ran out of turns rather than finishing. Mark the message
      // (by id, same reasoning as turn_stats) so it offers a Continue action
      // instead of leaving the backend's "would you like me to continue?"
      // as a question nothing can answer.
      const limit = ev.max_turns ?? 0;
      mutate((prev) =>
        prev.map((m) =>
          m.id === ctx.agentMsgId ? { ...m, turnLimit: limit } : m,
        ),
      );
    } else if (ev.type === "subagent_progress") {
      // PAI-6 P6. Attach by id — same reasoning as turn_stats — and fold
      // through the one shared reducer, so this surface and the hub cannot
      // disagree about what a frame means.
      mutate((prev) =>
        prev.map((m) =>
          m.id === ctx.agentMsgId
            ? {
                ...m,
                delegations: applySubagentProgress(m.delegations ?? [], ev),
              }
            : m,
        ),
      );
    } else if (ev.type === "context_warning") {
      // PAI-4 P7b. The window is filling. Attach by id — same reasoning as
      // turn_stats and turn_limit_reached — so the note lands on this turn
      // and not on whatever message a mid-stream session switch left last.
      const cw = ev as unknown as ContextWarning;
      mutate((prev) =>
        prev.map((m) =>
          m.id === ctx.agentMsgId ? { ...m, contextWarning: cw } : m,
        ),
      );
    }
  }
}

async function runTurn(turn: SendTurn): Promise<void> {
  const text = turn.text.trim();
  const images = turn.images ?? [];
  if (!text && images.length === 0) return;

  // Claimed synchronously, before the first await, so two sends in one tick
  // cannot both start a run.
  state.busy = true;
  state.turnSeed = Date.now();
  state.inThinkBlock = false;
  const runId = ++state.runSeq;
  /** A run the conversation has moved on from must not write anything. */
  const stale = () => runId !== state.runSeq;
  // A fresh turn: forget whatever run the last one left behind.
  state.runId = null;
  state.epoch = null;
  state.lastSeq = 0;

  for (const url of turn.previewUrls ?? []) state.ownedPreviews.add(url);

  const userMsg: Message = {
    id: ++_msgId,
    role: "user",
    text,
    images: turn.previewUrls?.length ? turn.previewUrls : undefined,
  };
  const agentMsg: Message = {
    id: ++_msgId,
    role: "agent",
    text: "",
    streaming: true,
  };
  state.messages = [...state.messages, userMsg, agentMsg];
  commit();

  const ctx: TurnCtx = {
    stale,
    userMsgId: userMsg.id,
    agentMsgId: agentMsg.id,
  };

  try {
    const token = state.bridge?.sessionToken ?? null;
    api.setToken(token);
    await consume(
      api.chatStream(
        text,
        state.sessionId,
        token ?? undefined,
        undefined,
        images,
        // Ask the server to keep going if this window goes away. Everything
        // below -- remembering the run, reattaching on the way back -- is only
        // reachable because of this flag.
        true,
      ),
      ctx,
    );
  } catch (e) {
    if (stale()) return;
    // Loud on the console as well as in the bubble: with no surface mounted the
    // bubble is the only record, and it is not read until someone comes back.
    console.warn("Chat turn failed:", e);
    patchLastAgent((last) => ({
      ...last,
      text: `Error: ${String(e)}`,
      streaming: false,
      error: true,
    }));
  } finally {
    if (!stale()) {
      mutate((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent" || !last.streaming) return prev;
        return [...prev.slice(0, -1), { ...last, streaming: false }];
      });
      state.busy = false;
      state.completedTurns += 1;
      commit();
      // A microtask, so a run that rejects before its first await cannot
      // recurse straight back into itself on this stack.
      queueMicrotask(drainQueue);
    }
  }
}

// ── Surviving a reload ────────────────────────────────────────────────────────

/**
 * Where the run pointer is kept between page loads.
 *
 * `localStorage` and not the store, obviously — the store dies with the window,
 * which is the case this exists for. It holds no conversation content, only
 * enough to ask the server what it is still doing: the session, the run, the
 * server process that run belongs to, and how far this client had read.
 */
const RUN_POINTER_KEY = "giap-chat-run";

interface RunPointer {
  sessionId: string;
  runId: string;
  epoch: string;
  lastSeq: number;
}

function rememberRun(): void {
  if (!state.sessionId || !state.runId || !state.epoch) return;
  const pointer: RunPointer = {
    sessionId: state.sessionId,
    runId: state.runId,
    epoch: state.epoch,
    lastSeq: state.lastSeq,
  };
  try {
    localStorage.setItem(RUN_POINTER_KEY, JSON.stringify(pointer));
  } catch {
    // Private browsing, or storage disabled. Losing the pointer costs a resume,
    // not a turn — the answer is still persisted server-side either way.
  }
}

function forgetRun(): void {
  try {
    localStorage.removeItem(RUN_POINTER_KEY);
  } catch {
    // Same as above: nothing here is load-bearing enough to fail over.
  }
}

function readRunPointer(): RunPointer | null {
  try {
    const raw = localStorage.getItem(RUN_POINTER_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as RunPointer;
    if (!parsed.sessionId || !parsed.runId) return null;
    return parsed;
  } catch {
    return null;
  }
}

/**
 * Pick up a turn this window was never around for.
 *
 * The reload story, end to end. The store died with the last window, so the
 * transcript comes from the session's persisted messages and the turn in flight
 * comes from the run's own replay — the one part the database cannot answer,
 * because an assistant turn is only written once it finishes.
 *
 * Returns whether anything was resumed. Every failure is quiet and ends in the
 * same place: no run, and a conversation the user can still read.
 */
export async function resumeActiveRun(): Promise<boolean> {
  const pointer = readRunPointer();
  if (!pointer) return false;
  if (state.busy) return false;

  let active;
  try {
    active = await api.getActiveRun(pointer.sessionId);
  } catch (e) {
    console.warn("Could not ask about the active run (non-fatal):", e);
    return false;
  }

  // Gone, or gone with the process that owned it. Either way the pointer is
  // stale and the persisted messages are the whole truth — but the conversation
  // still opens, because the person was just in it and `hasLiveThread` has
  // already sent the surface to the thread on the strength of that pointer.
  // Landing them in an empty one would be worse than the wall they were spared.
  if (
    !active ||
    active.run_id !== pointer.runId ||
    active.epoch !== pointer.epoch
  ) {
    forgetRun();
    await openSession(pointer.sessionId);
    state.completedTurns += 1;
    commit();
    return false;
  }
  if (active.state !== "running") {
    // It finished while nothing was here to see it. The answer is in the
    // database by now, so there is nothing to tail — but the thread should
    // still open on it rather than on the wall.
    forgetRun();
    await openSession(pointer.sessionId);
    state.completedTurns += 1;
    commit();
    return true;
  }

  // Everything committed so far, which is the user's question and every turn
  // before this one. The answer being written right now is not in here yet.
  await openSession(pointer.sessionId);

  const runId = ++state.runSeq;
  const stale = () => runId !== state.runSeq;
  state.busy = true;
  state.turnSeed = Date.now();
  state.inThinkBlock = false;
  state.runId = pointer.runId;
  state.epoch = pointer.epoch;
  state.lastSeq = pointer.lastSeq;

  // A bubble for the answer in progress. The user's half is already on screen
  // from the history load above, so only the agent's is added here.
  const agentMsg: Message = {
    id: ++_msgId,
    role: "agent",
    text: "",
    streaming: true,
  };
  const lastUser = [...state.messages].reverse().find((m) => m.role === "user");
  state.messages = [...state.messages, agentMsg];
  commit();

  const ctx: TurnCtx = {
    stale,
    userMsgId: lastUser?.id ?? agentMsg.id,
    agentMsgId: agentMsg.id,
  };

  try {
    const token = state.bridge?.sessionToken ?? null;
    api.setToken(token);
    // From where this client had actually read, not from the beginning: the
    // frames before that are already on screen from a previous window, and
    // replaying them would write the answer out twice.
    await consume(
      api.reattachRun(
        pointer.runId,
        pointer.lastSeq,
        pointer.epoch,
        token ?? undefined,
      ),
      ctx,
    );
  } catch (e) {
    if (!stale()) {
      console.warn("Could not follow the run that was already in flight:", e);
      patchLastAgent((last) => ({
        ...last,
        text: `Error: ${String(e)}`,
        streaming: false,
        error: true,
      }));
    }
  } finally {
    if (!stale()) {
      mutate((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent" || !last.streaming) return prev;
        return [...prev.slice(0, -1), { ...last, streaming: false }];
      });
      state.busy = false;
      state.completedTurns += 1;
      forgetRun();
      commit();
    }
  }
  return true;
}

/**
 * Let go of the run this window is driving, and stop it server-side.
 *
 * Bumping `runSeq` alone only stops us *writing* the frames down -- the run is
 * `Detached`, so the model keeps generating to completion for nobody. Measured
 * against a live pond: a client that left at frame 2 had its run finish at
 * frame 664, seventy-three seconds later, and post-turn memory extraction then
 * opened a further provider call on the same single-slot engine.
 *
 * Deliberately leaving a turn is not the case `resumable` exists for. That case
 * is the window going away with the turn still running, which is unchanged:
 * nothing here runs on unload, so a closed window still comes back to a
 * finished answer.
 *
 * Fire-and-forget: navigation must not wait on the network, and a stop the user
 * asked for should not look like it failed because the request did.
 */
function stopServerRun(): void {
  const runId = state.runId;
  forgetRun();
  if (!runId) return;
  void api.cancelRun(runId).catch((e) => {
    console.warn("Could not stop the run server-side (non-fatal):", e);
  });
}

/**
 * Stop the turn on purpose.
 *
 * The only way now: a detached run does not end because its reader left, so
 * closing the window or navigating away is no longer a cancel. Local state is
 * cleared regardless of what the server says, because a stop the user asked for
 * should not appear to have failed on a network error.
 */
export async function abortRun(): Promise<void> {
  const runId = state.runId;
  state.runSeq += 1;
  state.busy = false;
  state.queued = [];
  forgetRun();
  commit();
  if (!runId) return;
  try {
    await api.cancelRun(runId);
  } catch (e) {
    console.warn("Could not stop the run server-side (non-fatal):", e);
  }
}

// ── Conversation lifecycle ────────────────────────────────────────────────────

/** Replace the transcript wholesale, revoking anything the old one owned. */
function replaceMessages(next: Message[]): void {
  revokeOwnedPreviews(next);
  state.messages = next;
  commit();
}

/**
 * Open one conversation, replaying its persisted history.
 *
 * `stopCurrentRun` is the difference between *leaving* a turn and merely
 * *re-reading* one, and it defaults to off because three of this function's
 * four callers are the latter: `resumeActiveRun` uses it to lay down the
 * history before it reattaches, and the `replay_gap` / `run_evicted` arm uses
 * it to reload a conversation whose run is still generating. Cancelling from
 * in here would have aborted the very run those paths exist to recover.
 *
 * Only the wall's "open this conversation" passes `true`.
 */
export async function openSession(
  sessionId: string,
  opts?: { stopCurrentRun?: boolean },
): Promise<void> {
  state.runSeq += 1; // anything still streaming stops writing here
  if (opts?.stopCurrentRun) stopServerRun(); // ...and stops generating too
  state.busy = false;
  state.queued = [];
  state.sessionId = sessionId;
  state.loadingSession = true;
  replaceMessages([]);
  try {
    const msgs = await api.getSessionMessages(sessionId);
    if (state.sessionId !== sessionId) return;
    replaceMessages(sessionMessagesToMessages(msgs ?? []));
  } catch (err) {
    console.warn("Could not open conversation (non-fatal):", err);
  } finally {
    state.loadingSession = false;
    commit();
  }
}

/**
 * Follow a session id someone else set -- a deep link, or the `session-created`
 * event `AppContext` listens for.
 *
 * Bails when the id already matches, because the driver sets it on `done` and
 * the resulting dispatch must not reload history over the live stream. Bails
 * while busy for the same reason from the other direction: that listener is a
 * second writer, and a running turn is not its to replace.
 */
export async function followExternalSession(sessionId: string): Promise<void> {
  if (sessionId === state.sessionId) return;
  if (state.busy) return;
  state.sessionId = sessionId;
  try {
    const msgs = await api.getSessionMessages(sessionId);
    if (state.sessionId !== sessionId) return;
    replaceMessages(sessionMessagesToMessages(msgs ?? []));
  } catch (err) {
    console.warn("Could not load session history (non-fatal):", err);
  }
}

/** New chat: stop the run, drop the queue, clear the transcript. */
export function resetConversation(): void {
  state.runSeq += 1;
  stopServerRun(); // what this function's doc comment has always claimed to do
  state.busy = false;
  state.queued = [];
  state.sessionId = undefined;
  state.loadingSession = false;
  replaceMessages([]);
}

/** Drop this message and everything after it — edit and regenerate both do. */
export function truncateFrom(localMessageId: number): void {
  mutate((prev) => {
    const idx = prev.findIndex((m) => m.id === localMessageId);
    if (idx === -1) return prev;
    const next = prev.slice(0, idx);
    revokeOwnedPreviews(next);
    return next;
  });
}

/** Targeted write for optimistic UI a surface owns — today the like/dislike flip. */
export function patchMessage(
  localMessageId: number,
  patch: Partial<Message>,
): void {
  mutate((prev) =>
    prev.map((m) => (m.id === localMessageId ? { ...m, ...patch } : m)),
  );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/**
 * Module state outlives a test file's `render`/`cleanup`, so every suite that
 * touches chat has to start from a known one. Same contract as
 * `__resetHubDataForTests`.
 */
export function __resetChatRunForTests(): void {
  state.messages = [];
  state.busy = false;
  state.queued = [];
  state.turnSeed = 0;
  state.loadingSession = false;
  state.sessionId = undefined;
  state.completedTurns = 0;
  state.acknowledgedTurns = 0;
  state.runSeq = 0;
  state.inThinkBlock = false;
  state.ownedPreviews.clear();
  state.runId = null;
  state.epoch = null;
  state.lastSeq = 0;
  forgetRun();
  state.bridge = null;
  state.subs.clear();
  commit();
}
