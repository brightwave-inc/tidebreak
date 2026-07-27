import { useEffect, useState, type ReactNode } from "react";
import { Plus, RefreshCw, Trash2 } from "lucide-react";
import type {
  ApiClient,
  McpServerDefinition,
  McpServerInfo,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

const DEFAULT_TIMEOUT_MS = 60_000;
const MAX_TIMEOUT_MS = 3_600_000;

function emptyServer(index: number): McpServerInfo {
  return {
    name: `server_${index + 1}`,
    command: "",
    args: [],
    env: {},
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    request_timeout_ms: DEFAULT_TIMEOUT_MS,
    enabled: true,
    health: "initializing",
    tool_count: 0,
    diagnostic: null,
  };
}

type Transport = "stdio" | "http";

function transportOf(server: McpServerInfo): Transport {
  return server.url !== null ? "http" : "stdio";
}

/** Switching transports clears the other transport's fields so a saved
 * definition can never carry both. */
function transportFields(transport: Transport): Partial<McpServerInfo> {
  return transport === "http"
    ? {
        command: null,
        args: [],
        env: {},
        env_from: [],
        cwd: null,
        url: "",
        bearer_token_env: null,
      }
    : {
        command: "",
        url: null,
        bearer_token_env: null,
      };
}

function definition(server: McpServerInfo): McpServerDefinition {
  const { health: _, tool_count: __, diagnostic: ___, ...value } = server;
  return value;
}

export function McpPanel({ client }: { client: ApiClient }) {
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [reconnecting, setReconnecting] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void client
      .listMcpServers()
      .then((result) => {
        if (!cancelled) {
          setServers(result.servers);
          setDirty(false);
        }
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  function update(index: number, change: Partial<McpServerInfo>) {
    setDirty(true);
    setServers((current) =>
      current.map((server, itemIndex) =>
        itemIndex === index ? { ...server, ...change } : server,
      ),
    );
  }

  async function save() {
    setSaving(true);
    setError(null);
    try {
      const result = await client.putMcpServers(servers.map(definition));
      setServers(result.servers);
      setDirty(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function reconnect(name: string) {
    setReconnecting(name);
    setError(null);
    setServers((current) =>
      current.map((server) =>
        server.name === name
          ? { ...server, health: "reconnecting", diagnostic: null }
          : server,
      ),
    );
    try {
      const result = await client.reconnectMcpServer(name);
      setServers(result.servers);
    } catch (err) {
      setError(String(err));
      try {
        const result = await client.listMcpServers();
        setServers(result.servers);
      } catch {
        // Preserve the reconnect error; reopening Settings performs a full load.
      }
    } finally {
      setReconnecting(null);
    }
  }

  const working = saving || reconnecting !== null;

  return (
    <SettingsPanel
      title="MCP servers"
      description="Connect local stdio tool servers or remote HTTP endpoints without a shell or a desktop restart."
      busy={loading || working}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">
          Loading MCP servers…
        </p>
      ) : (
        <>
          {servers.length === 0 && (
            <SettingsSection>
              <p className="text-sm text-muted-foreground">
                No MCP servers configured. Add one to make its tools available
                to new conversations.
              </p>
            </SettingsSection>
          )}

          {servers.map((server, index) => (
            <SettingsSection
              key={index}
              title={server.name || `Server ${index + 1}`}
            >
              <div
                className={`web-search-state is-${healthTone(server.health)}`}
                role="status"
              >
                <strong>{healthLabel(server.health)}</strong>
                <span>
                  {server.health === "healthy"
                    ? `${server.tool_count} tool${server.tool_count === 1 ? "" : "s"} available to new turns.`
                    : server.diagnostic ??
                      "Save the configuration to verify this server."}
                </span>
              </div>

              <label className="flex items-center gap-2 text-sm font-medium">
                <input
                  type="checkbox"
                  checked={server.enabled}
                  disabled={working}
                  onChange={(event) =>
                    update(index, { enabled: event.target.checked })
                  }
                />
                Enabled
              </label>

              <SettingsField
                label="Namespace"
                hint="ASCII letters, numbers, underscores, and hyphens only."
              >
                <Input
                  value={server.name}
                  disabled={working}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(event) =>
                    update(index, { name: event.target.value })
                  }
                />
              </SettingsField>

              <FieldGroup label="Transport">
                <div className="flex gap-4" role="radiogroup" aria-label="Transport">
                  {(["stdio", "http"] as const).map((transport) => (
                    <label
                      key={transport}
                      className="flex items-center gap-2 text-sm"
                    >
                      <input
                        type="radio"
                        name={`transport-${index}`}
                        checked={transportOf(server) === transport}
                        disabled={working}
                        onChange={() =>
                          update(index, transportFields(transport))
                        }
                      />
                      {transport === "stdio"
                        ? "Local process (stdio)"
                        : "Remote endpoint (HTTP)"}
                    </label>
                  ))}
                </div>
              </FieldGroup>

              {transportOf(server) === "stdio" && (
                <>
                  <SettingsField
                    label="Executable"
                    hint="An executable path or command name. OpenWave never invokes a shell."
                  >
                    <Input
                      value={server.command ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="/absolute/path/to/server"
                      onChange={(event) =>
                        update(index, { command: event.target.value })
                      }
                    />
                  </SettingsField>

                  <StringListEditor
                    label="Arguments"
                    values={server.args}
                    disabled={working}
                    addLabel="Add argument"
                    onChange={(args) => update(index, { args })}
                  />

                  <SettingsField
                    label="Working directory"
                    hint="Optional. It does not grant the server any OpenWave folder capability."
                  >
                    <Input
                      value={server.cwd ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="Optional"
                      onChange={(event) =>
                        update(index, { cwd: event.target.value || null })
                      }
                    />
                  </SettingsField>
                </>
              )}

              {transportOf(server) === "http" && (
                <>
                  <SettingsField
                    label="Server URL"
                    hint="An http or https MCP endpoint. Credentials never go in the URL."
                  >
                    <Input
                      value={server.url ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="https://gateway.example/mcp/tools"
                      onChange={(event) =>
                        update(index, { url: event.target.value })
                      }
                    />
                  </SettingsField>

                  <SettingsField
                    label="Bearer token variable"
                    hint="Optional. Only the name is saved; the token is read from the host environment when connecting and never displayed."
                  >
                    <Input
                      value={server.bearer_token_env ?? ""}
                      disabled={working}
                      autoComplete="off"
                      spellCheck={false}
                      placeholder="GATEWAY_TOKEN"
                      onChange={(event) =>
                        update(index, {
                          bearer_token_env: event.target.value || null,
                        })
                      }
                    />
                  </SettingsField>
                </>
              )}

              <SettingsField
                label="Request timeout (ms)"
                hint={`A whole number from 1 to ${MAX_TIMEOUT_MS.toLocaleString()}.`}
              >
                <Input
                  type="number"
                  inputMode="numeric"
                  min={1}
                  max={MAX_TIMEOUT_MS}
                  value={server.request_timeout_ms}
                  disabled={working}
                  onChange={(event) =>
                    update(index, {
                      request_timeout_ms: Number(event.target.value),
                    })
                  }
                />
              </SettingsField>

              {transportOf(server) === "stdio" && (
                <>
                  <EnvironmentEditor
                    values={server.env}
                    disabled={working}
                    onChange={(env) => update(index, { env })}
                  />

                  <StringListEditor
                    label="Forward environment names"
                    hint="Only names are saved or displayed. Their values are resolved in the host process and never returned to the app."
                    values={server.env_from}
                    disabled={working}
                    addLabel="Add variable name"
                    onChange={(env_from) => update(index, { env_from })}
                  />
                </>
              )}

              <div className="flex flex-wrap gap-2">
                {server.enabled &&
                  server.health !== "initializing" &&
                  !dirty && (
                  <Button
                    type="button"
                    variant="outline"
                    disabled={working}
                    onClick={() => void reconnect(server.name)}
                  >
                    <RefreshCw size={14} />
                    {reconnecting === server.name
                      ? "Reconnecting…"
                      : "Reconnect and refresh tools"}
                  </Button>
                )}
                <Button
                  type="button"
                  variant="destructive"
                  disabled={working}
                  onClick={() =>
                    setServers((current) => {
                      setDirty(true);
                      return current.filter(
                        (_, itemIndex) => itemIndex !== index,
                      );
                    })
                  }
                >
                  <Trash2 size={14} />
                  Remove
                </Button>
              </div>
            </SettingsSection>
          ))}

          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              disabled={working}
              onClick={() =>
                setServers((current) => {
                  setDirty(true);
                  return [...current, emptyServer(current.length)];
                })
              }
            >
              <Plus size={14} />
              Add server
            </Button>
            <Button type="button" disabled={working} onClick={() => void save()}>
              {saving ? "Verifying…" : "Save and verify"}
            </Button>
          </div>

          {dirty && (
            <p className="text-xs text-muted-foreground">
              Save and verify changes before reconnecting a server.
            </p>
          )}

          <p className="text-sm leading-relaxed text-muted-foreground">
            MCP tools are always sensitive and keep OpenWave’s existing approval
            boundary. Child environments start empty. Put credentials in the
            host environment and select only their variable names above; do not
            enter secrets in the executable, arguments, working directory,
            literal values, or a server URL.
          </p>
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

function StringListEditor({
  label,
  hint,
  values,
  disabled,
  addLabel,
  onChange,
}: {
  label: string;
  hint?: string;
  values: string[];
  disabled: boolean;
  addLabel: string;
  onChange: (values: string[]) => void;
}) {
  return (
    <FieldGroup label={label} hint={hint}>
      <div className="flex flex-col gap-2">
        {values.map((value, index) => (
          <div className="flex gap-2" key={index}>
            <Input
              aria-label={`${label} ${index + 1}`}
              value={value}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) =>
                onChange(
                  values.map((item, itemIndex) =>
                    itemIndex === index ? event.target.value : item,
                  ),
                )
              }
            />
            <Button
              type="button"
              variant="outline"
              aria-label={`Remove ${label.toLowerCase()} ${index + 1}`}
              disabled={disabled}
              onClick={() =>
                onChange(values.filter((_, itemIndex) => itemIndex !== index))
              }
            >
              <Trash2 size={14} />
            </Button>
          </div>
        ))}
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={disabled}
          onClick={() => onChange([...values, ""])}
        >
          <Plus size={14} />
          {addLabel}
        </Button>
      </div>
    </FieldGroup>
  );
}

function EnvironmentEditor({
  values,
  disabled,
  onChange,
}: {
  values: Record<string, string>;
  disabled: boolean;
  onChange: (values: Record<string, string>) => void;
}) {
  const rows = Object.entries(values);
  function replaceRow(index: number, name: string, value: string) {
    const next = rows.map(([key, item], itemIndex) =>
      itemIndex === index ? ([name, value] as const) : ([key, item] as const),
    );
    onChange(Object.fromEntries(next));
  }
  return (
    <FieldGroup
      label="Literal non-secret environment"
      hint="These values are displayed and stored as ordinary settings. Never put credentials here."
    >
      <div className="flex flex-col gap-2">
        {rows.map(([name, value], index) => (
          <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-2" key={index}>
            <Input
              aria-label={`Environment name ${index + 1}`}
              placeholder="NAME"
              value={name}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) =>
                replaceRow(index, event.target.value, value)
              }
            />
            <Input
              aria-label={`Environment value ${index + 1}`}
              placeholder="non-secret value"
              value={value}
              disabled={disabled}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) =>
                replaceRow(index, name, event.target.value)
              }
            />
            <Button
              type="button"
              variant="outline"
              aria-label={`Remove environment value ${index + 1}`}
              disabled={disabled}
              onClick={() =>
                onChange(
                  Object.fromEntries(
                    rows.filter((_, itemIndex) => itemIndex !== index),
                  ),
                )
              }
            >
              <Trash2 size={14} />
            </Button>
          </div>
        ))}
        <Button
          type="button"
          variant="outline"
          className="self-start"
          disabled={disabled}
          onClick={() => onChange({ ...values, "": "" })}
        >
          <Plus size={14} />
          Add literal value
        </Button>
      </div>
    </FieldGroup>
  );
}

function FieldGroup({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <span className="text-sm font-medium">{label}</span>
      {children}
      {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
    </div>
  );
}

function healthLabel(health: McpServerInfo["health"]): string {
  switch (health) {
    case "healthy":
      return "Healthy";
    case "degraded":
      return "Needs attention";
    case "reconnecting":
      return "Reconnecting";
    case "disabled":
      return "Disabled";
    case "initializing":
      return "Not verified";
  }
}

function healthTone(
  health: McpServerInfo["health"],
): "ready" | "not-configured" | "disabled" {
  if (health === "healthy") return "ready";
  if (health === "disabled") return "disabled";
  return "not-configured";
}
