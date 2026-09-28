import { useCallback, useEffect, useRef, useState } from "react";
import { apiJson } from "./api";

export type SessionRow = { id: string; title: string; updatedAt?: string; running: boolean };
export type ContentPart =
  | { type: "text"; text: string }
  | { type: "thinking"; thinking: string; redacted?: boolean }
  | { type: "image"; mimeType: string; data: string }
  | { type: "toolCall"; id: string; name: string; arguments: Record<string, unknown> };
export type Message = {
  role: "user" | "assistant" | "toolResult";
  content: string | ContentPart[];
  timestamp: number;
  toolCallId?: string;
  toolName?: string;
  isError?: boolean;
  stopReason?: string;
  errorMessage?: string;
};
export type QueueItem = {
  id: string;
  text: string;
  status: "pending" | "delivering" | "delivered" | "failed" | "cancelled";
  delivery: "queue" | "steer";
  images: { id: string; mimeType: string }[];
  sequence: number;
};
export type InboxSnapshot = { version: number; paused: boolean; items: QueueItem[] };
export type Question = { requestId: string; kind: "select" | "confirm" | "input" | "editor"; title: string; options?: string[]; message?: string; prefill?: string };
export type SessionDetail = {
  id: string;
  messages: Message[];
  entries: { id: string; message: Message }[];
  running: boolean;
  contextUsage?: { tokens: number | null; contextWindow: number; percent: number | null } | null;
  queue: InboxSnapshot;
  ui: Question[];
};

export function useSessions() {
  const [sessions, setSessions] = useState<SessionRow[]>([]);
  const [stage, setStage] = useState("");
  const [error, setError] = useState("");
  const refresh = useCallback(async () => {
    try {
      const state = await apiJson<{ sessions: SessionRow[]; stage: string }>("/api/state");
      setSessions(state.sessions);
      setStage(state.stage);
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "会话列表加载失败");
    }
  }, []);
  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    if (!stage || stage === "preview") return;
    const events = new EventSource("/api/events");
    events.onopen = () => { void refresh(); };
    events.addEventListener("state", () => { void refresh(); });
    events.addEventListener("reset", () => { void refresh(); });
    events.onerror = () => { setError("连接已断开。请确认 pi 仍在运行。"); };
    return () => events.close();
  }, [refresh, stage]);
  return { sessions, stage, error, refresh };
}

export function useSessionDetail(id: string | undefined) {
  const activeId = useRef(id);
  const requestSequence = useRef(0);
  activeId.current = id;
  const [detail, setDetail] = useState<SessionDetail>();
  const [streaming, setStreaming] = useState<Message>();
  const [error, setError] = useState("");
  const refresh = useCallback(async () => {
    if (!id) return;
    const sequence = ++requestSequence.current;
    try {
      const state = await apiJson<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`);
      if (activeId.current !== id || sequence !== requestSequence.current) return;
      setDetail(state);
      setError("");
    } catch (cause) {
      if (activeId.current !== id || sequence !== requestSequence.current) return;
      setError(cause instanceof Error ? cause.message : "无法读取会话");
    }
  }, [id]);
  useEffect(() => {
    setDetail(undefined);
    setStreaming(undefined);
    if (!id) return;
    void refresh();
    const events = new EventSource(`/api/sessions/${encodeURIComponent(id)}/events`);
    events.onopen = () => { setError(""); void refresh(); };
    events.addEventListener("session", (event) => {
      try {
        const change = JSON.parse((event as MessageEvent).data);
        if (change.type === "message_update" && change.message?.role === "assistant") setStreaming(change.message);
        if (change.type === "ui_requests") setDetail((state) => state ? { ...state, ui: change.questions } : state);
        if (["message_start", "message_end", "agent_end", "agent_start"].includes(change.type)) {
          if (change.type === "message_end" || change.type === "agent_end") setStreaming(undefined);
          void refresh();
        }
      } catch { void refresh(); }
    });
    events.addEventListener("queue", (event) => {
      try { const queue: InboxSnapshot = JSON.parse((event as MessageEvent).data); setDetail((state) => state ? { ...state, queue } : state); }
      catch { void refresh(); }
    });
    events.addEventListener("reset", () => { setStreaming(undefined); void refresh(); });
    events.onerror = () => { setError("事件流已断开。请确认 pi 仍在运行；恢复后页面会自动同步。"); };
    return () => events.close();
  }, [id, refresh]);
  return { detail, streaming, error, refresh, setDetail };
}
