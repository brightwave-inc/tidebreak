import { Fragment, useId, useRef, useState } from "react";
import { toast } from "sonner";
import { Download, Upload } from "lucide-react";

import type { ApiClient } from "@/api/client";
import type {
  ExportedCodeRepository,
  ExportedMcpServer,
  WorkspaceConfigAction,
  WorkspaceConfigApplyRequest,
  WorkspaceConfigApplyResult,
  WorkspaceConfigDecision,
  WorkspaceConfigDocument,
  WorkspaceConfigPreview,
  WorkspaceConfigPreviewEntry,
  WorkspaceConfigPreviewStatus,
} from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  hasNativeHost,
  pickWorkspaceConfig,
  saveWorkspaceConfig,
} from "@/host";
import { SettingsError, SettingsSection, SettingsStatus } from "./primitives";

type ConfigClient = Pick<
  ApiClient,
  "exportWorkspaceConfig" | "previewWorkspaceConfig" | "applyWorkspaceConfig"
>;

export function PortableConfigSection({ client }: { client: ConfigClient }) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<{
    document: WorkspaceConfigDocument;
    entries: WorkspaceConfigPreviewEntry[];
  } | null>(null);
  const [actions, setActions] = useState<Record<string, WorkspaceConfigAction>>(
    {},
  );
  // Rows whose choice the person made themselves. An entry that needs a path
  // follows its path otherwise: Skip while the path is empty, Add once it is
  // filled in.
  const [chosen, setChosen] = useState<Record<string, boolean>>({});
  const [result, setResult] = useState<WorkspaceConfigApplyResult | null>(null);
  const [remaps, setRemaps] = useState<Record<string, Record<string, string>>>(
    {},
  );
  // Servers the person chose to start after import. Every one starts turned
  // off: a local command runs a program on this computer, which Tidebreak
  // also asks about in a native dialog, and a remote server that sends a
  // credential hands a value from this computer to the file's URL.
  const [starts, setStarts] = useState<Record<string, boolean>>({});
  const [applying, setApplying] = useState(false);
  const [applyError, setApplyError] = useState<string | null>(null);

  async function exportConfig() {
    setBusy(true);
    setError(null);
    try {
      const exported = await client.exportWorkspaceConfig();
      const contents = JSON.stringify(exported, null, 2);
      if (hasNativeHost()) {
        const saved = await saveWorkspaceConfig(contents);
        if (saved) toast.success("Workspace configuration saved");
      } else {
        const blob = new Blob([contents], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const link = window.document.createElement("a");
        link.href = url;
        link.download = "tidebreak-config.json";
        link.click();
        URL.revokeObjectURL(url);
        toast.success("Workspace configuration downloaded");
      }
    } catch (caught) {
      const message = friendlyErrorMessage(
        caught,
        "Could not export workspace configuration.",
      );
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  }

  async function importText(text: string) {
    setBusy(true);
    setError(null);
    try {
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch {
        throw new Error(
          "The file is not valid JSON. Export a workspace configuration from Tidebreak and import that file.",
        );
      }
      const result: WorkspaceConfigPreview =
        await client.previewWorkspaceConfig(parsed);
      const document = parsed as WorkspaceConfigDocument;
      // Only an entry that is new here and needs nothing remapped starts on
      // Add. A conflict would overwrite something set up on this machine, and
      // an entry that needs a path or command has none yet, so both start on
      // Skip until the person decides.
      const nextActions: Record<string, WorkspaceConfigAction> = {};
      for (const entry of result.entries) {
        nextActions[entryKey(entry)] = entry.status === "new" ? "add" : "skip";
      }
      setActions(nextActions);
      setChosen({});
      setRemaps({});
      setStarts({});
      setApplyError(null);
      setResult(null);
      setPreview({ document, entries: result.entries });
    } catch (caught) {
      const message = friendlyErrorMessage(
        caught,
        "Could not preview the workspace configuration.",
      );
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  }

  async function importFromPicker() {
    if (hasNativeHost()) {
      try {
        const contents = await pickWorkspaceConfig();
        if (contents) await importText(contents);
      } catch (caught) {
        const message = friendlyErrorMessage(
          caught,
          "Could not open the workspace configuration.",
        );
        setError(message);
        toast.error(message);
      }
      return;
    }
    fileRef.current?.click();
  }

  /** The action an entry imports with, as the dialog shows it. */
  function actionFor(
    entry: WorkspaceConfigPreviewEntry,
  ): WorkspaceConfigAction {
    const key = entryKey(entry);
    const action = actions[key] ?? "skip";
    if (entry.status !== "needs_remap") return action;
    if (!remapComplete(entry, remaps[key])) return "skip";
    return chosen[key] ? action : "add";
  }

  async function applyImport() {
    if (!preview) return;
    setApplying(true);
    setApplyError(null);
    try {
      const body: WorkspaceConfigApplyRequest = {
        document: preview.document,
        decisions: preview.entries.map((entry) => {
          const key = entryKey(entry);
          const decision: WorkspaceConfigDecision = {
            section: entry.section,
            key: entry.key,
            action: actionFor(entry),
            remaps: filledRemaps(remaps[key]),
          };
          // A server that runs a command or sends a credential starts only
          // when the person switched it on for this row. Saying so every time
          // keeps the file's own flag out of the decision.
          const server = findMcpServer(preview.document, entry);
          if (server?.enabled && waitsForStart(server)) {
            decision.enabled = starts[key] === true;
          }
          return decision;
        }),
      };
      const applied = await client.applyWorkspaceConfig(body);
      toast.success(appliedSummary(applied));
      setResult(applied);
      setPreview(null);
    } catch (caught) {
      const message = friendlyErrorMessage(
        caught,
        "Could not apply the workspace configuration.",
      );
      setApplyError(message);
      toast.error(message);
    } finally {
      setApplying(false);
    }
  }

  return (
    <>
      <SettingsSection
        title="Portable configuration"
        description="Export code repository registrations and MCP server definitions to a file you can import on another machine. Secrets are never written."
      >
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="outline"
            disabled={busy}
            onClick={() => void exportConfig()}
          >
            <Download size={14} />
            Export
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={busy}
            onClick={() => void importFromPicker()}
          >
            <Upload size={14} />
            Import
          </Button>
          <input
            ref={fileRef}
            type="file"
            accept=".json,.tidebreak-config.json,application/json"
            className="sr-only"
            aria-label="Import workspace configuration"
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              event.currentTarget.value = "";
              if (!file) return;
              void file.text().then((text) => importText(text));
            }}
          />
        </div>
        {result && (
          <SettingsStatus
            tone="ready"
            label="Import finished"
            description={appliedSummary(result)}
          />
        )}
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsSection>
      <Dialog
        open={preview !== null}
        onOpenChange={(open) => {
          if (!open) setPreview(null);
        }}
      >
        <DialogContent className="max-w-xl">
          <DialogHeader>
            <DialogTitle>Import workspace configuration</DialogTitle>
            <DialogDescription>
              Review what each entry runs and connects to. Tidebreak overwrites
              an existing record only when you choose Replace. An MCP server
              that runs a command or sends a credential from this computer
              imports turned off unless you start it.
            </DialogDescription>
          </DialogHeader>
          {preview && (
            <ul
              className="flex max-h-[min(60vh,32rem)] flex-col gap-3 overflow-y-auto"
              aria-label="Import preview"
            >
              {preview.entries.map((entry) => (
                <PreviewRow
                  key={entryKey(entry)}
                  entry={entry}
                  document={preview.document}
                  action={actionFor(entry)}
                  remap={remaps[entryKey(entry)] ?? {}}
                  start={starts[entryKey(entry)] === true}
                  onAction={(next) => {
                    setActions((current) => ({
                      ...current,
                      [entryKey(entry)]: next,
                    }));
                    setChosen((current) => ({
                      ...current,
                      [entryKey(entry)]: true,
                    }));
                  }}
                  onRemap={(field, value) =>
                    setRemaps((current) => ({
                      ...current,
                      [entryKey(entry)]: {
                        ...(current[entryKey(entry)] ?? {}),
                        [field]: value,
                      },
                    }))
                  }
                  onStart={(next) =>
                    setStarts((current) => ({
                      ...current,
                      [entryKey(entry)]: next,
                    }))
                  }
                />
              ))}
            </ul>
          )}
          {applyError && (
            <SettingsStatus
              tone="critical"
              label="The import did not run"
              description={applyError}
            />
          )}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setPreview(null)}
            >
              Cancel
            </Button>
            <Button
              type="button"
              disabled={applying}
              onClick={() => void applyImport()}
            >
              {applying ? "Applying…" : "Apply"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function entryKey(entry: WorkspaceConfigPreviewEntry): string {
  return `${entry.section}:${entry.key}`;
}

/** Whether every field an entry needs on this machine has a value. */
function remapComplete(
  entry: WorkspaceConfigPreviewEntry,
  remap: Record<string, string> | undefined,
): boolean {
  return (entry.remap_fields ?? []).every(
    (field) => (remap?.[field] ?? "").trim() !== "",
  );
}

function appliedSummary(result: WorkspaceConfigApplyResult): string {
  const imported = `Imported ${result.applied} ${result.applied === 1 ? "entry" : "entries"}`;
  return result.skipped > 0
    ? `${imported} and skipped ${result.skipped}.`
    : `${imported}.`;
}

/** Remap fields the person filled in. A blank one means "not remapped". */
function filledRemaps(
  remap: Record<string, string> | undefined,
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(remap ?? {}).filter(([, value]) => value.trim() !== ""),
  );
}

function findMcpServer(
  document: WorkspaceConfigDocument,
  entry: WorkspaceConfigPreviewEntry,
): ExportedMcpServer | undefined {
  if (entry.section !== "mcp_servers") return undefined;
  return (document.sections.mcp_servers ?? []).find(
    (server) => server.name === entry.key,
  );
}

/** The key the server previews a repository under: origin, else the remote
 * it was cloned from, else its display name. */
function repositoryKey(repo: ExportedCodeRepository): string {
  return repo.origin_url ?? repo.cloned_from ?? repo.display_name;
}

function findRepository(
  document: WorkspaceConfigDocument,
  entry: WorkspaceConfigPreviewEntry,
): ExportedCodeRepository | undefined {
  if (entry.section !== "code_repositories") return undefined;
  return (document.sections.code_repositories ?? []).find(
    (repo) => repositoryKey(repo) === entry.key,
  );
}

/**
 * The environment variables whose values a remote server sends to its URL:
 * its bearer token variable. The server applies the same rule, and covers
 * process environment names too in case a remote server ever carries one.
 */
function sentVariables(server: ExportedMcpServer): string[] {
  if (server.url === undefined) return [];
  return [
    ...(server.bearer_token_env ? [server.bearer_token_env] : []),
    ...server.env_from,
    ...server.env,
  ];
}

/** A server whose start needs the person's switch for this row: it runs a
 * command on this computer, or sends a value from this computer's
 * environment to a URL the file chose. */
function waitsForStart(server: ExportedMcpServer): boolean {
  return server.command !== undefined || sentVariables(server).length > 0;
}

/** The host a URL names, for a sentence. */
function urlHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

type Detail = { label: string; value: string };

/** One argument as a shell would need it to read back the same. */
function shellWord(word: string): string {
  return word === "" || /[\s"'\\]/.test(word) ? JSON.stringify(word) : word;
}

function mcpDetails(server: ExportedMcpServer): Detail[] {
  const details: Detail[] = [];
  if (server.command !== undefined) {
    details.push({
      label: "Command",
      value: [server.command, ...server.args].map(shellWord).join(" "),
    });
  }
  if (server.cwd) {
    details.push({ label: "Working directory", value: server.cwd });
  }
  if (server.url) details.push({ label: "URL", value: server.url });
  if (server.gateway_endpoint) {
    details.push({ label: "Gateway endpoint", value: server.gateway_endpoint });
  }
  // A remote server's variables show as what they are: values sent to its
  // host, in the sentence under these details.
  if (server.command !== undefined && server.env_from.length > 0) {
    details.push({ label: "Environment", value: server.env_from.join(", ") });
  }
  return details;
}

function repositoryDetails(repo: ExportedCodeRepository): Detail[] {
  const details: Detail[] = [{ label: "Path", value: repo.root_path }];
  const origin = repo.origin_url ?? repo.cloned_from;
  if (origin) details.push({ label: "URL", value: origin });
  if (repo.setup_script) {
    details.push({ label: "Setup script", value: repo.setup_script });
  }
  if (repo.archive_script) {
    details.push({ label: "Archive script", value: repo.archive_script });
  }
  const onCreate = (repo.quick_actions ?? []).filter(
    (action) => action.auto_run_on_create,
  );
  if (onCreate.length > 0) {
    details.push({
      label: "Runs on create",
      value: onCreate.map((action) => action.command).join("\n"),
    });
  }
  return details;
}

/** The choices the server accepts for an entry in this state. An existing
 * record can only be kept or replaced; a new one can only be skipped or
 * added. An entry that needs a remap may be either, so it offers all three. */
function choicesFor(
  status: WorkspaceConfigPreviewStatus,
): WorkspaceConfigAction[] {
  switch (status) {
    case "new":
      return ["skip", "add"];
    case "identical":
    case "conflict":
      return ["skip", "replace"];
    case "needs_remap":
      return ["skip", "add", "replace"];
  }
}

const CHOICE_LABEL: Record<WorkspaceConfigAction, string> = {
  skip: "Skip",
  add: "Add",
  replace: "Replace",
};

/** What a missing remap is called in a sentence. */
const REMAP_NOUN: Record<string, string> = {
  command: "command on this machine",
  cwd: "working directory on this machine",
  root_path: "path on this machine",
};

const REMAP_LABEL: Record<string, string> = {
  command: "Command on this machine",
  cwd: "Working directory on this machine",
  root_path: "Path on this machine",
};

function PreviewRow({
  entry,
  document,
  action,
  remap,
  start,
  onAction,
  onRemap,
  onStart,
}: {
  entry: WorkspaceConfigPreviewEntry;
  document: WorkspaceConfigDocument;
  action: WorkspaceConfigAction;
  remap: Record<string, string>;
  start: boolean;
  onAction: (action: WorkspaceConfigAction) => void;
  onRemap: (field: string, value: string) => void;
  onStart: (start: boolean) => void;
}) {
  const startId = useId();
  const server = findMcpServer(document, entry);
  const repo = findRepository(document, entry);
  const title = repo?.display_name ?? entry.key;
  const details = server
    ? mcpDetails(server)
    : repo
      ? repositoryDetails(repo)
      : [];
  const local = server?.command !== undefined;
  const sent = server ? sentVariables(server) : [];
  const waits = server !== undefined && waitsForStart(server);
  const offersStart = waits && server?.enabled === true && action !== "skip";
  // An entry that needs a path on this machine stays on Skip until it has
  // one: importing it without the path would fail the whole import.
  const missing =
    entry.status === "needs_remap" && !remapComplete(entry, remap)
      ? (entry.remap_fields ?? []).filter(
          (field) => (remap[field] ?? "").trim() === "",
        )
      : [];
  return (
    <li className="rounded-lg border p-3">
      <p className="text-sm font-medium break-all">{title}</p>
      <p className="text-xs text-muted-foreground">
        {statusCopy(entry)}
        {entry.differing_fields && entry.differing_fields.length > 0 ? (
          <span> Differing: {entry.differing_fields.join(", ")}.</span>
        ) : null}
      </p>
      {details.length > 0 && (
        <dl className="mt-2 grid grid-cols-[9.5rem_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
          {details.map((detail) => (
            <Fragment key={detail.label}>
              <dt className="text-muted-foreground">{detail.label}</dt>
              <dd className="font-mono break-words whitespace-pre-wrap">
                {detail.value}
              </dd>
            </Fragment>
          ))}
        </dl>
      )}
      {server?.url && sent.length > 0 && (
        <p className="mt-2 text-xs break-words">
          Sends <code className="font-mono">{sent.join(", ")}</code> to{" "}
          <code className="font-mono">{urlHost(server.url)}</code>.
        </p>
      )}
      <div
        className="mt-3 flex flex-wrap gap-2"
        role="group"
        aria-label={`Import choice for ${title}`}
      >
        {choicesFor(entry.status).map((choice) => (
          <Button
            key={choice}
            type="button"
            size="sm"
            variant={action === choice ? "default" : "outline"}
            aria-pressed={action === choice}
            disabled={choice !== "skip" && missing.length > 0}
            onClick={() => onAction(choice)}
          >
            {CHOICE_LABEL[choice]}
          </Button>
        ))}
      </div>
      {missing.length > 0 && (
        <p className="mt-2 text-xs text-muted-foreground">
          Skipped until you enter the{" "}
          {missing.map((field) => REMAP_NOUN[field] ?? field).join(" and ")}{" "}
          below.
        </p>
      )}
      {offersStart && (
        <div className="mt-3 flex items-start gap-2">
          <Switch
            id={startId}
            checked={start}
            onCheckedChange={onStart}
            aria-label={`Start ${title} after import`}
            aria-describedby={`${startId}-hint`}
          />
          <div className="flex min-w-0 flex-col gap-0.5">
            <label htmlFor={startId} className="text-sm font-medium">
              Start after import
            </label>
            <p id={`${startId}-hint`} className="text-xs text-muted-foreground">
              {!start
                ? "Imports turned off. You can turn it on later in Connected apps."
                : local
                  ? "Tidebreak shows you the command and asks before it runs."
                  : "Connects when you apply the import."}
            </p>
          </div>
        </div>
      )}
      {waits && server?.enabled === false && action !== "skip" && (
        <p className="mt-2 text-xs text-muted-foreground">
          Imports turned off, as in the file.
        </p>
      )}
      {(entry.remap_fields ?? []).map((field) => (
        <label key={field} className="mt-3 block text-xs">
          <span className="text-muted-foreground">
            {REMAP_LABEL[field] ?? `${field} on this machine`}
          </span>
          <Input
            className="mt-1 font-mono"
            aria-label={`Remap ${field} for ${entry.key}`}
            value={remap[field] ?? ""}
            onChange={(event) => onRemap(field, event.target.value)}
          />
        </label>
      ))}
    </li>
  );
}

function statusCopy(entry: WorkspaceConfigPreviewEntry): string {
  switch (entry.status) {
    case "new":
      return "New on this machine.";
    case "identical":
      return "Already matches this machine.";
    case "conflict":
      return "Conflicts with an existing record.";
    case "needs_remap":
      return "Needs a path or command on this machine.";
  }
}
