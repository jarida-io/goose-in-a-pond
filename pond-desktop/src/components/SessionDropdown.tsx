import { useState, useRef, useEffect } from "react";
import { Button } from "@heroui/react";
import { Plus, MessageSquare, Pencil, Trash2, Check, X } from "lucide-react";
import type { SessionSummary } from "../api/types";

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h`;
  const days = Math.floor(hrs / 24);
  return `${days}d`;
}

/** Row label: the stored or server-derived title, else a short id slug; never the full id. */
function sessionLabel(s: SessionSummary): string {
  const t = s.title?.trim();
  if (t) return t;
  return `Session ${s.id.slice(0, 8)}`;
}

function groupSessions(sessions: SessionSummary[]) {
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterdayStart = todayStart - 86400000;

  const today: SessionSummary[] = [];
  const yesterday: SessionSummary[] = [];
  const older: SessionSummary[] = [];

  for (const s of sessions) {
    const t = new Date(s.updated_at).getTime();
    if (t >= todayStart) today.push(s);
    else if (t >= yesterdayStart) yesterday.push(s);
    else older.push(s);
  }

  return { today, yesterday, older };
}

interface Props {
  sessions: SessionSummary[];
  currentSessionId: string | null;
  onSelect: (id: string) => void;
  onNewChat: () => void;
  onRename: (id: string, title: string) => void | Promise<void>;
  onDelete: (id: string) => void | Promise<void>;
  isOpen: boolean;
  onClose: () => void;
}

export function SessionDropdown({
  sessions,
  currentSessionId,
  onSelect,
  onNewChat,
  onRename,
  onDelete,
  isOpen,
  onClose,
}: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const editInputRef = useRef<HTMLInputElement>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [isOpen, onClose]);

  useEffect(() => {
    if (!isOpen) {
      setEditingId(null);
      setConfirmDeleteId(null);
    }
  }, [isOpen]);

  useEffect(() => {
    if (editingId) {
      editInputRef.current?.focus();
      editInputRef.current?.select();
    }
  }, [editingId]);

  if (!isOpen) return null;

  const { today, yesterday, older } = groupSessions(sessions);

  function beginRename(s: SessionSummary) {
    setConfirmDeleteId(null);
    setEditingId(s.id);
    setEditValue(sessionLabel(s));
  }

  function commitRename(id: string) {
    const next = editValue.trim();
    setEditingId(null);
    if (next) void onRename(id, next);
  }

  function cancelRename() {
    setEditingId(null);
    setEditValue("");
  }

  function confirmDelete(id: string) {
    setConfirmDeleteId(null);
    void onDelete(id);
  }

  function renderItem(s: SessionSummary) {
    const isActive = s.id === currentSessionId;
    const isEditing = editingId === s.id;
    const isConfirming = confirmDeleteId === s.id;
    const count = s.message_count ?? 0;

    if (isEditing) {
      return (
        <div key={s.id} className="session-dropdown__item is-editing">
          <MessageSquare size={13} style={{ flexShrink: 0, opacity: 0.5 }} />
          <input
            ref={editInputRef}
            className="session-dropdown__edit-input"
            value={editValue}
            onChange={(e) => setEditValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitRename(s.id);
              else if (e.key === "Escape") cancelRename();
            }}
            aria-label="Rename conversation"
          />
          <button
            className="session-dropdown__action"
            onClick={() => commitRename(s.id)}
            aria-label="Save name"
            title="Save"
            type="button"
          >
            <Check size={13} />
          </button>
          <button
            className="session-dropdown__action"
            onClick={cancelRename}
            aria-label="Cancel rename"
            title="Cancel"
            type="button"
          >
            <X size={13} />
          </button>
        </div>
      );
    }

    if (isConfirming) {
      return (
        <div key={s.id} className="session-dropdown__item is-confirming">
          <Trash2 size={13} style={{ flexShrink: 0, color: "var(--color-destructive)" }} />
          <span className="session-dropdown__title">Delete this conversation?</span>
          <button
            className="session-dropdown__action session-dropdown__action--danger"
            onClick={() => confirmDelete(s.id)}
            aria-label="Confirm delete"
            title="Delete"
            type="button"
          >
            <Check size={13} />
          </button>
          <button
            className="session-dropdown__action"
            onClick={() => setConfirmDeleteId(null)}
            aria-label="Cancel delete"
            title="Cancel"
            type="button"
          >
            <X size={13} />
          </button>
        </div>
      );
    }

    return (
      <div
        key={s.id}
        className={`session-dropdown__item${isActive ? " is-active" : ""}`}
      >
        <button
          className="session-dropdown__open"
          onClick={() => { onSelect(s.id); onClose(); }}
          type="button"
        >
          <MessageSquare size={13} style={{ flexShrink: 0, opacity: 0.5 }} />
          <span className="session-dropdown__title">{sessionLabel(s)}</span>
        </button>
        <span className="session-dropdown__meta">
          {count > 0 && (
            <span className="session-dropdown__count" title={`${count} messages`}>
              {count}
            </span>
          )}
          <span>{timeAgo(s.updated_at)}</span>
        </span>
        <span className="session-dropdown__actions">
          <button
            className="session-dropdown__action"
            onClick={() => beginRename(s)}
            aria-label="Rename conversation"
            title="Rename"
            type="button"
          >
            <Pencil size={13} />
          </button>
          <button
            className="session-dropdown__action session-dropdown__action--danger"
            onClick={() => { setEditingId(null); setConfirmDeleteId(s.id); }}
            aria-label="Delete conversation"
            title="Delete"
            type="button"
          >
            <Trash2 size={13} />
          </button>
        </span>
      </div>
    );
  }

  function renderGroup(label: string, items: SessionSummary[]) {
    if (items.length === 0) return null;
    return (
      <div className="session-dropdown__group">
        <div className="session-dropdown__group-label">{label}</div>
        {items.map(renderItem)}
      </div>
    );
  }

  return (
    <div ref={ref} className="session-dropdown">
      <div className="session-dropdown__header">
        <span style={{ fontWeight: 600, fontSize: 13 }}>Conversations</span>
        <Button size="sm" variant="ghost" onPress={() => { onNewChat(); onClose(); }}>
          <Plus size={14} /> New
        </Button>
      </div>
      <div className="session-dropdown__list">
        {sessions.length === 0 ? (
          <div style={{ padding: "16px 12px", color: "var(--grey-400)", fontSize: 13, textAlign: "center" }}>
            No conversations yet
          </div>
        ) : (
          <>
            {renderGroup("Today", today)}
            {renderGroup("Yesterday", yesterday)}
            {renderGroup("Older", older)}
          </>
        )}
      </div>
    </div>
  );
}
