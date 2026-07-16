import { useEffect, useRef, useState } from "react";
import {
  ApiClient,
  type AgentRun,
  type Chat,
  type ChatMessage,
  type ModelInfo,
  type PendingFolderAccessRequest,
  type ProviderInfo,
  type ProviderKind,
  type SequencedEvent,
  type ServerInfo,
  type WebSearchConfigInfo,
  type WebSearchCredentialReadiness,
  type WebSearchProviderKind,
} from "./api";
import { resolveServerInfo } from "./boot";
import {
  connectFolder,
  disconnectFolder,
  hasNativeHost,
  listConnectedFolders,
  resolveFolderAccessRequest,
  type ConnectedFolder,
  type FolderAccessDecision,
} from "./host";
import { FolderAccessCard } from "./FolderAccessCard";
import { Logomark } from "./Logomark";
import { MessageMarkdown } from "./MessageMarkdown";
import { ToolCallCard, type ToolCallStatus } from "./ToolCallCard";

type Msg =
  | { id: string; role: "user"; text: string }
  | { id: string; role: "assistant"; text: string }
  | { id: string; role: "system"; text: string }
  | {
      id: string;
      role: "tool";
      callId: string;
      name: string;
      status: ToolCallStatus;
    }
  | {
      id: string;
      role: "approval";
      callId: string;
      summary: string;
      resolved?: boolean;
    }
  | { id: string; role: "error"; text: string };

let msgSeq = 0;
const MAX_RECENT_TERMINAL_SANDBOX_RUNS = 2;

function nextId(): string {
  msgSeq += 1;
  return `m${msgSeq}`;
}

function messageFromSnapshot(message: ChatMessage): Msg {
  return {
    id: message.id,
    role: message.role,
    text: message.content,
  };
}

