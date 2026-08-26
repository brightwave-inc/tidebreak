import type { CodeEvent, SequencedCodeEventFrame } from "../generated/wire";
import { resumeAfter, shouldApplyDurable } from "./cursor";

export type TimelineItem =
  | { kind: "user"; id: string; text: string }
  | { kind: "assistant"; id: string; text: string; streaming: boolean }
  | { kind: "tool"; id: string; callId: string; name: string; summary: string }
  | { kind: "status"; id: string; text: string };

export type TranscriptState = {
  lastSeq: number;
  items: TimelineItem[];
  assistantBuffer: string;
  tools: Record<string, { name: string; summary: string }>;
};

export function initialTranscript(): TranscriptState {
  return { lastSeq: 0, items: [], assistantBuffer: "", tools: {} };
}

export function reduceTranscript(
  state: TranscriptState,
  frame: SequencedCodeEventFrame,
): TranscriptState {
  if (!shouldApplyDurable(state.lastSeq, frame)) {
    return state;
  }
  const lastSeq = resumeAfter(state.lastSeq, frame);
  return applyEvent({ ...state, lastSeq }, frame.event, frame);
}

function applyEvent(
  state: TranscriptState,
  event: CodeEvent,
  frame: SequencedCodeEventFrame,
): TranscriptState {
  switch (event.type) {
    case "session_started":
      return appendStatus(
        state,
        `session:${state.lastSeq}`,
        `Session started (${event.harness_kind}${event.harness_version ? ` ${event.harness_version}` : ""})`,
      );
    case "turn_started":
      return appendStatus(
        { ...state, assistantBuffer: "" },
        `turn:${event.turn_id}`,
        "Turn started",
      );
    case "assistant_delta": {
      const text =
        frame.replacement === true
          ? event.text
          : state.assistantBuffer + event.text;
      return {
        ...state,
        assistantBuffer: text,
        items: upsertAssistant(state.items, text, true),
      };
    }
    case "assistant_message":
      return {
        ...state,
        assistantBuffer: event.text,
        items: upsertAssistant(state.items, event.text, false),
      };
    case "tool_started": {
      const tools = {
        ...state.tools,
        [event.call_id]: { name: event.name, summary: toolSummary(event.detail) },
      };
      return {
        ...state,
        tools,
        items: upsertTool(state.items, event.call_id, tools[event.call_id]!),
      };
    }
    case "tool_completed": {
      const existing = state.tools[event.call_id];
      const name = existing?.name ?? "tool";
      const summary = event.preview?.trim()
        ? event.preview.trim()
        : (existing?.summary ?? event.outcome);
      const tools = {
        ...state.tools,
        [event.call_id]: { name, summary },
      };
      return {
        ...state,
        tools,
        items: upsertTool(state.items, event.call_id, tools[event.call_id]!),
      };
    }
    case "turn_completed":
      return appendStatus(state, `done:${state.lastSeq}`, "Turn completed");
    case "turn_failed":
      return appendStatus(
        state,
        `fail:${state.lastSeq}`,
        `Turn failed${event.error?.message ? `: ${event.error.message}` : ""}`,
      );
    case "turn_interrupted":
      return appendStatus(state, `int:${state.lastSeq}`, "Turn interrupted");
    case "approval_requested":
      return appendStatus(
        state,
        `appr:${event.approval_id}`,
        "Approval waiting",
      );
    case "approval_resolved":
      return appendStatus(
        state,
        `appr-done:${event.approval_id}`,
        `Approval ${event.decision.type}`,
      );
    case "user_steered":
      return {
        ...state,
        items: [
          ...state.items,
          { kind: "user", id: `steer:${state.lastSeq}`, text: event.text },
        ],
      };
    case "harness_notice":
      return appendStatus(state, `note:${state.lastSeq}`, event.message);
    case "attention_changed":
      return appendStatus(
        state,
        `att:${state.lastSeq}`,
        `Attention: ${event.state.type}`,
      );
    default:
      return state;
  }
}

function appendStatus(
  state: TranscriptState,
  id: string,
  text: string,
): TranscriptState {
  if (state.items.some((item) => item.id === id)) return state;
  return {
    ...state,
    items: [...state.items, { kind: "status", id, text }],
  };
}

function upsertAssistant(
  items: TimelineItem[],
  text: string,
  streaming: boolean,
): TimelineItem[] {
  const last = items[items.length - 1];
  const next: TimelineItem = {
    kind: "assistant",
    id: last?.kind === "assistant" ? last.id : `asst:${items.length}`,
    text,
    streaming,
  };
  if (last?.kind === "assistant") {
    return [...items.slice(0, -1), next];
  }
  return [...items, next];
}

function upsertTool(
  items: TimelineItem[],
  callId: string,
  tool: { name: string; summary: string },
): TimelineItem[] {
  const item: TimelineItem = {
    kind: "tool",
    id: `tool:${callId}`,
    callId,
    name: tool.name,
    summary: tool.summary,
  };
  const index = items.findIndex(
    (entry) => entry.kind === "tool" && entry.callId === callId,
  );
  if (index === -1) return [...items, item];
  const next = [...items];
  next[index] = item;
  return next;
}

function toolSummary(detail: unknown): string {
  if (!detail || typeof detail !== "object") return "";
  const record = detail as Record<string, unknown>;
  for (const key of ["cmd", "path", "query", "summary"]) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return typeof record.kind === "string" ? record.kind : "";
}

export function isSequencedCodeEventFrame(
  value: unknown,
): value is SequencedCodeEventFrame {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.seq === "number" &&
    Number.isFinite(record.seq) &&
    !!record.event &&
    typeof record.event === "object" &&
    typeof (record.event as { type?: unknown }).type === "string"
  );
}
