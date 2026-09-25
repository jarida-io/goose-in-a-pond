/**
 * The live chat turn, at module scope because `<Chat />` unmounts on every sidebar press.
 * Not in the `AppContext` reducer: `messages` changes per token and would repaint the app.
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
  /** Reasoning-stream wall clock, so the disclosure can say how long it took. */
  thinkingStartedAt?: number;
  thinkingEndedAt?: number;
  modelRole?: string;
  tokenUsage?: { prompt_tokens: number; completion_tokens: number };
  turnStats?: TurnStats;
  error?: boolean;
  historyToolNames?: string[];
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** From the turn's `context_warning` frame; renders the pressure line and "Compact now". */
  contextWarning?: ContextWarning;
  /** Folded from `subagent_progress` frames; unlike the other notes, rendered while streaming. */
  delegations?: SubagentRun[];
  /** A live send's local preview URLs, or attachment URLs for replayed history. */
  images?: string[];
  /** session_messages.id; unset on a live turn until `done` backfills it, disabling its actions. */
  backendId?: string;
  /** Agent messages only; mirrors the backend `liked` column, `null`/absent = no vote. */
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
      const thinkingBlocks = m.thinking?.length ? m.thinking : undefined;
      out.push({
        id: ++_msgId,
        role: "agent",
        text: m.content,
        historyToolNames,
        images,
        thinkingBlocks,
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
  /** Reseeded per turn: the working quip changes between turns but holds across a remount. */
  readonly turnSeed: number;
  readonly loadingSession: boolean;
  readonly sessionId: string | undefined;
  /** Monotonic. Anything that must happen once per finished turn keys on it. */
  readonly completedTurns: number;
}

/** App state and dispatch from `AppContextProvider`; the module-scope driver has neither. */
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
  /** Bumped per run; writes check it, so an old turn can't write into a switched conversation. */
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

/** Snapshot built on mutation: `useSyncExternalStore` needs a stable `getSnapshot` result. */
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
 * Land in the thread, not the wall, while a turn runs or its answer is unseen. Both conditions
 * self-clear via `acknowledgeCompletion`; deliberately no timer.
 */
export function hasLiveThread(): boolean {
  if (state.busy) return true;
  if (state.completedTurns > state.acknowledgedTurns) return true;
  // Read synchronously: surfaces pick a screen while mounting, before resumeActiveRun answers.
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
  // A bridge with the server up releases a queue held through an outage.
  drainQueue();
  return () => {
    // Identity check: under StrictMode a blind clear would null the second mount's bridge.
    if (state.bridge === bridge) state.bridge = null;
  };
}

// ── Object URLs ───────────────────────────────────────────────────────────────

/** Revoke store-created previews no surviving message shows; never history (http) URLs. */
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
  /** Composer preview object URLs; the store revokes them, since the bubble outlives the tray. */
  previewUrls?: string[];
}

/** Send, or queue while a reply streams. Attachments never queue: they belong to their turn. */
export function sendTurn(turn: SendTurn): void {
  if (state.busy) {
    if (!turn.text) return;
    state.queued = [...state.queued, turn.text];
    commit();
    return;
  }
  void runTurn(turn);
}

/** Drain one queued message. Module-level, not an effect, so the queue survives leaving Chat. */
function drainQueue(): void {
  if (state.busy || state.queued.length === 0) return;
  // Held, not dropped: the bridge reinstalling after an outage calls this again.
  if (!state.bridge?.serverOnline) return;
  const [next, ...rest] = state.queued;
  state.queued = rest;
  commit();
  void runTurn({ text: next });
}

interface TurnCtx {
  /** True once the conversation has moved on and this run must stop writing. */
  stale: () => boolean;
  userMsgId: number;
  agentMsgId: number;
}

