import { useState, useRef, useEffect, useCallback } from "react";
import { Button, Chip } from "@heroui/react";
import { ArrowUp, Cpu, Mic, Paperclip, Zap } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { nextCardId } from "../state/reducer";
import type { ContextCard as ContextCardType } from "../state/reducer";
import { ContextCard } from "../components/ContextCard";
import { ThinkingPlaceholder } from "../components/ThinkingPlaceholder";
import type { ChatEvent } from "../api/types";
import { filterThinking } from "../lib/thinkFilter";

// Human-readable tool status for the chat bubble while a tool runs.
function friendlyToolStatus(rawName: string): string {
  const bare = rawName.includes("__") ? rawName.split("__").pop()! : rawName;
  const map: Record<string, string> = {
    get_current_weather: "Checking the weather…",
    list_registered_devices: "Looking up your devices…",
    recall_memories: "Recalling what I know…",
    save_memory: "Saving that for later…",
    list_schedules: "Looking up your schedules…",
    get_recipe: "Finding that recipe…",
    get_user_profile: "Looking up your profile…",
    list_skills: "Checking my skills…",
  };
  if (map[bare]) return map[bare];
  const pretty = bare.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  return `Working on: ${pretty}…`;
}

interface Message {
  id: number;
  role: "user" | "agent";
  text: string;
  streaming?: boolean;
  status?: string;       // current activity description (e.g. "Thinking...", "Using tool...")
  cards?: ContextCardType[];  // inline tool call results attached to this message
  modelRole?: string;         // which role answered (chat/think/task)
  tokenUsage?: { prompt_tokens: number; completion_tokens: number };
  error?: boolean;            // true when this bubble represents an error
}

let msgId = 0;

