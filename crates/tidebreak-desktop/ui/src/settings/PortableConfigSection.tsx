import { Fragment, useId, useRef, useState } from "react";
import { toast } from "sonner";
import { Download, Upload } from "lucide-react";

import type { ApiClient } from "@/api/client";
import type {
  ExportedCodeRepository,
  ExportedMcpServer,
  WorkspaceConfigAction,
  WorkspaceConfigApplyRequest,
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
import { SettingsError, SettingsSection } from "./primitives";

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
  const [remaps, setRemaps] = useState<Record<string, Record<string, string>>>(
    {},
  );
  // Local command servers the person chose to start after import. Every one
  // starts turned off: starting one runs a program on this computer, and
  // Tidebreak asks for that in a native dialog, never by default.
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
      setRemaps({});
      setStarts({});
      setApplyError(null);
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
            action: actions[key] ?? "skip",
            remaps: filledRemaps(remaps[key]),
          };
          const server = findMcpServer(preview.document, entry);
          if (server && startsLocalCommand(server)) {
            decision.enabled = starts[key] === true;
          }
          return decision;
        }),
      };
      const result = await client.applyWorkspaceConfig(body);
      toast.success(
        `Imported ${result.applied} ${result.applied === 1 ? "item" : "items"}; skipped ${result.skipped}.`,
      );
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
              an existing record only when you choose Replace, and local MCP
              servers import turned off unless you choose to start them.
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
                  action={actions[entryKey(entry)] ?? "skip"}
                  remap={remaps[entryKey(entry)] ?? {}}
                  start={starts[entryKey(entry)] === true}
                  onAction={(next) =>
                    setActions((current) => ({
                      ...current,
                      [entryKey(entry)]: next,
                    }))
                  }
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
          {applyError && <SettingsError>{applyError}</SettingsError>}
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

/** An enabled server with a command starts a program on this computer. */
function startsLocalCommand(server: ExportedMcpServer): boolean {
  return server.enabled && server.command !== undefined;
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
  if (server.env_from.length > 0) {
    details.push({ label: "Environment", value: server.env_from.join(", ") });
  }
  if (server.bearer_token_env) {
    details.push({
      label: "Bearer token variable",
      value: server.bearer_token_env,
    });
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
  const local = server !== undefined && server.command !== undefined;
  const offersStart =
    server !== undefined && startsLocalCommand(server) && action !== "skip";
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
            onClick={() => onAction(choice)}
          >
            {CHOICE_LABEL[choice]}
          </Button>
        ))}
      </div>
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
              {start
                ? "Tidebreak shows you the command and asks before it runs."
                : "Imports turned off. You can turn it on later in Connected apps."}
            </p>
          </div>
        </div>
      )}
      {local && server?.enabled === false && action !== "skip" && (
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
