import { useEffect, useState } from "react";
import { toast } from "sonner";
import {
  ChevronDown,
  ChevronRight,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import type {
  ApiClient,
  ConnectedAppInfo,
  CredentialPlacement,
  RestCredentialUpdate,
  SpecPreviewInfo,
} from "../api";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { McpHealthChip, McpPanel, McpTierChip } from "./McpPanel";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  usedByLabel,
} from "./primitives";

type RestEntry = Extract<ConnectedAppInfo, { kind: "rest_api" }>;
type McpEntry = Extract<ConnectedAppInfo, { kind: "mcp_server" }>;

type CredentialMode = "none" | "bearer" | "header";

/** Where the OpenAPI document comes from: fetched by the server from a URL
 * (the primary path — real vendor documents are too large to hand-trim), or
 * pasted inline. */
type DocumentSource = "url" | "paste";

/** The create/edit draft for one REST connected app. */
type Draft = {
  /** Record id the save targets; minted fresh for a create. */
  id: string;
  /** The listed entry being edited, or null for a create. */
  existing: RestEntry | null;
  name: string;
  baseUrl: string;
  source: DocumentSource;
  documentUrl: string;
  document: string;
  /** What the last preview enumerated, pinned by its document hash. Cleared
   * whenever the source it came from changes, so a stale selection can never
   * outlive the document it was made against. */
  preview: SpecPreviewInfo | null;
  /** Selected operationIds; the saved catalog is exactly this set. */
  selected: string[];
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
    source: "url",
    documentUrl: "",
    document: "",
    preview: null,
    selected: [],
    mode:
      placement === null ? "none" : placement === "bearer" ? "bearer" : "header",
    headerName:
      placement !== null && placement !== "bearer" ? placement.header : "",
    value: "",
  };
}

/** The server refuses a catalog over 256 operations, so a larger preview
 * starts unselected and the user picks; at or under the bound, everything
 * starts selected — the same whole-document outcome as before. */
const MAX_SELECTABLE_OPERATIONS = 256;