export function Chat() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [loadingSession, setLoadingSession] = useState(false);
  const bottomRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const sessionIdRef = useRef<string | undefined>(state.sessionId ?? undefined);
  const inThinkBlockRef = useRef(false);

  // Keep sessionIdRef in sync with state. When the session id changes
  // externally (e.g. user clicked a Recent item on Dashboard), load
  // that session's messages.
  useEffect(() => {
    const newId = state.sessionId ?? undefined;
    if (newId === sessionIdRef.current) return;
    const wasExternal = !!newId;
    sessionIdRef.current = newId;
    if (!wasExternal) return;
    api.getSessionMessages(newId!)
      .then((msgs) => {
        setMessages(
          (msgs ?? []).map((m) => ({
            id: ++msgId,
            role: m.role === "user" ? ("user" as const) : ("agent" as const),
            text: m.content,
          })),
        );
      })
      .catch((err) => {
        console.warn("Could not load session history (non-fatal):", err);
      });
  }, [state.sessionId]);

  // Load most recent session on mount (once server is online)
  useEffect(() => {
    if (!state.serverOnline || messages.length > 0) return;
    setLoadingSession(true);
    api.listSessions()
      .then((sessions) => {
        if (sessions.length === 0) return;
        const latest = sessions[0];
        dispatch({ type: "SET_SESSION_ID", payload: latest.id });
        sessionIdRef.current = latest.id;
        return api.getSessionMessages(latest.id);
      })
      .then((msgs) => {
        if (!msgs || msgs.length === 0) return;
        setMessages(
          msgs.map((m) => ({
            id: ++msgId,
            role: m.role === "user" ? "user" : "agent",
            text: m.content,
          })),
        );
      })
      .catch((err) => {
        console.warn("Could not load session history (non-fatal):", err);
      })
      .finally(() => setLoadingSession(false));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.serverOnline]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  function newConversation() {
    setMessages([]);
    sessionIdRef.current = undefined;
    dispatch({ type: "SET_SESSION_ID", payload: null });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
    textareaRef.current?.focus();
  }

  const sendMessage = useCallback(async () => {
    const text = input.trim();
    if (!text || busy || !state.serverOnline) return;

    setInput("");
    // Reset textarea height
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }
    setBusy(true);

    inThinkBlockRef.current = false; // reset for new stream
    const userMsg: Message = { id: ++msgId, role: "user", text };
    const agentMsg: Message = { id: ++msgId, role: "agent", text: "", streaming: true };
    setMessages((prev) => [...prev, userMsg, agentMsg]);

    try {
      api.setToken(state.sessionToken);
      for await (const event of api.chatStream(text, sessionIdRef.current, state.sessionToken ?? undefined)) {
        const ev = event as ChatEvent;

        if (ev.type === "text" && (ev.content ?? ev.token)) {
          const raw = ev.content ?? ev.token ?? "";
          const [visible, newInBlock] = filterThinking(raw, inThinkBlockRef.current);
          inThinkBlockRef.current = newInBlock;
          if (visible) {
            setMessages((prev) => {
              const last = prev[prev.length - 1];
              if (!last || last.role !== "agent") return prev;
              return [...prev.slice(0, -1), { ...last, text: last.text + visible, status: undefined }];
            });
          }

        } else if (ev.type === "status" && ev.content) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, status: ev.content }];
          });

        } else if (ev.type === "tool_call" && ev.tool) {
          const card: ContextCardType = {
            id: nextCardId(),
            tool: ev.tool,
            data: (ev.result as Record<string, unknown>) ?? {},
            timestamp_ms: Date.now(),
          };
          dispatch({ type: "PUSH_CONTEXT_CARD", payload: card }); // keep for voice compat
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { 
              ...last, 
              cards: [...(last.cards ?? []), card],
              status: friendlyToolStatus(ev.tool)
            }];
          });

        } else if (ev.type === "tool_result" && ev.id) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent" || !last.cards) return prev;
            // Update the data for the specific card
            const newCards = last.cards.map(c => 
              // We don't have tool_call_id on ContextCardType yet, but we can match by tool name if it was the last one
              // or better: let's just update the last one for now or add id to card
              c.tool === ev.tool ? { ...c, data: { result: ev.content } } : c
            );
            return [...prev.slice(0, -1), { ...last, cards: newCards, status: undefined }];
          });

        } else if (ev.type === "error" || ev.error) {
          const errMsg = ev.error ?? "Unknown error from agent";
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, text: `Error: ${errMsg}`, streaming: false, error: true }];
          });

        } else if (ev.done && ev.session_id) {
          sessionIdRef.current = ev.session_id;
          dispatch({ type: "SET_SESSION_ID", payload: ev.session_id });
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), {
              ...last,
              ...(ev.model_role ? { modelRole: ev.model_role } : {}),
              ...(ev.usage && ev.usage.completion_tokens > 0 ? { tokenUsage: ev.usage } : {}),
            }];
          });
          if (ev.model_name && ev.model_role) {
            dispatch({
              type: "SET_LAST_RESPONSE_META",
              payload: {
                modelName: ev.model_name,
                modelRole: ev.model_role,
                completionTokens: ev.usage?.completion_tokens ?? 0,
              },
            });
          }
        }
      }
    } catch (e) {
      setMessages((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent") return prev;
        return [...prev.slice(0, -1), { ...last, text: `Error: ${String(e)}`, streaming: false, error: true }];
      });
    } finally {
      setMessages((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent") return prev;
        // Only clear streaming flag if not already cleared by error handler
        if (!last.streaming) return prev;
        return [...prev.slice(0, -1), { ...last, streaming: false }];
      });
      setBusy(false);
      textareaRef.current?.focus();
    }
  }, [input, busy, state.serverOnline, state.sessionToken, dispatch]);

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      sendMessage();
    }
  }

  function onInput(e: React.ChangeEvent<HTMLTextAreaElement>) {
    setInput(e.target.value);
    const el = e.target;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }

  const modelLabel = state.lastResponseMeta?.modelName ?? "local model";

  return (
    <div className="screen screen--chat">
      {/* Chat Toolbar */}
      <div className="chat-toolbar">
        <div className="chat-toolbar__left">
          <h1 className="page-header__title chat-toolbar__title">Chat</h1>
          <Chip size="sm" variant="soft">
            <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
              <Cpu size={12} /> {modelLabel}
            </span>
          </Chip>
        </div>
        <div className="chat-toolbar__right">
          <Button
            size="sm"
            variant="ghost"
            onPress={newConversation}
            aria-label="New conversation"
          >
            New chat
          </Button>
          <Button size="sm" variant="outline">
            <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
              <Zap size={14} /> Tools
            </span>
          </Button>
        </div>
      </div>

      {/* Messages */}
      <div className="chat-body" role="log" aria-live="polite">
        <div className="chat-thread">
          {loadingSession && (
            <div style={styles.skeleton} aria-busy="true" aria-label="Loading conversation">
              {[88, 64, 72].map((w, i) => (
                <div key={i} style={{ ...styles.skeletonRow, alignSelf: i % 2 === 0 ? "flex-start" : "flex-end" }}>
                  <div style={{ ...styles.skeletonLine, width: `${w}%`, height: "14px", marginBottom: "6px" }} />
                  <div style={{ ...styles.skeletonLine, width: `${Math.round(w * 0.65)}%`, height: "14px" }} />
                </div>
              ))}
            </div>
          )}
          {!loadingSession && messages.length === 0 && (
            <div className="empty-state">
              <p style={styles.emptyTitle}>Start a conversation</p>
              <p style={styles.emptyHint}>Ask Pond anything. Type a message or use voice mode.</p>
            </div>
          )}
          {messages.map((msg) => (
            <div
              key={msg.id}
              className={`bubble ${msg.role === "user" ? "bubble--user" : "bubble--assistant"}`}
            >
              <div className="bubble__author">
                {msg.role === "user" ? "You" : "Pond"}
              </div>
              <div
                className="bubble__body"
                style={{
                  userSelect: "text",
                  wordBreak: "break-word",
                  ...(msg.error ? styles.bubbleError : {}),
                }}
              >
                {msg.text || (msg.streaming ? (
                  msg.status
                    ? <ThinkingPlaceholder status={msg.status} />
                    : <span className="stream-dots"><span /><span /><span /></span>
                ) : "")}
              </div>

              {/* Inline tool-call ContextCards intentionally NOT rendered in
               * the chat thread. They were leaking the agent's plumbing
               * (raw "Get Recipe" / "Get Weather" chips with `{}` JSON
               * underneath) every time the model invoked a tool. The
               * canvas overlay still receives cards via PUSH_CONTEXT_CARD
               * for voice mode, so that flow is unaffected. */}

              {/* Model role badge + token count */}
              {msg.role === "agent" && msg.modelRole && !msg.streaming && (
                <span style={styles.modelRoleBadge}>
                  {msg.modelRole}
                  {msg.tokenUsage && msg.tokenUsage.completion_tokens > 0 && (
                    <> · {msg.tokenUsage.completion_tokens} tokens</>
                  )}
                </span>
              )}
            </div>
          ))}
          <div ref={bottomRef} />
        </div>
      </div>

      {/* Composer */}
      <div className="chat-composer">
        <div className="chat-composer__inner">
          <textarea
            ref={textareaRef}
            className="chat-composer__field"
            style={styles.textarea}
            value={input}
            onChange={onInput}
            onKeyDown={onKeyDown}
            placeholder="Message Pond..."
            disabled={!state.serverOnline || busy}
            aria-label="Message input"
          />
          <div className="chat-composer__actions">
            <Button isIconOnly size="sm" variant="ghost">
              <Paperclip size={16} />
            </Button>
            <Button isIconOnly size="sm" variant="ghost">
              <Mic size={16} />
            </Button>
            <Button
              isIconOnly
              size="sm"
              variant="secondary"
              isDisabled={!input.trim() || !state.serverOnline || busy}
              onPress={sendMessage}
              aria-label="Send message"
            >
              <ArrowUp size={16} />
            </Button>
          </div>
        </div>
        <div className="chat-composer__hint">
          <span>Ctrl/Cmd + Enter to send</span>
          <span>&middot;</span>
          <span>Up arrow to edit last message</span>
        </div>
      </div>
    </div>
  );
}

