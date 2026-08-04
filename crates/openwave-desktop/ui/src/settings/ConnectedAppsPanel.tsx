import { useEffect, useState } from "react";
import { toast } from "sonner";
import { ChevronDown, ChevronRight, Pencil, Plus, Trash2 } from "lucide-react";
import type {
  ApiClient,
  ConnectedAppInfo,
  CredentialPlacement,
  RestCredentialUpdate,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { McpPanel } from "./McpPanel";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

type RestEntry = Extract<ConnectedAppInfo, { kind: "rest_api" }>;
type McpEntry = Extract<ConnectedAppInfo, { kind: "mcp_server" }>;

type CredentialMode = "none" | "bearer" | "header";

/** The create/edit draft for one REST connected app. */
type Draft = {
  /** Record id the save targets; minted fresh for a create. */
  id: string;
  /** The listed entry being edited, or null for a create. */
  existing: RestEntry | null;
  name: string;
  baseUrl: string;
  document: string;
  mode: CredentialMode;
  headerName: string;
  /** The credential value. Never prefilled: an untouched (empty) value on an
   * edit means "keep the stored one", and nothing this panel renders ever
   * reads a value back from the server. */
  value: string;
};

function draftFor(existing: RestEntry | null): Draft {
  const placement = existing?.placement ?? null;
  return {
    id: existing?.id ?? crypto.randomUUID(),
    existing,
    name: existing?.name ?? "",
    baseUrl: existing?.base_url ?? "",
    document: "",
    mode:
      placement === null ? "none" : placement === "bearer" ? "bearer" : "header",
    headerName:
      placement !== null && placement !== "bearer" ? placement.header : "",
    value: "",
  };
}

function samePlacement(
  stored: CredentialPlacement | null,
  chosen: CredentialPlacement,
): boolean {
  if (stored === null) return false;
  if (stored === "bearer" || chosen === "bearer") return stored === chosen;
  return stored.header === chosen.header;
}

function credentialLabel(entry: RestEntry): string {
  switch (entry.credential_status) {
    case "none":
      return "No credential";
    case "configured":
      return entry.placement === "bearer" || entry.placement === null
        ? "Credential configured"
        : `Credential configured (${entry.placement.header})`;
    case "missing":
      return "Credential missing";
  }
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** The name an app entry leads with. A gateway-backed record shows the
 * organization's entitled app names when the gateway reported them; a local
 * record has no org-app concept — its record display name is the app. */
function mcpTitle(entry: McpEntry): string {
  return entry.gateway_endpoint !== null && entry.gateway_apps.length > 0
    ? entry.gateway_apps.join(", ")
    : entry.name;
}

/** One renderer-safe status sentence per entry; diagnostics already are one. */
function mcpStatus(entry: McpEntry): string {
  switch (entry.health) {
    case "healthy":
      return `${entry.tool_count} tool${entry.tool_count === 1 ? "" : "s"} available to new turns.`;
    case "initializing":
    case "reconnecting":
      return "Connecting…";
    case "disabled":
      return entry.diagnostic ?? "Disabled.";
    default:
      return entry.diagnostic ?? "Needs attention. See Advanced below.";
  }
}

/**
 * One MCP-backed app entry: the app name first, a status line, and a
 * collapsed accordion enumerating the mounted tool names. Names only — never
 * remote-authored tool descriptions — the same renderer-safety posture as
 * the consent sheet.
 */
function McpAppCard({ entry }: { entry: McpEntry }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="flex flex-col gap-1 rounded-md border px-3 py-2">
      <p className="text-sm font-bold">{mcpTitle(entry)}</p>
      <p className="text-xs text-muted-foreground">
        {entry.gateway_endpoint !== null &&
          "Via your organization's gateway · "}
        {mcpStatus(entry)}
      </p>
      {entry.tools.length > 0 && (
        <>
          <button
            type="button"
            className="flex items-center gap-1 self-start text-xs text-muted-foreground hover:text-foreground"
            aria-expanded={open}
            onClick={() => setOpen((current) => !current)}
          >
            {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            {entry.tools.length} tool{entry.tools.length === 1 ? "" : "s"}
          </button>
          {open && (
            <ul className="flex flex-col gap-0.5 pl-4">
              {entry.tools.map((tool) => (
                <li key={tool} className="font-mono text-xs">
                  {tool}
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

/**
 * The demoted transport machinery: gateway endpoint mounts, per-server
 * health, and (on unmanaged profiles) the manual MCP server editor — the
 * whole `McpPanel`, unchanged, behind a disclosure. Everything it could do
 * it still does; it just no longer leads the page.
 */
function AdvancedSection({
  client,
  managed,
}: {
  client: ApiClient;
  managed: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <section className="flex flex-col gap-6">
      <button
        type="button"
        className="flex items-center gap-1 self-start text-sm font-medium text-muted-foreground hover:text-foreground"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        Advanced: transport &amp; endpoints
      </button>
      {open && <McpPanel client={client} managed={managed} />}
    </section>
  );
}

/**
 * The Connected apps page, app-first: the primary list is the apps this
 * profile can reach — org-app names for gateway-backed MCP records, record
 * names for local ones, REST entries beside them — each with a names-only
 * tool accordion. The transport machinery (endpoint mounts, the manual MCP
 * server editor absorbed from the retired MCP page) is demoted to an
 * Advanced disclosure below; nothing is removed, only de-emphasized.
 */
export function ConnectedAppsPanel({
  client,
  managed = false,
}: {
  client: ApiClient;
  /** On a managed profile the server refuses `rest_api` writes wholesale —
   * the gateway is the sole governed REST channel — so the REST half renders
   * a read-only notice and no editing affordances. `McpPanel` reads the same
   * flag and renders its own managed view. */
  managed?: boolean;
}) {
  const [apps, setApps] = useState<ConnectedAppInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    client
      .listConnectedApps()
      .then((info) => {
        if (cancelled) return;
        setApps(info.apps);
        setListError(null);
        setLoading(false);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setListError(errorMessage(err));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  function update(change: Partial<Draft>) {
    setDraft((current) => (current === null ? null : { ...current, ...change }));
  }

  async function save() {
    if (draft === null) return;
    const name = draft.name.trim();
    const baseUrl = draft.baseUrl.trim();
    const document = draft.document.trim();
    if (name === "" || baseUrl === "" || document === "") {
      setFormError(
        "Name, base URL, and the OpenAPI document are all required.",
      );
      return;
    }
    let credential: RestCredentialUpdate;
    if (draft.mode === "none") {
      credential = "none";
    } else {
      const headerName = draft.headerName.trim();
      if (draft.mode === "header" && headerName === "") {
        setFormError("Enter the header name the credential is sent in.");
        return;
      }
      const placement: CredentialPlacement =
        draft.mode === "bearer" ? "bearer" : { header: headerName };
      if (draft.value !== "") {
        credential = { set: { value: draft.value, placement } };
      } else if (
        draft.existing !== null &&
        draft.existing.credential_status !== "none" &&
        samePlacement(draft.existing.placement, placement)
      ) {
        // Untouched value on an edit: keep the stored credential unchanged.
        credential = "keep";
      } else {
        setFormError("Enter the credential value.");
        return;
      }
    }
    setSaving(true);
    setFormError(null);
    try {
      const result = await client.putRestConnectedApp(draft.id, {
        name,
        base_url: baseUrl,
        openapi_document: document,
        credential,
      });
      setApps(result.apps);
      setDraft(null);
      toast.success(`Saved ${name}`);
    } catch (err) {
      setFormError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  async function remove(entry: RestEntry) {
    setDeleting(entry.id);
    try {
      await client.deleteRestConnectedApp(entry.id);
      setApps((current) => current.filter((app) => app.id !== entry.id));
      toast.success(`Removed ${entry.name}`);
    } catch (err) {
      setListError(errorMessage(err));
    } finally {
      setDeleting(null);
    }
  }

  const restRow = (entry: RestEntry) => (
    <div
      key={entry.id}
      className="flex items-center justify-between gap-4 rounded-md border px-3 py-2"
    >
      <div className="min-w-0 flex-1">
        <p className="text-sm font-bold">{entry.name}</p>
        <p className="truncate text-xs text-muted-foreground">
          {entry.base_url} · {entry.operation_count} operation
          {entry.operation_count === 1 ? "" : "s"} · {credentialLabel(entry)}
        </p>
      </div>
      {!managed && (
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={saving || deleting !== null}
            onClick={() => {
              setFormError(null);
              setDraft(draftFor(entry));
            }}
          >
            <Pencil size={14} /> Edit
          </Button>
          <Button
            variant="outline"
            size="sm"
            aria-label={`Remove ${entry.name}`}
            disabled={saving || deleting !== null}
            onClick={() => void remove(entry)}
          >
            <Trash2 size={14} />
          </Button>
        </div>
      )}
    </div>
  );

  const editor = draft !== null && (
    <SettingsSection
      title={draft.existing === null ? "Add REST API" : `Edit ${draft.existing.name}`}
    >
      <SettingsField label="Name">
        <Input
          value={draft.name}
          disabled={saving}
          autoComplete="off"
          spellCheck={false}
          placeholder="Sentry"
          onChange={(event) => update({ name: event.target.value })}
        />
      </SettingsField>
      <SettingsField
        label="Base URL"
        hint="https only; operation paths from the document append to it."
      >
        <Input
          value={draft.baseUrl}
          disabled={saving}
          autoComplete="off"
          spellCheck={false}
          placeholder="https://api.example.com/v2"
          onChange={(event) => update({ baseUrl: event.target.value })}
        />
      </SettingsField>
      <SettingsField
        label="OpenAPI document"
        hint={
          draft.existing === null
            ? "Paste the JSON OpenAPI 3.x document. Only the declared operations are kept, never the document itself."
            : "The raw document is not stored, so paste it again to save changes."
        }
      >
        <textarea
          className="min-h-40 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
          value={draft.document}
          disabled={saving}
          spellCheck={false}
          onChange={(event) => update({ document: event.target.value })}
        />
      </SettingsField>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="font-bold">Credential</legend>
        <div className="flex gap-4" role="radiogroup" aria-label="Credential">
          {(
            [
              ["none", "No credential"],
              ["bearer", "Bearer token"],
              ["header", "Custom header"],
            ] as const
          ).map(([mode, label]) => (
            <label key={mode} className="flex items-center gap-2 text-sm">
              <input
                type="radio"
                name="credential-mode"
                checked={draft.mode === mode}
                disabled={saving}
                onChange={() => update({ mode })}
              />
              {label}
            </label>
          ))}
        </div>
      </fieldset>
      {draft.mode === "header" && (
        <SettingsField label="Header name">
          <Input
            value={draft.headerName}
            disabled={saving}
            autoComplete="off"
            spellCheck={false}
            placeholder="X-Api-Key"
            onChange={(event) => update({ headerName: event.target.value })}
          />
        </SettingsField>
      )}
      {draft.mode !== "none" && (
        <SettingsField
          label="Credential value"
          hint={
            draft.existing !== null &&
            draft.existing.credential_status !== "none"
              ? "Leave blank to keep the stored value."
              : "Stored in the profile secret store; never shown again."
          }
        >
          <Input
            type="password"
            value={draft.value}
            disabled={saving}
            autoComplete="off"
            onChange={(event) => update({ value: event.target.value })}
          />
        </SettingsField>
      )}
      {formError && <SettingsError>{formError}</SettingsError>}
      <div className="flex gap-2">
        <Button disabled={saving} onClick={() => void save()}>
          Save
        </Button>
        <Button
          variant="outline"
          disabled={saving}
          onClick={() => {
            setFormError(null);
            setDraft(null);
          }}
        >
          Cancel
        </Button>
      </div>
    </SettingsSection>
  );

  return (
    <SettingsPanel
      title="Connected apps"
      description="The apps this profile can reach — through your organization's gateway, local MCP servers, and REST APIs — bound by local apps with your consent."
      busy={loading || saving || deleting !== null}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading connected apps…</p>
      ) : (
        <>
          <SettingsSection
            title="Apps"
            description="Each entry is one connected app. Expand it to see the tools it makes available."
          >
            {managed && (
              <SettingsStatus
                tone="disabled"
                label="Managed by your organization"
                description="REST connected apps are managed by your organization's gateway; there is nothing to configure here."
              />
            )}
            {apps.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No apps connected. Add a REST API here, or configure MCP
                servers under Advanced below.
              </p>
            ) : (
              apps.map((entry) =>
                entry.kind === "mcp_server" ? (
                  <McpAppCard key={entry.id} entry={entry} />
                ) : (
                  restRow(entry)
                ),
              )
            )}
            {!managed && draft === null && (
              <div>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={deleting !== null}
                  onClick={() => {
                    setFormError(null);
                    setDraft(draftFor(null));
                  }}
                >
                  <Plus size={14} /> Add REST API
                </Button>
              </div>
            )}
          </SettingsSection>

          {!managed && editor}
        </>
      )}

      <AdvancedSection client={client} managed={managed} />

      {listError && <SettingsError>{listError}</SettingsError>}
    </SettingsPanel>
  );
}