/** Folds one turn's frames, for started and reattached turns alike, so the two can't disagree. */
async function consume(
  stream: AsyncGenerator<unknown>,
  ctx: TurnCtx,
): Promise<void> {
  for await (const event of stream) {
    if (ctx.stale()) return;
    const ev = event as ChatEvent;

    // Read position, so a reattach asks only for the rest.
    if (typeof ev.seq === "number") {
      state.lastSeq = ev.seq;
      rememberRun();
    }

    if (ev.type === "run_started") {
      // The server mints the session id up front; adopting it now lets a reload mid-turn resume.
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
      // Bookkeeping only; `cancelled` is followed by a `done` carrying `interrupted`.
      continue;
    }
    if (ev.type === "replay_gap" || ev.type === "run_evicted") {
      // Missed frames can't be replayed; reload the session rather than show a partial answer.
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
          // The server streams on past an error: start a new bubble, don't append to the error.
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
        // First chunk opens the span, each chunk moves the close: reasoning time, not turn time.
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
      // The stream carries no persisted ids, which the message actions need: fetch the session tail
      // and attach by local id, so a mid-fetch session switch can't misattribute them.
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
          // Non-fatal: only the action icons stay disabled until the next history load.
        }
      })();
    } else if (ev.type === "turn_stats") {
      // By id, not position: a mid-stream session switch replaces `messages` with another history.
      const stats = ev as unknown as TurnStats;
      mutate((prev) =>
        prev.map((m) =>
          m.id === ctx.agentMsgId ? { ...m, turnStats: stats } : m,
        ),
      );
    } else if (ev.type === "turn_limit_reached") {
      // Out of turns, not finished: mark it (by id) so it offers a Continue action.
      const limit = ev.max_turns ?? 0;
      mutate((prev) =>
        prev.map((m) =>
          m.id === ctx.agentMsgId ? { ...m, turnLimit: limit } : m,
        ),
      );
    } else if (ev.type === "subagent_progress") {
      // By id; folded through the shared reducer so this surface and the hub agree.
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
      // The context window is filling; attach by id, like turn_stats.
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

  // Claimed before the first await, so two sends in one tick can't both start a run.
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
        // Resumable: the run outlives this window, which the reattach path depends on.
        true,
      ),
      ctx,
    );
  } catch (e) {
    if (stale()) return;
    // Also logged: with no surface mounted, the bubble goes unread.
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
      // Microtask, so a run rejecting before its first await can't recurse on this stack.
      queueMicrotask(drainQueue);
    }
  }
}

// ── Surviving a reload ────────────────────────────────────────────────────────

/** localStorage key for the run pointer: no content, only enough to ask what's still running. */
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
    // Storage unavailable: losing the pointer costs a resume, not the answer.
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
 * Reattach to a turn started by a previous window: history from the DB, the in-flight answer
 * from the run's replay. Resolves whether anything resumed; failures are quiet.
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

  // Run gone: the pointer is stale, but still open the thread `hasLiveThread` already chose.
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
    // Finished unseen: the answer is persisted, so just open the thread on it.
    forgetRun();
    await openSession(pointer.sessionId);
    state.completedTurns += 1;
    commit();
    return true;
  }

  // Committed history, including the question; the in-flight answer isn't persisted yet.
  await openSession(pointer.sessionId);

  const runId = ++state.runSeq;
  const stale = () => runId !== state.runSeq;
  state.busy = true;
  state.turnSeed = Date.now();
  state.inThinkBlock = false;
  state.runId = pointer.runId;
  state.epoch = pointer.epoch;
  state.lastSeq = pointer.lastSeq;

  // Only the agent bubble: the question came with the history above.
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
    // From lastSeq, not 0, so frames the last window already read are not replayed.
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
 * Forget the run and cancel it server-side: a detached run otherwise generates to completion
 * for nobody. Fire-and-forget; not called on unload, so a closed window can still resume.
 */
function stopServerRun(): void {
  const runId = state.runId;
  forgetRun();
  if (!runId) return;
  void api.cancelRun(runId).catch((e) => {
    console.warn("Could not stop the run server-side (non-fatal):", e);
  });
}

/** The only cancel: a detached run outlives its reader. Local state clears even if this fails. */
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
 * Open a conversation from its persisted history. `stopCurrentRun` is for leaving a turn; resume
 * and replay-gap recovery only re-read, so it defaults off (only the wall passes `true`).
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
 * Follow a session id set elsewhere (deep link, `session-created`). No-op when it already
 * matches (the driver set it on `done`) or while a turn runs.
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
  stopServerRun();
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

/** Resets module state, which outlives a test file's render/cleanup. */
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
