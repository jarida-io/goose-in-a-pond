import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import { friendlyToolName } from "../components/toolNames";
import { filterThinking } from "../lib/thinkFilter";

export type TestStatus = "idle" | "running" | "done" | "failed" | "stopped";

export interface TestRunState {
  status: TestStatus;
  text: string;
  /** Tools the run called, in order, named the way the chat names them. */
  tools: string[];
  error: string | null;
  seconds: number | null;
  /** The conversation the run left in chat history, until it is removed. */
  sessionId: string | null;
  removed: boolean;
  removeError: string | null;
}

const IDLE: TestRunState = {
  status: "idle",
  text: "",
  tools: [],
  error: null,
  seconds: null,
  sessionId: null,
  removed: false,
  removeError: null,
};

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Streams `/recipes/{name}/run` for one saved recipe. Stopping hangs up, and cancels the run if it has an id. */
export function useTestRun(name: string) {
  const [state, setState] = useState<TestRunState>(IDLE);
  const stopped = useRef(false);
  const alive = useRef(true);
  const runId = useRef<string | null>(null);
  const generation = useRef(0);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const run = useCallback(async () => {
    stopped.current = false;
    runId.current = null;
    const mine = ++generation.current;
    // A stopped run only notices at its next event, so a newer run must be able to outlive it.
    const cancelled = () => stopped.current || !alive.current || generation.current !== mine;
    const started = Date.now();
    const tools: string[] = [];
    let text = "";
    let insideThought = false;
    let sessionId: string | null = null;
    setState({ ...IDLE, status: "running" });
    try {
      for await (const event of api.runRecipe(name)) {
        if (cancelled()) break;
        if (event.type === "run_started" && event.run_id) runId.current = event.run_id;
        if (event.type === "text") {
          const [visible, inside] = filterThinking(event.content ?? event.token ?? "", insideThought);
          insideThought = inside;
          text += visible;
          setState((s) => ({ ...s, text }));
        } else if (event.type === "tool_call" && event.tool) {
          tools.push(friendlyToolName(event.tool));
          setState((s) => ({ ...s, tools: [...tools] }));
        } else if (event.type === "error") {
          throw new Error(event.error ?? "The run ended with an error.");
        } else if (event.type === "done") {
          sessionId = event.session_id ?? sessionId;
        }
      }
      if (cancelled()) return;
      setState((s) => ({ ...s, status: "done", sessionId, seconds: (Date.now() - started) / 1000 }));
    } catch (e) {
      if (cancelled()) return;
      setState((s) => ({ ...s, status: "failed", error: messageOf(e) }));
    }
  }, [name]);

  const stop = useCallback(() => {
    stopped.current = true;
    setState((s) => (s.status === "running" ? { ...s, status: "stopped" } : s));
    const id = runId.current;
    if (id) void api.cancelRun(id).catch(() => {});
  }, []);

  const removeConversation = useCallback(async () => {
    const id = state.sessionId;
    if (!id) return;
    try {
      await api.deleteSession(id);
      if (alive.current) setState((s) => ({ ...s, sessionId: null, removed: true, removeError: null }));
    } catch (e) {
      if (alive.current) setState((s) => ({ ...s, removeError: messageOf(e) }));
    }
  }, [state.sessionId]);

  return { state, run, stop, removeConversation };
}
