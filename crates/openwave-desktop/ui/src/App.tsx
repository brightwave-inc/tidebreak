import { useEffect, useRef, useState } from "react";
import {
  ApiClient,
  type Chat,
  type ModelInfo,
  type ProviderInfo,
  type ProviderKind,
  type SequencedEvent,
  type ServerInfo,
} from "./api";
import { resolveServerInfo } from "./boot";
import { Logomark } from "./Logomark";

type Msg =
  | { id: string; role: "user"; text: string }
  | { id: string; role: "assistant"; text: string }
  | { id: string; role: "system"; text: string }
  | {
      id: string;
      role: "approval";
      callId: string;
      summary: string;
      resolved?: boolean;
    }
  | { id: string; role: "error"; text: string };

let msgSeq = 0;
function nextId(): string {
  msgSeq += 1;
  return `m${msgSeq}`;
}

export default function App() {
  const [bootError, setBootError] = useState<string | null>(null);
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [client, setClient] = useState<ApiClient | null>(null);
  const [chat, setChat] = useState<Chat | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [status, setStatus] = useState("starting…");
  const socketRef = useRef<WebSocket | null>(null);
  const lastSeqRef = useRef(0);
  const assistantBufRef = useRef("");
  const scrollRef = useRef<HTMLDivElement | null>(null);

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
        const [catalog, providerList] = await Promise.all([
          client.listModels(),
          client.listProviders(),
        ]);
        if (cancelled) return;
        setModels(catalog.models);
        setProviders(providerList.providers);
        const created = await client.createChat(
          info.scratchDir,
          catalog.models[0]?.id,
        );
        if (cancelled) return;
        setChat(created);
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
    socketRef.current?.close();
    lastSeqRef.current = 0;
    const socket = client.openEvents(chat.id, 0, (event) => {
      handleEvent(event);
    });
    socket.onopen = () => setStatus((s) => `${s} · live`);
    socket.onerror = () => setStatus("websocket error");
    socketRef.current = socket;
    return () => {
      socket.close();
    };
  }, [client, chat?.id]);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [messages]);

  function handleEvent(framed: SequencedEvent) {
    if (framed.seq <= lastSeqRef.current) return;
    lastSeqRef.current = framed.seq;
    const event = framed.event;

    if (event.type === "turn_started") {
      assistantBufRef.current = "";
      setBusy(true);
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
      setMessages((prev) => {
        const copy = [...prev];
        if (copy[copy.length - 1]?.role === "assistant") {
          copy.pop();
        }
        return copy;
      });
      return;
    }

    if (event.type === "tool_call_started") {
      setMessages((prev) => [
        ...prev,
        {
          id: nextId(),
          role: "system",
          text: `tool ${event.name}`,
        },
      ]);
      return;
    }

    if (event.type === "approval_required") {
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

    if (event.type === "user_steered") {
      setMessages((prev) => [
        ...prev,
        { id: nextId(), role: "user", text: event.content },
      ]);
      return;
    }

    if (event.type === "turn_completed") {
      setBusy(false);
      return;
    }

    if (event.type === "turn_cancelled") {
      setBusy(false);
      setMessages((prev) => [
        ...prev,
        { id: nextId(), role: "system", text: "turn cancelled" },
      ]);
      return;
    }

    if (event.type === "turn_failed") {
      setBusy(false);
      setMessages((prev) => [
        ...prev,
        {
          id: nextId(),
          role: "error",
          text: event.error.message,
        },
      ]);
    }
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
    if (!client || !chat || !draft.trim() || busy) return;
    const content = draft.trim();
    const turnId = crypto.randomUUID();
    setDraft("");
    setMessages((prev) => [...prev, { id: nextId(), role: "user", text: content }]);
    setBusy(true);
    try {
      await client.postMessage(chat.id, turnId, content);
    } catch (err) {
      setBusy(false);
      setMessages((prev) => [
        ...prev,
        { id: nextId(), role: "error", text: String(err) },
      ]);
    }
  }

  async function onModelChange(modelId: string) {
    if (!client || !chat) return;
    const updated = await client.patchChatModel(chat.id, modelId || null);
    setChat(updated);
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
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <Logomark />
          OpenWave
        </div>
        <div className="topbar-actions">
          <span className="status">{status}</span>
          <button
            type="button"
            className="btn"
            onClick={() => setShowSettings((v) => !v)}
          >
            {showSettings ? "Hide providers" : "Providers"}
          </button>
        </div>
      </header>

      <div className={`main${showSettings ? " with-settings" : ""}`}>
        <section className="chat-pane">
          <div className="chat-meta">
            <label>
              Model{" "}
              <select
                value={chat.model ?? ""}
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
            <span title={chat.workspace_dir}>
              private scratch · {shortPath(chat.workspace_dir)}
            </span>
          </div>

          <div className="messages" ref={scrollRef}>
            {messages.length === 0 && (
              <div className="bubble system">
                Configure a provider, pick a model, then send a message.
              </div>
            )}
            {messages.map((m) => {
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
                  {m.text || (m.role === "assistant" && busy ? "…" : "")}
                </div>
              );
            })}
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
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void onSend();
                }
              }}
            />
            <button
              type="submit"
              className="btn btn-primary"
              disabled={busy || !draft.trim()}
            >
              Send
            </button>
          </form>
        </section>

        {showSettings && (
          <ProvidersPanel
            providers={providers}
            client={client}
            onChanged={() => void refreshCatalog()}
          />
        )}
      </div>
    </div>
  );
}

function shortPath(path: string): string {
  const parts = path.split(/[/\\]/).filter(Boolean);
  if (parts.length <= 2) return path;
  return `…/${parts.slice(-2).join("/")}`;
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