/* Residual inline styles for elements not fully covered by CSS classes */
const styles: Record<string, React.CSSProperties> = {
  emptyTitle: {
    fontFamily: "var(--font-heading)",
    fontWeight: 700,
    fontSize: "var(--text-lg)",
    color: "var(--fg)",
    margin: 0,
  },
  emptyHint: {
    fontSize: "var(--text-sm)",
    color: "var(--grey-500)",
    margin: 0,
  },
  bubbleError: {
    borderColor: "var(--color-destructive)",
    color: "var(--color-destructive)",
  },
  cardList: {
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    maxWidth: "360px",
    width: "100%",
  },
  modelRoleBadge: {
    fontSize: "10px",
    color: "var(--grey-500)",
    fontFamily: "var(--font-mono)",
    paddingTop: "2px",
  },
  textarea: {
    flex: 1,
    border: "none",
    background: "transparent",
    resize: "none",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    color: "var(--fg)",
    lineHeight: 1.55,
    outline: "none",
    padding: "8px 12px",
    minHeight: "36px",
    maxHeight: "120px",
    overflowY: "auto",
    userSelect: "text",
  },
  skeleton: {
    display: "flex",
    flexDirection: "column",
    gap: "20px",
    padding: "16px 0",
  },
  skeletonRow: {
    display: "flex",
    flexDirection: "column",
    gap: "4px",
    maxWidth: "70%",
  },
  skeletonLine: {
    borderRadius: "var(--radius-md)",
    background: "var(--grey-200)",
    animation: "shimmer 1.4s ease-in-out infinite",
  },
};