export default function App() {
  const [bootError, setBootError] = useState<string | null>(null);
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [client, setClient] = useState<ApiClient | null>(null);
  const [chat, setChat] = useState<Chat | null>(null);
  const [hydratedChatId, setHydratedChatId] = useState<string | null>(null);
  const [chats, setChats] = useState<Chat[]>([]);
  const [chatsError, setChatsError] = useState<string | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [agentRuns, setAgentRuns] = useState<AgentRun[]>([]);
  const [agentRunsLoading, setAgentRunsLoading] = useState(false);
  const [agentRunsError, setAgentRunsError] = useState<string | null>(null);
  const [folderAccessRequests, setFolderAccessRequests] = useState<
    PendingFolderAccessRequest[]
  >([]);
  const [resolvingFolderCalls, setResolvingFolderCalls] = useState<Set<string>>(
    new Set(),
  );
  const [folderAccessErrors, setFolderAccessErrors] = useState<
    Record<string, string>
  >({});
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [activeTurnId, setActiveTurnId] = useState<string | null>(null);
  const [cancelPendingTurnId, setCancelPendingTurnId] = useState<string | null>(
    null,
  );
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [creatingChat, setCreatingChat] = useState(false);
  const [deletingChatId, setDeletingChatId] = useState<string | null>(null);
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [savingTitle, setSavingTitle] = useState(false);
  const [settingsPanel, setSettingsPanel] = useState<
    "providers" | "web-search" | "folders" | null
  >(null);
  const [status, setStatus] = useState("starting…");
  const socketRef = useRef<WebSocket | null>(null);
  const socketGenerationRef = useRef(0);
  const chatSelectionRef = useRef(0);
  const hydratedMessageIdsRef = useRef<Set<string>>(new Set());
  const lastSeqRef = useRef(0);
  const assistantBufRef = useRef("");
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const refreshFolderAccessRef = useRef<(() => void) | null>(null);
  const refreshAgentRunsRef = useRef<(() => void) | null>(null);
  const resolvingFolderCallsRef = useRef<Set<string>>(new Set());
  const visibleFolderCallIdsRef = useRef<Set<string>>(new Set());
  const cancelRequestTurnRef = useRef<string | null>(null);
  const provisionalToolCallIdsRef = useRef<Set<string>>(new Set());
  const deletionInFlightRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const server = await resolveServerInfo();
        if (cancelled) return;
        setInfo(server);
        setClient(new ApiClient(server.baseUrl, server.token));
        setStatus(`connected ${server.baseUrl}`);
      } catch (err) {
        if (!cancelled) setBootError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!client || !info) return;
    let cancelled = false;
    (async () => {
      try {
        const [catalog, providerList, existingChats] = await Promise.all([
          client.listModels(),
          client.listProviders(),
          client.listChats(),
        ]);
        if (cancelled) return;
        setModels(catalog.models);
        setProviders(providerList.providers);
        setChats(existingChats);
        const created =
          existingChats[0] ??
          (await client.createChat(catalog.models[0]?.id));
        if (cancelled) return;
        if (existingChats.length === 0) setChats([created]);
        activateChat(created);
        setStatus(`chat ${created.id.slice(0, 8)}…`);
      } catch (err) {
        if (!cancelled) setBootError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, info]);

  useEffect(() => {
    if (!client || !chat) return;
    let cancelled = false;
    setHydratedChatId(null);
    setMessages([]);
    hydratedMessageIdsRef.current = new Set();
    (async () => {
      try {
        const transcript = await client.listChatMessages(chat.id);
        if (cancelled) return;
        lastSeqRef.current = transcript.last_event_seq;
        hydratedMessageIdsRef.current = new Set(
          transcript.messages.map((message) => message.id),
        );
        setMessages(transcript.messages.map(messageFromSnapshot));
        setHydratedChatId(chat.id);
      } catch (err) {
        if (!cancelled) {
          setMessages([
            {
              id: nextId(),
              role: "error",
              text: `Could not load this conversation: ${String(err)}`,
            },
          ]);
          setHydratedChatId(chat.id);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, chat?.id]);

  useEffect(() => {
    if (!client || !chat || hydratedChatId !== chat.id) return;
    socketRef.current?.close();
    const generation = ++socketGenerationRef.current;
    let disposed = false;
    let reconnectTimer: number | null = null;
    let reconnectDelayMs = 250;

    const scheduleReconnect = () => {
      if (
        disposed ||
        socketGenerationRef.current !== generation ||
        reconnectTimer !== null
      ) {
        return;
      }
      setStatus((s) => `${withoutConnectionState(s)} · reconnecting`);
      const delay = reconnectDelayMs;
      reconnectDelayMs = Math.min(reconnectDelayMs * 2, 5_000);
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    };

    const connect = () => {
      if (disposed || socketGenerationRef.current !== generation) return;
      let socket: WebSocket;
      try {
        socket = client.openEvents(chat.id, lastSeqRef.current, (event) => {
          if (socketGenerationRef.current !== generation) return;
          handleEvent(event);
        });
      } catch {
        scheduleReconnect();
        return;
      }
      socketRef.current = socket;
      socket.onopen = () => {
        if (disposed || socketGenerationRef.current !== generation) return;
        reconnectDelayMs = 250;
        setStatus((s) => `${withoutConnectionState(s)} · live`);
      };
      socket.onerror = () => {
        if (!disposed && socketGenerationRef.current === generation) {
          setStatus((s) => `${withoutConnectionState(s)} · reconnecting`);
        }
      };
      socket.onclose = () => {
        if (socketRef.current === socket) socketRef.current = null;
        scheduleReconnect();
      };
    };

    connect();
    return () => {
      disposed = true;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      socketRef.current?.close();
      socketRef.current = null;
      if (socketGenerationRef.current === generation) {
        socketGenerationRef.current += 1;
      }
    };
  }, [client, chat?.id, hydratedChatId]);

  useEffect(() => {
    if (!client || !chat) return;
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const runs = await client.listAgentRuns(chat.id);
        if (!cancelled && seq === requestSeq) {
          setAgentRuns(runs);
          setAgentRunsError(null);
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          setAgentRunsError(String(err));
        }
      } finally {
        if (!cancelled && seq === requestSeq) {
          setAgentRunsLoading(false);
        }
      }
    };

    setAgentRuns([]);
    setAgentRunsError(null);
    setAgentRunsLoading(true);
    refreshAgentRunsRef.current = () => void refresh();
    void refresh();
    return () => {
      cancelled = true;
      requestSeq += 1;
      if (refreshAgentRunsRef.current) {
        refreshAgentRunsRef.current = null;
      }
    };
  }, [client, chat?.id]);

  const hasActiveSandboxRun = agentRuns.some(
    (run) =>
      run.execution === "sandbox" &&
      ["queued", "running", "cancelling", "waiting", "retry_wait"].includes(
        run.status,
      ),
  );

  useEffect(() => {
    if (!hasActiveSandboxRun) return;
    const interval = window.setInterval(
      () => refreshAgentRunsRef.current?.(),
      5_000,
    );
    return () => window.clearInterval(interval);
  }, [hasActiveSandboxRun]);

  useEffect(() => {
    if (!client || !chat) return;
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const requests = await client.listPendingFolderAccessRequests(chat.id);
        if (!cancelled && seq === requestSeq) {
          setFolderAccessRequests(requests);
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          console.error("failed to refresh pending folder access", err);
          setFolderAccessRequests([]);
        }
      }
    };

    refreshFolderAccessRef.current = () => void refresh();
    void refresh();
    const interval = window.setInterval(() => void refresh(), 10_000);
    return () => {
      cancelled = true;
      requestSeq += 1;
      window.clearInterval(interval);
      if (refreshFolderAccessRef.current) {
        refreshFolderAccessRef.current = null;
      }
      setFolderAccessRequests([]);
    };
  }, [client, chat?.id]);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [messages]);

  useEffect(() => {
    const next = new Set(folderAccessRequests.map((request) => request.callId));
    const gainedRequest = [...next].some(
      (callId) => !visibleFolderCallIdsRef.current.has(callId),
    );
    visibleFolderCallIdsRef.current = next;
    if (gainedRequest) {
      scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
    }
  }, [folderAccessRequests]);

  function handleEvent(framed: SequencedEvent) {
    if (framed.seq <= lastSeqRef.current) return;
    lastSeqRef.current = framed.seq;
    const event = framed.event;

    if (event.type === "turn_started") {
      refreshAgentRunsRef.current?.();
      assistantBufRef.current = "";
      provisionalToolCallIdsRef.current = new Set();
      setBusy(true);
      setActiveTurnId(event.turn_id);
      setCancelPendingTurnId(null);
      setCancelError(null);
      cancelRequestTurnRef.current = null;
      setMessages((prev) => [
        ...prev,
        { id: nextId(), role: "assistant", text: "" },
      ]);
      return;
    }

    if (event.type === "text_delta") {
      assistantBufRef.current += event.text;
      const text = assistantBufRef.current;
      setMessages((prev) => {
        const copy = [...prev];
        const last = copy[copy.length - 1];
        if (last?.role === "assistant") {
          copy[copy.length - 1] = { id: last.id, role: "assistant", text };
        } else {
          copy.push({ id: nextId(), role: "assistant", text });
        }
        return copy;
      });
      return;
    }

    if (event.type === "stream_interrupted") {
      assistantBufRef.current = "";
      const provisionalCallIds = provisionalToolCallIdsRef.current;
      provisionalToolCallIdsRef.current = new Set();
      setMessages((prev) => {
        const copy = discardToolCalls(prev, provisionalCallIds);
        if (copy[copy.length - 1]?.role === "assistant") {
          copy.pop();
        }
        return copy;
      });
      return;
    }

    if (event.type === "tool_call_started") {
      refreshAgentRunsRef.current?.();
      if (event.name === "request_folder_access") {
        refreshFolderAccessRef.current?.();
      }
      provisionalToolCallIdsRef.current.add(event.call_id);
      setMessages((prev) =>
        upsertToolCall(prev, event.call_id, event.name, "running"),
      );
      return;
    }

    if (event.type === "tool_call_args_delta") {
      // Arguments are intentionally not retained in renderer state. They can
      // contain paths, file content, credentials, or provider-specific data.
      setMessages((prev) =>
        updateToolCall(prev, event.call_id, (tool) => ({
          ...tool,
          status: tool.status === "waiting_approval" ? tool.status : "running",
        })),
      );
      return;
    }

    if (event.type === "approval_required") {
      provisionalToolCallIdsRef.current.delete(event.call_id);
      setMessages((prev) =>
        updateToolCall(prev, event.call_id, (tool) => ({
          ...tool,
          status: "waiting_approval",
        })),
      );
      setMessages((prev) => [
        ...prev,
        {
          id: nextId(),
          role: "approval",
          callId: event.call_id,
          summary: event.summary,
        },
      ]);
      return;
    }

    if (event.type === "approval_decided") {
      setMessages((prev) =>
        updateApprovalAndToolCall(prev, event.call_id, event.approved),
      );
      return;
    }

    if (event.type === "tool_call_completed") {
      provisionalToolCallIdsRef.current.delete(event.call_id);
      setMessages((prev) =>
        updateToolCall(prev, event.call_id, (tool) => ({
          ...tool,
          status:
            tool.status === "cancelled"
              ? "cancelled"
              : toolOutputFailed(event.output)
                ? "failed"
                : "completed",
        })),
      );
      return;
    }

    if (event.type === "user_steered") {
      if (hydratedMessageIdsRef.current.has(event.message_id)) return;
      hydratedMessageIdsRef.current.add(event.message_id);
      setMessages((prev) => [
        ...prev,
        { id: event.message_id, role: "user", text: event.content },
      ]);
      return;
    }

    if (event.type === "turn_completed") {
      provisionalToolCallIdsRef.current = new Set();
      resolveActiveTurn();
      refreshAgentRunsRef.current?.();
      return;
    }

    if (event.type === "turn_cancelled") {
      provisionalToolCallIdsRef.current = new Set();
      resolveActiveTurn();
      refreshAgentRunsRef.current?.();
      setMessages((prev) => [
        ...settleActiveToolCalls(prev, "cancelled"),
        { id: nextId(), role: "system", text: "turn cancelled" },
      ]);
      return;
    }

    if (event.type === "turn_failed") {
      provisionalToolCallIdsRef.current = new Set();
      resolveActiveTurn();
      refreshAgentRunsRef.current?.();
      setMessages((prev) => [
        ...settleActiveToolCalls(prev, "failed"),
        {
          id: nextId(),
          role: "error",
          text: event.error.message,
        },
      ]);
    }
  }

  function resolveActiveTurn() {
    setBusy(false);
    setActiveTurnId(null);
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
  }

  async function refreshCatalog() {
    if (!client) return;
    const [catalog, providerList] = await Promise.all([
      client.listModels(),
      client.listProviders(),
    ]);
    setModels(catalog.models);
    setProviders(providerList.providers);
  }

  async function onSend() {
    if (!client || !chat || !draft.trim() || busy || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const content = draft.trim();
    const turnId = crypto.randomUUID();
    setDraft("");
    setMessages((prev) => [...prev, { id: nextId(), role: "user", text: content }]);
    setBusy(true);
    setActiveTurnId(turnId);
    setCancelPendingTurnId(null);
    setCancelError(null);
    try {
      await client.postMessage(chatId, turnId, content);
      if (chatSelectionRef.current !== selection) return;
      refreshAgentRunsRef.current?.();
    } catch (err) {
      if (chatSelectionRef.current !== selection) return;
      resolveActiveTurn();
      setMessages((prev) => [
        ...prev,
        { id: nextId(), role: "error", text: String(err) },
      ]);
    }
  }

  async function onCancelActiveTurn() {
    const turnId = activeTurnId;
    if (
      !client ||
      !chat ||
      !busy ||
      !turnId ||
      cancelRequestTurnRef.current === turnId
    ) {
      return;
    }
    const selection = chatSelectionRef.current;
    const chatId = chat.id;

    cancelRequestTurnRef.current = turnId;
    setCancelPendingTurnId(turnId);
    setCancelError(null);
    try {
      await client.cancel(chatId, turnId);
    } catch (err) {
      if (
        chatSelectionRef.current === selection &&
        cancelRequestTurnRef.current === turnId
      ) {
        cancelRequestTurnRef.current = null;
        setCancelPendingTurnId(null);
        setCancelError(String(err));
      }
    }
  }

  async function onNewChat() {
    if (!client || creatingChat || deletionInFlightRef.current) return;
    setCreatingChat(true);
    try {
      const created = await client.createChat(chat?.model ?? models[0]?.id);
      socketGenerationRef.current += 1;
      socketRef.current?.close();
      socketRef.current = null;
      assistantBufRef.current = "";
      provisionalToolCallIdsRef.current = new Set();
      lastSeqRef.current = 0;
      setMessages([]);
      hydratedMessageIdsRef.current = new Set();
      setAgentRuns([]);
      setAgentRunsError(null);
      setFolderAccessRequests([]);
      setFolderAccessErrors({});
      setDraft("");
      setBusy(false);
      setActiveTurnId(null);
      setCancelPendingTurnId(null);
      setCancelError(null);
      cancelRequestTurnRef.current = null;
      activateChat(created);
      setChats((current) => [created, ...current]);
      setChatsError(null);
      setStatus(`chat ${created.id.slice(0, 8)}…`);
    } catch (err) {
      setMessages((prev) => [
        ...prev,
        {
          id: nextId(),
          role: "error",
          text: `Could not create a chat: ${String(err)}`,
        },
      ]);
    } finally {
      setCreatingChat(false);
    }
  }

  async function onDeleteChat(target: Chat) {
    if (!client || deletionInFlightRef.current || creatingChat) return;
    const label = target.title?.trim() || "this conversation";
    if (!window.confirm(`Delete ${label}? This cannot be undone.`)) return;

    deletionInFlightRef.current = true;
    setDeletingChatId(target.id);
    setChatsError(null);
    const deletingSelectedChat = chat?.id === target.id;
    if (deletingSelectedChat) {
      // This invalidates callbacks that captured the deleted selection. The
      // ref update also gates sends before the disabled composer renders.
      chatSelectionRef.current += 1;
    }
    try {
      await client.deleteChat(target.id);
      let refreshed = await client.listChats();
      if (!deletingSelectedChat) {
        setChats(refreshed);
        return;
      }

      let next = refreshed[0];
      if (!next) {
        next = await client.createChat(models[0]?.id);
        refreshed = await client.listChats();
      }
      setChats(refreshed);
      selectChat(next, true);
    } catch (err) {
      setChatsError(`Could not delete conversation: ${String(err)}`);
    } finally {
      deletionInFlightRef.current = false;
      setDeletingChatId(null);
    }
  }

  function activateChat(next: Chat) {
    chatSelectionRef.current += 1;
    setChat(next);
  }

  function selectChat(next: Chat, force = false) {
    if (next.id === chat?.id || creatingChat || (!force && deletionInFlightRef.current)) return;
    socketGenerationRef.current += 1;
    socketRef.current?.close();
    socketRef.current = null;
    assistantBufRef.current = "";
    lastSeqRef.current = 0;
    setMessages([]);
    hydratedMessageIdsRef.current = new Set();
    setAgentRuns([]);
    setAgentRunsError(null);
    setFolderAccessRequests([]);
    setFolderAccessErrors({});
    setDraft("");
    setBusy(false);
    setActiveTurnId(null);
    setCancelPendingTurnId(null);
    setCancelError(null);
    cancelRequestTurnRef.current = null;
    setEditingTitle(false);
    activateChat(next);
    setStatus(`chat ${next.id.slice(0, 8)}…`);
  }

  async function onRenameChat() {
    if (!client || !chat || savingTitle || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    setSavingTitle(true);
    try {
      const updated = await client.patchChatTitle(chatId, titleDraft.trim() || null);
      setChats((current) =>
        current.map((item) => (item.id === updated.id ? updated : item)),
      );
      if (chatSelectionRef.current !== selection) return;
      setChat(updated);
      setEditingTitle(false);
    } catch (err) {
      if (chatSelectionRef.current !== selection) return;
      setChatsError(`Could not rename conversation: ${String(err)}`);
    } finally {
      setSavingTitle(false);
    }
  }

  async function onModelChange(modelId: string) {
    if (!client || !chat || deletionInFlightRef.current) return;
    const chatId = chat.id;
    const selection = chatSelectionRef.current;
    const updated = await client.patchChatModel(chatId, modelId || null);
    if (chatSelectionRef.current !== selection) {
      setChats((current) =>
        current.map((item) => (item.id === updated.id ? updated : item)),
      );
      return;
    }
    setChat(updated);
    setChats((current) =>
      current.map((item) => (item.id === updated.id ? updated : item)),
    );
  }

  async function onApproval(callId: string, decision: "approve" | "reject") {
    if (!client || !chat) return;
    await client.decideApproval(chat.id, callId, decision);
    setMessages((prev) =>
      prev.map((m) =>
        m.role === "approval" && m.callId === callId
          ? { ...m, resolved: true }
          : m,
      ),
    );
  }

  async function onFolderAccessDecision(
    callId: string,
    decision: FolderAccessDecision,
  ) {
    if (!chat || !hasNativeHost()) return;
    if (resolvingFolderCallsRef.current.size > 0) return;
    resolvingFolderCallsRef.current.add(callId);
    setResolvingFolderCalls((calls) => new Set(calls).add(callId));
    setFolderAccessErrors((errors) => {
      const next = { ...errors };
      delete next[callId];
      return next;
    });
    try {
      await resolveFolderAccessRequest(chat.id, callId, decision);
    } catch (err) {
      setFolderAccessErrors((errors) => ({
        ...errors,
        [callId]: String(err),
      }));
    } finally {
      resolvingFolderCallsRef.current.delete(callId);
      setResolvingFolderCalls((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      refreshFolderAccessRef.current?.();
    }
  }

  async function onFolderAccessCancel(callId: string, turnId: string) {
    if (!client || !chat || resolvingFolderCallsRef.current.size > 0) return;
    resolvingFolderCallsRef.current.add(callId);
    setResolvingFolderCalls((calls) => new Set(calls).add(callId));
    setFolderAccessErrors((errors) => {
      const next = { ...errors };
      delete next[callId];
      return next;
    });
    try {
      await client.cancel(chat.id, turnId);
    } catch (err) {
      setFolderAccessErrors((errors) => ({
        ...errors,
        [callId]: String(err),
      }));
    } finally {
      resolvingFolderCallsRef.current.delete(callId);
      setResolvingFolderCalls((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      refreshFolderAccessRef.current?.();
    }
  }

  if (bootError) {
    return (
      <div className="boot">
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>{bootError}</p>
      </div>
    );
  }

  if (!client || !chat) {
    return (
      <div className="boot">
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <p>{status}</p>
      </div>
    );
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="sidebar-brand">
          <Logomark />
          <span>OpenWave</span>
        </div>

        <button
          type="button"
          className="new-chat"
          onClick={() => void onNewChat()}
          disabled={creatingChat || deletingChatId !== null}
        >
          <span aria-hidden="true">+</span>
          {creatingChat ? "Starting…" : deletingChatId ? "Deleting…" : "New chat"}
        </button>

        <div className="sidebar-section">
          <span className="sidebar-label">Conversations</span>
          <div className="conversation-list" aria-label="Conversations">
            {chats.map((item) => (
              <div
                key={item.id}
                className={`conversation-row${item.id === chat.id ? " is-active" : ""}`}
              >
                <button
                  type="button"
                  className="conversation-item"
                  aria-current={item.id === chat.id ? "page" : undefined}
                  disabled={deletingChatId !== null || creatingChat}
                  onClick={() => selectChat(item)}
                >
                  <span className="conversation-dot" aria-hidden="true" />
                  <span>{item.title?.trim() || "New conversation"}</span>
                </button>
                <button
                  type="button"
                  className="conversation-delete"
                  aria-label={`Delete ${item.title?.trim() || "conversation"}`}
                  title="Delete conversation"
                  disabled={deletingChatId !== null || creatingChat}
                  onClick={() => void onDeleteChat(item)}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
          {chatsError && <p className="sidebar-error">{chatsError}</p>}
        </div>

        <div className="sidebar-footer">
          {hasNativeHost() && (
            <button
              type="button"
              className={`sidebar-action${settingsPanel === "folders" ? " is-active" : ""}`}
              onClick={() =>
                setSettingsPanel((panel) =>
                  panel === "folders" ? null : "folders",
                )
              }
            >
              Folders
            </button>
          )}
          <button
            type="button"
            className={`sidebar-action${settingsPanel === "providers" ? " is-active" : ""}`}
            onClick={() =>
              setSettingsPanel((panel) =>
                panel === "providers" ? null : "providers",
              )
            }
          >
            Providers
          </button>
          <button
            type="button"
            className={`sidebar-action${settingsPanel === "web-search" ? " is-active" : ""}`}
            onClick={() =>
              setSettingsPanel((panel) =>
                panel === "web-search" ? null : "web-search",
              )
            }
          >
            Web search
          </button>
        </div>
      </aside>

      <div className={`main${settingsPanel ? " with-settings" : ""}`}>
        <section className="chat-pane">
          <header className="conversation-header">
            <div>
              <p className="eyebrow">Conversation</p>
              {editingTitle ? (
                <form
                  className="conversation-title-editor"
                  onSubmit={(event) => {
                    event.preventDefault();
                    void onRenameChat();
                  }}
                >
                  <input
                    autoFocus
                    value={titleDraft}
                    aria-label="Conversation title"
                    onChange={(event) => setTitleDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") setEditingTitle(false);
                    }}
                  />
                  <button
                    type="submit"
                    className="btn"
                    disabled={savingTitle || deletingChatId !== null}
                  >
                    {savingTitle ? "Saving…" : "Save"}
                  </button>
                </form>
              ) : (
                <div className="conversation-title-row">
                  <h1>{chat.title?.trim() || "New conversation"}</h1>
                  <button
                    type="button"
                    className="title-action"
                    disabled={deletingChatId !== null}
                    onClick={() => {
                      setTitleDraft(chat.title ?? "");
                      setEditingTitle(true);
                    }}
                  >
                    Rename
                  </button>
                </div>
              )}
            </div>
            <div className="conversation-header-actions">
              <div className="mobile-settings-actions">
                {hasNativeHost() && (
                  <button
                    type="button"
                    className={`btn${settingsPanel === "folders" ? " is-active" : ""}`}
                    onClick={() =>
                      setSettingsPanel((panel) =>
                        panel === "folders" ? null : "folders",
                      )
                    }
                  >
                    Folders
                  </button>
                )}
                <button
                  type="button"
                  className={`btn${settingsPanel === "providers" ? " is-active" : ""}`}
                  onClick={() =>
                    setSettingsPanel((panel) =>
                      panel === "providers" ? null : "providers",
                    )
                  }
                >
                  Providers
                </button>
                <button
                  type="button"
                  className={`btn${settingsPanel === "web-search" ? " is-active" : ""}`}
                  onClick={() =>
                    setSettingsPanel((panel) =>
                      panel === "web-search" ? null : "web-search",
                    )
                  }
                >
                  Web search
                </button>
              </div>
              <span className="status" title={status}>
                {status}
              </span>
            </div>
          </header>

          <div className="chat-meta">
            <label>
              Model{" "}
              <select
                value={chat.model ?? ""}
                disabled={deletingChatId !== null}
                onChange={(e) => void onModelChange(e.target.value)}
              >
                <option value="">default</option>
                {models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.id} ({m.provider})
                  </option>
                ))}
                {chat.model && !models.some((m) => m.id === chat.model) && (
                  <option value={chat.model}>{chat.model} (custom)</option>
                )}
              </select>
            </label>
            <input
              className="model-custom"
              type="text"
              placeholder="or type a model id"
              defaultValue={
                chat.model && !models.some((m) => m.id === chat.model)
                  ? chat.model
                  : ""
              }
              key={chat.id}
              onBlur={(e) => {
                const next = e.target.value.trim();
                if (next && next !== (chat.model ?? "")) {
                  void onModelChange(next);
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
            />
            <AgentActivityPanel
              runs={agentRuns}
              loading={agentRunsLoading}
              error={agentRunsError}
              onRetry={() => refreshAgentRunsRef.current?.()}
            />
          </div>

          <div className="messages" ref={scrollRef}>
            {messages.length === 0 && folderAccessRequests.length === 0 && (
              <div className="bubble system">
                Configure a provider, pick a model, then send a message.
              </div>
            )}
            {messages.map((m) => {
              if (m.role === "tool") {
                return <ToolCallCard key={m.id} name={m.name} status={m.status} />;
              }
              if (m.role === "approval") {
                return (
                  <div key={m.id} className="bubble system">
                    <div>Approval needed: {m.summary}</div>
                    {!m.resolved && (
                      <div className="approval">
                        <button
                          type="button"
                          className="btn btn-primary"
                          onClick={() => void onApproval(m.callId, "approve")}
                        >
                          Approve
                        </button>
                        <button
                          type="button"
                          className="btn"
                          onClick={() => void onApproval(m.callId, "reject")}
                        >
                          Reject
                        </button>
                      </div>
                    )}
                  </div>
                );
              }
              return (
                <div key={m.id} className={`bubble ${m.role}`}>
                  {m.text ? (
                    m.role === "assistant" || m.role === "user" ? (
                      <MessageMarkdown>{m.text}</MessageMarkdown>
                    ) : (
                      m.text
                    )
                  ) : m.role === "assistant" && busy ? (
                    "…"
                  ) : (
                    ""
                  )}
                </div>
              );
            })}
            {folderAccessRequests.map((request) => (
              <FolderAccessCard
                key={request.callId}
                request={request}
                nativeHost={hasNativeHost()}
                nativeBusy={resolvingFolderCalls.size > 0}
                working={resolvingFolderCalls.has(request.callId)}
                error={folderAccessErrors[request.callId]}
                onDecision={(decision) =>
                  void onFolderAccessDecision(request.callId, decision)
                }
                onCancel={() =>
                  void onFolderAccessCancel(request.callId, request.turnId)
                }
              />
            ))}
          </div>

          <form
            className="composer"
            onSubmit={(e) => {
              e.preventDefault();
              void onSend();
            }}
          >
            <textarea
              value={draft}
              placeholder="Message OpenWave…"
              disabled={deletingChatId !== null}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void onSend();
                }
              }}
            />
            {busy && activeTurnId && (
              <div className="composer-turn-control" aria-live="polite">
                <button
                  type="button"
                  className="btn btn-stop"
                  disabled={cancelPendingTurnId === activeTurnId}
                  onClick={() => void onCancelActiveTurn()}
                >
                  {cancelPendingTurnId === activeTurnId
                    ? "Stopping…"
                    : "Stop"}
                </button>
                {cancelError && (
                  <span className="composer-turn-error" role="status">
                    Couldn’t stop turn: {cancelError}
                  </span>
                )}
              </div>
            )}
            <button
              type="submit"
              className="btn btn-primary"
              disabled={busy || !draft.trim() || deletingChatId !== null}
            >
              Send
            </button>
          </form>
        </section>

        {settingsPanel === "providers" && (
          <ProvidersPanel
            providers={providers}
            client={client}
            onChanged={() => void refreshCatalog()}
          />
        )}
        {settingsPanel === "web-search" && <WebSearchPanel client={client} />}
        {settingsPanel === "folders" && <FoldersPanel chat={chat} />}
      </div>
    </div>
  );
}

function upsertToolCall(
  messages: Msg[],
  callId: string,
  name: string,
  status: ToolCallStatus,
): Msg[] {
  const existing = messages.findIndex(
    (message) => message.role === "tool" && message.callId === callId,
  );
  if (existing >= 0) {
    return messages.map((message, index) =>
      index === existing && message.role === "tool" ? { ...message, status } : message,
    );
  }
  return [...messages, { id: nextId(), role: "tool", callId, name, status }];
}

function updateToolCall(
  messages: Msg[],
  callId: string,
  update: (tool: Extract<Msg, { role: "tool" }>) => Extract<Msg, { role: "tool" }>,
): Msg[] {
  return messages.map((message) =>
    message.role === "tool" && message.callId === callId ? update(message) : message,
  );
}

function updateApprovalAndToolCall(
  messages: Msg[],
  callId: string,
  approved: boolean,
): Msg[] {
  return messages.map((message) => {
    if (message.role === "approval" && message.callId === callId) {
      return { ...message, resolved: true };
    }
    if (message.role === "tool" && message.callId === callId) {
      return {
        ...message,
        status: approved ? "running" : "cancelled",
      };
    }
    return message;
  });
}

function settleActiveToolCalls(
  messages: Msg[],
  status: Extract<ToolCallStatus, "failed" | "cancelled">,
): Msg[] {
  const activeCallIds = new Set(
    messages.flatMap((message) =>
      message.role === "tool" &&
      (message.status === "running" || message.status === "waiting_approval")
        ? [message.callId]
        : [],
    ),
  );
  return messages.map((message) =>
    message.role === "tool" &&
    (message.status === "running" || message.status === "waiting_approval")
      ? { ...message, status }
      : message.role === "approval" &&
          !message.resolved &&
          activeCallIds.has(message.callId)
        ? { ...message, resolved: true }
      : message,
  );
}

function discardToolCalls(messages: Msg[], callIds: Set<string>): Msg[] {
  return messages.filter(
    (message) => message.role !== "tool" || !callIds.has(message.callId),
  );
}

function toolOutputFailed(output: unknown): boolean {
  if (!output || typeof output !== "object") return false;
  return (output as { is_error?: unknown }).is_error === true;
}

function withoutConnectionState(status: string): string {
  return status.replace(/ · (?:live|reconnecting)$/, "");
}

function AgentActivityPanel({
  runs,
  loading,
  error,
  onRetry,
}: {
  runs: AgentRun[];
  loading: boolean;
  error: string | null;
  onRetry: () => void;
}) {
  if (loading) {
    return <div className="agent-activity-state">Loading activity…</div>;
  }

  if (error) {
    return (
      <div className="agent-activity-state is-error" role="status">
        Activity unavailable
        <button type="button" className="agent-activity-retry" onClick={onRetry}>
          Retry
        </button>
      </div>
    );
  }

  const foreground = runs.find((run) => run.execution === "foreground");
  const sandboxes = runs.filter((run) => run.execution === "sandbox");
  if (!foreground && sandboxes.length === 0) return null;

  const activeSandboxes = sandboxes.filter((run) =>
    isActiveAgentRunStatus(run.status),
  );
  const terminalSandboxes = sandboxes
    .filter((run) => !isActiveAgentRunStatus(run.status))
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at));
  const recentTerminalSandboxes = terminalSandboxes.slice(
    0,
    MAX_RECENT_TERMINAL_SANDBOX_RUNS,
  );
  const hiddenTerminalCount = terminalSandboxes.length - recentTerminalSandboxes.length;
  const active = runs.some((run) => isActiveAgentRunStatus(run.status));
  return (
    <section
      className="agent-activity"
      aria-label="Agent activity"
      aria-live={active ? "polite" : "off"}
    >
      <div className="agent-activity-heading">
        <span>Activity</span>
        <span className="agent-activity-summary">
          {activeSandboxes.length > 0
            ? `${activeSandboxes.length} background ${activeSandboxes.length === 1 ? "task" : "tasks"}`
            : recentTerminalSandboxes.length > 0
              ? "Recent background work"
              : "No background work"}
        </span>
      </div>
      <ul className="agent-activity-list">
        {foreground && <AgentActivityItem run={foreground} label="Conversation" />}
        {activeSandboxes.map((run, index) => (
          <AgentActivityItem
            key={run.id}
            run={run}
            label={`Background task ${index + 1}`}
          />
        ))}
        {recentTerminalSandboxes.map((run, index) => (
          <AgentActivityItem
            key={run.id}
            run={run}
            label={`Recent background task ${index + 1}`}
          />
        ))}
      </ul>
      {hiddenTerminalCount > 0 && (
        <p className="agent-activity-history">
          {hiddenTerminalCount} earlier {hiddenTerminalCount === 1 ? "result" : "results"}
        </p>
      )}
    </section>
  );
}

function AgentActivityItem({ run, label }: { run: AgentRun; label: string }) {
  const activity = agentActivityPresentation(run.activity);
  const status = activity?.status ?? readableAgentRunStatus(run.status);
  return (
    <li
      className={`agent-activity-item is-${run.status}${activity ? ` is-activity-${activity.sourceStatus}` : ""}`}
    >
      <span className="agent-activity-indicator" aria-hidden="true" />
      <span className="agent-activity-item-copy">
        <span>{activity?.label ?? label}</span>
        <span className="agent-activity-detail">
          {activity?.detail ?? agentRunStatusDescription(run.status)}
        </span>
      </span>
      <strong>{status}</strong>
    </li>
  );
}

function agentActivityPresentation(
  activity: AgentRun["activity"],
): { label: string; detail: string; status: string; sourceStatus: string } | null {
  if (!activity) return null;

  switch (activity.kind) {
    case "web_search":
      switch (activity.status) {
        case "waiting":
          return {
            label: "Web search",
            detail: "Waiting to search",
            status: "waiting",
            sourceStatus: "waiting",
          };
        case "running":
          return {
            label: "Web search",
            detail: "Searching the web",
            status: "searching",
            sourceStatus: "running",
          };
      }
    case "list_connected_folders":
      switch (activity.status) {
        case "waiting":
          return {
            label: "Connected folders",
            detail: "Waiting to check connected folders",
            status: "waiting",
            sourceStatus: "waiting",
          };
        case "running":
          return {
            label: "Connected folders",
            detail: "Checking connected folders",
            status: "checking",
            sourceStatus: "running",
          };
      }
    case "list_folder":
      switch (activity.status) {
        case "waiting":
          return {
            label: "Folder",
            detail: "Waiting to list a folder",
            status: "waiting",
            sourceStatus: "waiting",
          };
        case "running":
          return {
            label: "Folder",
            detail: "Listing a folder",
            status: "listing",
            sourceStatus: "running",
          };
      }
    case "read_connected_file":
      switch (activity.status) {
        case "waiting":
          return {
            label: "File",
            detail: "Waiting to read a file",
            status: "waiting",
            sourceStatus: "waiting",
          };
        case "running":
          return {
            label: "File",
            detail: "Reading a file",
            status: "reading",
            sourceStatus: "running",
          };
      }
  }
}

function readableAgentRunStatus(status: AgentRun["status"]): string {
  switch (status) {
    case "retry_wait":
      return "retrying";
    case "cancelling":
      return "stopping";
    default:
      return status;
  }
}

function isActiveAgentRunStatus(status: AgentRun["status"]): boolean {
  return ["active", "queued", "running", "cancelling", "waiting", "retry_wait"].includes(
    status,
  );
}

function agentRunStatusDescription(status: AgentRun["status"]): string {
  switch (status) {
    case "active":
      return "Ready for this conversation";
    case "queued":
      return "Queued to start";
    case "running":
      return "Working in the background";
    case "cancelling":
      return "Stopping";
    case "waiting":
      return "Waiting to continue";
    case "retry_wait":
      return "Waiting to retry";
    case "completed":
      return "Finished";
    case "failed":
      return "Could not finish";
    case "cancelled":
      return "Stopped";
  }
}

function FoldersPanel({ chat }: { chat: Chat }) {
  const [folders, setFolders] = useState<ConnectedFolder[]>([]);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scopeLabel = chat.project_id ? "project" : "chat";

  async function refresh() {
    setError(null);
    try {
      setFolders(await listConnectedFolders(chat));
    } catch (err) {
      setError(String(err));
    }
  }

  useEffect(() => {
    void refresh();
  }, [chat.id, chat.project_id]);

  async function addFolder() {
    setWorking(true);
    setError(null);
    try {
      const connected = await connectFolder(chat);
      if (connected) await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function removeFolder(rootId: string) {
    setWorking(true);
    setError(null);
    try {
      await disconnectFolder(chat, rootId);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  return (
    <aside className="settings">
      <h2>Connected folders</h2>
      <p>
        OpenWave can read only folders you choose for this {scopeLabel}. Folder
        locations stay with the native host.
      </p>
      <button
        type="button"
        className="btn btn-primary"
        disabled={working}
        onClick={() => void addFolder()}
      >
        Choose folder…
      </button>
      <div className="folder-list">
        {folders.length === 0 && !error && (
          <div className="status">
            No folders connected to this {scopeLabel}.
          </div>
        )}
        {folders.map((folder) => (
          <div className="folder" key={folder.rootId}>
            <div>
              <strong>{folder.displayName}</strong>
              <div className="status">read access</div>
            </div>
            <button
              type="button"
              className="btn"
              disabled={working}
              onClick={() => void removeFolder(folder.rootId)}
            >
              Disconnect from {scopeLabel}
            </button>
          </div>
        ))}
      </div>
      {error && <div className="folder-error">{error}</div>}
    </aside>
  );
}

const MIN_WEB_SEARCH_TIMEOUT_MS = 1_000;
const MAX_WEB_SEARCH_TIMEOUT_MS = 60_000;

function WebSearchPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<WebSearchConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<WebSearchCredentialReadiness[]>([]);
  const [provider, setProvider] = useState<WebSearchProviderKind | "">("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [savingConfig, setSavingConfig] = useState(false);
  const [savingCredential, setSavingCredential] = useState(false);
  const [removingCredential, setRemovingCredential] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    const [nextConfig, nextCredentials] = await Promise.all([
      client.getWebSearchConfig(),
      client.listWebSearchCredentials(),
    ]);
    setConfig(nextConfig);
    setCredentials(nextCredentials.credentials);
    setProvider(nextConfig.provider ?? "");
    setTimeoutMs(String(nextConfig.timeout_ms));
  }

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const [nextConfig, nextCredentials] = await Promise.all([
          client.getWebSearchConfig(),
          client.listWebSearchCredentials(),
        ]);
        if (cancelled) return;
        setConfig(nextConfig);
        setCredentials(nextCredentials.credentials);
        setProvider(nextConfig.provider ?? "");
        setTimeoutMs(String(nextConfig.timeout_ms));
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  const activeProvider = config?.provider;
  const selectedCredential = activeProvider
    ? credentials.find((credential) => credential.provider === activeProvider)
    : undefined;
  const selectedHasCredential = selectedCredential?.has_credential ?? false;
  const working = savingConfig || savingCredential || removingCredential;
  const state = webSearchState(config);

  async function saveConfig() {
    const parsedTimeout = Number(timeoutMs);
    if (
      !Number.isInteger(parsedTimeout) ||
      parsedTimeout < MIN_WEB_SEARCH_TIMEOUT_MS ||
      parsedTimeout > MAX_WEB_SEARCH_TIMEOUT_MS
    ) {
      setError(
        `Timeout must be a whole number between ${MIN_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} and ${MAX_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} ms.`,
      );
      return;
    }

    setSavingConfig(true);
    setError(null);
    try {
      const nextConfig = await client.putWebSearchConfig({
        provider: provider || null,
        timeout_ms: parsedTimeout,
      });
      setConfig(nextConfig);
      setTimeoutMs(String(nextConfig.timeout_ms));
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingConfig(false);
    }
  }

  async function saveCredential() {
    if (!activeProvider || !apiKey.trim()) return;
    setSavingCredential(true);
    setError(null);
    try {
      await client.putWebSearchCredential(activeProvider, apiKey.trim());
      setApiKey("");
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingCredential(false);
    }
  }

  async function removeCredential() {
    if (!activeProvider) return;
    setRemovingCredential(true);
    setError(null);
    try {
      await client.deleteWebSearchCredential(activeProvider);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setRemovingCredential(false);
    }
  }

  return (
    <aside className="settings web-search-settings" aria-busy={loading}>
      <h2>Web search</h2>
      <p>
        Choose a local provider and a bounded request timeout. Existing keys are
        never shown here.
      </p>

      {loading ? (
        <div className="status">Loading web-search settings…</div>
      ) : !config ? (
        <div className="status">Web-search settings are unavailable.</div>
      ) : (
        <>
          <div className={`web-search-state is-${state.kind}`} role="status">
            <strong>{state.label}</strong>
            <span>{state.description}</span>
          </div>

          <div className="provider">
            <label className="settings-field">
              <span>Provider</span>
              <select
                value={provider}
                disabled={working}
                onChange={(event) => setProvider(event.target.value as WebSearchProviderKind | "")}
              >
                <option value="">Disabled</option>
                <option value="exa">Exa</option>
                <option value="tavily">Tavily</option>
              </select>
            </label>

            <label className="settings-field">
              <span>Request timeout (ms)</span>
              <input
                type="number"
                inputMode="numeric"
                min={MIN_WEB_SEARCH_TIMEOUT_MS}
                max={MAX_WEB_SEARCH_TIMEOUT_MS}
                step="1000"
                value={timeoutMs}
                disabled={working}
                onChange={(event) => setTimeoutMs(event.target.value)}
              />
            </label>
            <p className="settings-hint">
              Between {MIN_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} and {MAX_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} ms.
            </p>
            <div className="row">
              <button
                type="button"
                className="btn btn-primary"
                disabled={working}
                onClick={() => void saveConfig()}
              >
                {savingConfig ? "Saving…" : "Save configuration"}
              </button>
            </div>
          </div>

          {activeProvider && (
            <div className="provider">
              <h3>{activeProvider} credential</h3>
              <span className="status">
                {selectedHasCredential ? "credential saved" : "no credential saved"}
              </span>
              <label className="settings-field">
                <span>{selectedHasCredential ? "Replace API key" : "API key"}</span>
                <input
                  type="password"
                  placeholder="Paste a new API key"
                  value={apiKey}
                  maxLength={8_192}
                  autoComplete="new-password"
                  disabled={working}
                  onChange={(event) => setApiKey(event.target.value)}
                />
              </label>
              <div className="row">
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={working || !apiKey.trim()}
                  onClick={() => void saveCredential()}
                >
                  {savingCredential ? "Saving…" : selectedHasCredential ? "Update key" : "Save key"}
                </button>
                {selectedHasCredential && (
                  <button
                    type="button"
                    className="btn btn-danger"
                    disabled={working}
                    onClick={() => void removeCredential()}
                  >
                    {removingCredential ? "Removing…" : "Remove saved key"}
                  </button>
                )}
              </div>
            </div>
          )}

          {provider !== (activeProvider ?? "") && (
            <p className="settings-hint">
              Save the provider configuration before managing that provider’s key.
            </p>
          )}

          <p className="settings-note">
            This configures host-owned search access only. Search is not yet a
            chat tool in this build.
          </p>
        </>
      )}
      {error && <div className="folder-error" role="alert">{error}</div>}
    </aside>
  );
}

function webSearchState(config: WebSearchConfigInfo | null): {
  kind: "disabled" | "ready" | "not-configured";
  label: string;
  description: string;
} {
  if (!config?.provider) {
    return {
      kind: "disabled",
      label: "Disabled",
      description: "No web-search provider is selected.",
    };
  }
  if (config.has_credential) {
    return {
      kind: "ready",
      label: "Ready",
      description: `${config.provider} is selected and has a saved credential.`,
    };
  }
  return {
    kind: "not-configured",
    label: "Not configured",
    description: `${config.provider} is selected but needs an API key.`,
  };
}

function ProvidersPanel({
  providers,
  client,
  onChanged,
}: {
  providers: ProviderInfo[];
  client: ApiClient;
  onChanged: () => void;
}) {
  return (
    <aside className="settings">
      <h2>Providers</h2>
      <p>Keys stay on this machine. Enable a provider, then save a credential.</p>
      {providers.map((p) => (
        <ProviderRow key={p.kind} info={p} client={client} onChanged={onChanged} />
      ))}
    </aside>
  );
}

function ProviderRow({
  info,
  client,
  onChanged,
}: {
  info: ProviderInfo;
  client: ApiClient;
  onChanged: () => void;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const body: {
        enabled: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
      } = { enabled };
      if (info.kind === "openai_compatible") {
        body.base_url = baseUrl.trim() || null;
      }
      if (key.trim()) {
        body.credential = { type: "api_key", key: key.trim() };
      }
      await client.putProvider(info.kind as ProviderKind, body);
      setKey("");
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function clearCredential() {
    setSaving(true);
    setError(null);
    try {
      await client.deleteCredential(info.kind as ProviderKind);
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="provider">
      <h3>{info.kind.replaceAll("_", " ")}</h3>
      <div className="row">
        <label>
          <input
            type="checkbox"
            checked={info.enabled}
            disabled={saving}
            onChange={(e) => void save(e.target.checked)}
          />{" "}
          enabled
        </label>
        <span className="status">
          {info.has_credential ? "credential set" : "no credential"}
        </span>
      </div>
      {info.kind === "openai_compatible" && (
        <div className="row">
          <input
            type="text"
            placeholder="base URL (e.g. http://127.0.0.1:1234/v1)"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
        </div>
      )}
      <div className="row">
        <input
          type="password"
          placeholder="API key"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          autoComplete="off"
        />
        <button
          type="button"
          className="btn btn-primary"
          disabled={saving || !key.trim()}
          onClick={() => void save(true)}
        >
          Save
        </button>
        {info.has_credential && (
          <button
            type="button"
            className="btn"
            disabled={saving}
            onClick={() => void clearCredential()}
          >
            Clear
          </button>
        )}
      </div>
      {error && <div className="status">{error}</div>}
    </div>
  );
}