function defaultSelection(preview: SpecPreviewInfo): string[] {
  return preview.operations.length <= MAX_SELECTABLE_OPERATIONS
    ? preview.operations.map((operation) => operation.operation_id)
    : [];
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

/**
 * One MCP-backed app entry: the app name and its health chip on one line, a
 * collapsed accordion enumerating the mounted tool names, and the count of
 * local apps bound to it. Names only — never remote-authored tool
 * descriptions — the same renderer-safety posture as the consent sheet.
 *
 * Tool availability lives here and nowhere else on the page; connection
 * state is the chip, detailed (for gateway endpoints on a managed profile)
 * on the endpoint's Advanced row. A record with no such row — any entry on
 * an unmanaged profile, or a pre-managed manual server — carries its own
 * diagnostic inline instead.
 */
function McpAppEntry({
  entry,
  managed,
  busy,
  reconnecting,
  onReconnect,
}: {
  entry: McpEntry;
  managed: boolean;
  busy: boolean;
  reconnecting: string | null;
  onReconnect: (name: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const detailInAdvanced = managed && entry.gateway_endpoint !== null;
  const unhealthy =
    entry.health !== "healthy" &&
    entry.health !== "initializing" &&
    entry.health !== "reconnecting";
  return (
    <li className="flex flex-col gap-1 rounded-md border px-3 py-2">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <p className="text-sm font-bold">{mcpTitle(entry)}</p>
        <McpHealthChip health={entry.health} />
        <McpTierChip curated={entry.curated} />
        {entry.gateway_endpoint !== null && (
          <span className="text-xs text-muted-foreground">
            · via your organization's gateway
          </span>
        )}
      </div>
      {!detailInAdvanced && unhealthy && entry.diagnostic !== null && (
        <p className="text-xs text-muted-foreground">{entry.diagnostic}</p>
      )}
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
      {entry.used_by_app_count > 0 && (
        <p className="text-xs text-muted-foreground">
          {usedByLabel(entry.used_by_app_count)}
        </p>
      )}
      {/* No endpoint indirection on an unmanaged profile, so the reconnect
          action rides the entry itself. */}
      {!managed &&
        entry.health !== "initializing" &&
        entry.health !== "disabled" && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            disabled={busy}
            onClick={() => onReconnect(entry.name)}
          >
            <RefreshCw size={14} />
            {reconnecting === entry.name ? "Reconnecting…" : "Reconnect"}
          </Button>
        )}
    </li>
  );
}

/**
 * The operation picker over one previewed document: a filter, select
 * all/none over the filtered view, and one checkbox per listable operation.
 * The muted counts keep the picker honest about what the preview could not
 * list and where it was cut.
 */
function OperationPicker({
  preview,
  selected,
  disabled,
  onChange,
}: {
  preview: SpecPreviewInfo;
  selected: string[];
  disabled: boolean;
  onChange: (selected: string[]) => void;
}) {
  const [filter, setFilter] = useState("");
  const needle = filter.trim().toLowerCase();
  const shown =
    needle === ""
      ? preview.operations
      : preview.operations.filter(
          (operation) =>
            operation.operation_id.toLowerCase().includes(needle) ||
            operation.path.toLowerCase().includes(needle) ||
            (operation.summary ?? "").toLowerCase().includes(needle),
        );
  const chosen = new Set(selected);
  const notes = [
    `${selected.length} of ${preview.operations.length} selected`,
    ...(preview.unlistable > 0
      ? [`${preview.unlistable} unselectable (no usable operationId)`]
      : []),
    ...(preview.truncated ? ["list truncated"] : []),
  ];
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          className="max-w-60"
          value={filter}
          disabled={disabled}
          autoComplete="off"
          spellCheck={false}
          placeholder="Filter operations"
          aria-label="Filter operations"
          onChange={(event) => setFilter(event.target.value)}
        />
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled}
          onClick={() =>
            onChange([
              ...new Set([
                ...selected,
                ...shown.map((operation) => operation.operation_id),
              ]),
            ])
          }
        >
          Select shown
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled || selected.length === 0}
          onClick={() => onChange([])}
        >
          Clear
        </Button>
      </div>
      <ul
        aria-label="Operations"
        className="flex max-h-64 flex-col gap-1 overflow-y-auto rounded-md border p-2"
      >
        {shown.map((operation) => (
          <li key={operation.operation_id}>
            <label className="flex items-start gap-2 text-xs">
              <input
                type="checkbox"
                className="mt-0.5"
                checked={chosen.has(operation.operation_id)}
                disabled={disabled}
                onChange={(event) =>
                  onChange(
                    event.target.checked
                      ? [...selected, operation.operation_id]
                      : selected.filter(
                          (id) => id !== operation.operation_id,
                        ),
                  )
                }
              />
              <span className="min-w-0">
                <code className="font-medium">
                  {operation.method.toUpperCase()} {operation.path}
                </code>
                {operation.summary !== null && (
                  <span className="block truncate text-muted-foreground">
                    {operation.summary}
                  </span>
                )}
              </span>
            </label>
          </li>
        ))}
        {shown.length === 0 && (
          <li className="text-xs text-muted-foreground">
            No operations match the filter.
          </li>
        )}
      </ul>
      <p className="text-xs text-muted-foreground">{notes.join(" · ")}</p>
    </div>
  );
}

/**
 * The transport machinery, managed profiles only: `McpPanel`'s compact
 * endpoint rows behind a disclosure, collapsed by default. Unmanaged
 * profiles have no gateway indirection, so they get no Advanced section —
 * the editor below the apps list is the whole transport surface.
 */
function AdvancedSection({ client }: { client: ApiClient }) {
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
      {open && <McpPanel client={client} managed />}
    </section>
  );
}

/**
 * The Connected apps page, app-first and once-only: every fact renders
 * exactly once, at the altitude of the thing it describes. The Apps list
 * owns identity, health chip, tool names, and the bound-app count; on a
 * managed profile the endpoint transport collapses to compact rows behind
 * an Advanced disclosure, while an unmanaged profile has no Advanced at
 * all — the manual MCP editor below the list is its whole transport
 * surface. The approval boundary is stated once, as the page footer.
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
  const [previewing, setPreviewing] = useState(false);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [reconnecting, setReconnecting] = useState<string | null>(null);

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

  /** Enumerate the draft's document (URL or pasted) into the picker. */
  async function loadOperations() {
    if (draft === null) return;
    const source =
      draft.source === "url"
        ? { url: draft.documentUrl.trim() }
        : { document: draft.document };
    if (draft.source === "url" && draft.documentUrl.trim() === "") {
      setFormError("Enter the OpenAPI document URL to fetch.");
      return;
    }
    if (draft.source === "paste" && draft.document.trim() === "") {
      setFormError("Paste the OpenAPI document first.");
      return;
    }
    setPreviewing(true);
    setFormError(null);
    try {
      const preview = await client.previewRestSpec(source);
      update({ preview, selected: defaultSelection(preview) });
      toast.success(
        `Found ${preview.operations.length} operation${preview.operations.length === 1 ? "" : "s"}`,
      );
    } catch (err) {
      setFormError(errorMessage(err));
      toast.error(errorMessage(err));
    } finally {
      setPreviewing(false);
    }
  }

  async function save() {
    if (draft === null) return;
    const name = draft.name.trim();
    const baseUrl = draft.baseUrl.trim();
    const document = draft.document.trim();
    if (name === "" || baseUrl === "") {
      setFormError("Name and base URL are required.");
      return;
    }
    let documentFields: {
      openapi_document?: string;
      openapi_document_url?: string;
      document_sha256?: string;
      operation_ids?: string[];
    };
    if (draft.source === "url") {
      // A URL save always rides the preview's hash pin: what the picker
      // showed is exactly what the server will ingest, or it refuses.
      if (draft.documentUrl.trim() === "") {
        setFormError("Enter the OpenAPI document URL.");
        return;
      }
      if (draft.preview === null) {
        setFormError("Fetch the document's operations before saving.");
        return;
      }
      if (draft.selected.length === 0) {
        setFormError("Select at least one operation.");
        return;
      }
      documentFields = {
        openapi_document_url: draft.documentUrl.trim(),
        document_sha256: draft.preview.document_sha256,
        operation_ids: draft.selected,
      };
    } else {
      if (document === "") {
        setFormError("Paste the OpenAPI document.");
        return;
      }
      if (draft.preview !== null && draft.selected.length === 0) {
        setFormError("Select at least one operation.");
        return;
      }
      documentFields = {
        openapi_document: document,
        // A pasted document only carries a selection when one was made; the
        // pin then guards against the textarea changing after the preview.
        ...(draft.preview !== null
          ? {
              document_sha256: draft.preview.document_sha256,
              operation_ids: draft.selected,
            }
          : {}),
      };
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
        ...documentFields,
        credential,
      });
      setApps(result.apps);
      setDraft(null);
      toast.success(`Saved ${name}`);
    } catch (err) {
      setFormError(errorMessage(err));
      toast.error(errorMessage(err));
    } finally {
      setSaving(false);
    }
  }

  /** Reconnect one local server from its app entry, then re-read the
   * listing so the entry's chip reflects the outcome. */
  async function reconnectEntry(name: string) {
    setReconnecting(name);
    setListError(null);
    try {
      await client.reconnectMcpServer(name);
    } catch (err) {
      setListError(errorMessage(err));
    }
    try {
      const info = await client.listConnectedApps();
      setApps(info.apps);
    } catch {
      // Keep the last-known entries; the reconnect error (if any) stands.
    } finally {
      setReconnecting(null);
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
    <li
      key={entry.id}
      className="flex items-center justify-between gap-4 rounded-md border px-3 py-2"
    >
      <div className="min-w-0 flex-1">
        <p className="text-sm font-bold">{entry.name}</p>
        <p className="truncate text-xs text-muted-foreground">
          {entry.base_url} · {entry.operation_count} operation
          {entry.operation_count === 1 ? "" : "s"} · {credentialLabel(entry)}
        </p>
        {entry.used_by_app_count > 0 && (
          <p className="text-xs text-muted-foreground">
            {usedByLabel(entry.used_by_app_count)}
          </p>
        )}
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
    </li>
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
      <div className="flex flex-col gap-1.5">
        <p className="font-bold">OpenAPI document</p>
        <p className="text-sm text-muted-foreground">
          {draft.existing === null
            ? "Only the selected operations are kept, never the document itself."
            : "The raw document is not stored, so provide it again to save changes."}
        </p>
        <div
          className="flex gap-4"
          role="radiogroup"
          aria-label="Document source"
        >
          {(
            [
              ["url", "Fetch from URL"],
              ["paste", "Paste document"],
            ] as const
          ).map(([source, label]) => (
            <label key={source} className="flex items-center gap-2 text-sm">
              <input
                type="radio"
                name="document-source"
                checked={draft.source === source}
                disabled={saving || previewing}
                onChange={() =>
                  // Switching sources invalidates any preview: the selection
                  // must never outlive the document it was made against.
                  update({ source, preview: null, selected: [] })
                }
              />
              {label}
            </label>
          ))}
        </div>
      </div>
      {draft.source === "url" ? (
        <SettingsField
          label="Document URL"
          hint="https only; JSON OpenAPI 3.x. The server fetches it — the document never rides the form."
        >
          <div className="flex gap-2">
            <Input
              value={draft.documentUrl}
              disabled={saving || previewing}
              autoComplete="off"
              spellCheck={false}
              placeholder="https://api.example.com/openapi.json"
              onChange={(event) =>
                update({
                  documentUrl: event.target.value,
                  preview: null,
                  selected: [],
                })
              }
            />
            <Button
              type="button"
              variant="outline"
              disabled={saving || previewing}
              onClick={() => void loadOperations()}
            >
              {previewing && <Loader2 size={14} className="animate-spin" />}
              {previewing ? "Fetching…" : "Fetch operations"}
            </Button>
          </div>
        </SettingsField>
      ) : (
        <div className="flex flex-col gap-2">
          <textarea
            className="min-h-40 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
            value={draft.document}
            disabled={saving || previewing}
            spellCheck={false}
            aria-label="OpenAPI document"
            onChange={(event) =>
              update({
                document: event.target.value,
                preview: null,
                selected: [],
              })
            }
          />
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            disabled={saving || previewing}
            onClick={() => void loadOperations()}
          >
            {previewing && <Loader2 size={14} className="animate-spin" />}
            {previewing ? "Loading…" : "Select operations…"}
          </Button>
        </div>
      )}
      {draft.preview !== null && (
        <OperationPicker
          preview={draft.preview}
          selected={draft.selected}
          disabled={saving || previewing}
          onChange={(selected) => update({ selected })}
        />
      )}
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
          {saving && <Loader2 size={14} className="animate-spin" />}
          {saving ? "Saving…" : "Save"}
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

  const busy = saving || previewing || deleting !== null || reconnecting !== null;

  return (
    <SettingsPanel
      title="Connected apps"
      description="The apps this profile can reach — through your organization's gateway, local MCP servers, and REST APIs — bound by local apps with your consent."
      busy={loading || busy}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading connected apps…</p>
      ) : (
        <>
          <section className="flex flex-col gap-4">
            <div className="flex flex-col gap-1">
              <h2 className="text-lg font-semibold">Apps</h2>
              <p className="text-sm text-muted-foreground">
                Each entry is one connected app. Expand it to see the tools it
                makes available.
              </p>
              {managed && (
                <p className="text-sm text-muted-foreground">
                  REST connected apps are managed by your organization's
                  gateway; there is nothing to configure here.
                </p>
              )}
            </div>
            <Card className="gap-4 border bg-transparent p-4">
              {apps.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  {managed
                    ? "No apps connected."
                    : "No apps connected. Add a REST API here, or configure MCP servers in the editor below."}
                </p>
              ) : (
                <ul
                  aria-label="Connected apps"
                  className="flex flex-col gap-2"
                >
                  {apps.map((entry) =>
                    entry.kind === "mcp_server" ? (
                      <McpAppEntry
                        key={entry.id}
                        entry={entry}
                        managed={managed}
                        busy={busy}
                        reconnecting={reconnecting}
                        onReconnect={(name) => void reconnectEntry(name)}
                      />
                    ) : (
                      restRow(entry)
                    ),
                  )}
                </ul>
              )}
              {!managed && draft === null && (
                <div>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => {
                      setFormError(null);
                      setDraft(draftFor(null));
                    }}
                  >
                    <Plus size={14} /> Add REST API
                  </Button>
                </div>
              )}
            </Card>
          </section>

          {!managed && editor}
        </>
      )}

      {managed ? (
        <AdvancedSection client={client} />
      ) : (
        <McpPanel client={client} managed={false} />
      )}

      {listError && <SettingsError>{listError}</SettingsError>}

      <p className="text-sm text-muted-foreground">
        MCP tools are always sensitive and keep OpenWave's existing approval
        boundary.
      </p>
    </SettingsPanel>
  );
}
