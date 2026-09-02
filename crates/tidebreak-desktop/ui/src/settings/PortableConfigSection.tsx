import { useRef, useState } from "react";
import { toast } from "sonner";
import { Download, Upload } from "lucide-react";

import type { ApiClient } from "@/api/client";
import type {
  WorkspaceConfigAction,
  WorkspaceConfigApplyRequest,
  WorkspaceConfigDocument,
  WorkspaceConfigPreview,
  WorkspaceConfigPreviewEntry,
} from "@/api/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
  const [applying, setApplying] = useState(false);

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
      const nextActions: Record<string, WorkspaceConfigAction> = {};
      for (const entry of result.entries) {
        nextActions[entryKey(entry)] =
          entry.status === "identical" ? "skip" : "add";
      }
      setActions(nextActions);
      setRemaps({});
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
    setError(null);
    try {
      const body: WorkspaceConfigApplyRequest = {
        document: preview.document,
        decisions: preview.entries.map((entry) => ({
          section: entry.section,
          key: entry.key,
          action: actions[entryKey(entry)] ?? "skip",
          remaps: remaps[entryKey(entry)] ?? {},
        })),
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
      setError(message);
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
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Import workspace configuration</DialogTitle>
            <DialogDescription>
              Review each entry. Tidebreak overwrites an existing record only
              when you choose Replace.
            </DialogDescription>
          </DialogHeader>
          {preview && (
            <ul
              className="flex max-h-80 flex-col gap-3 overflow-auto"
              aria-label="Import preview"
            >
              {preview.entries.map((entry) => (
                <PreviewRow
                  key={entryKey(entry)}
                  entry={entry}
                  action={actions[entryKey(entry)] ?? "skip"}
                  remap={remaps[entryKey(entry)] ?? {}}
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
                />
              ))}
            </ul>
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

function PreviewRow({
  entry,
  action,
  remap,
  onAction,
  onRemap,
}: {
  entry: WorkspaceConfigPreviewEntry;
  action: WorkspaceConfigAction;
  remap: Record<string, string>;
  onAction: (action: WorkspaceConfigAction) => void;
  onRemap: (field: string, value: string) => void;
}) {
  return (
    <li className="rounded-lg border p-3">
      <p className="text-sm font-medium">{entry.key}</p>
      <p className="text-xs text-muted-foreground">
        {statusCopy(entry)}
        {entry.differing_fields && entry.differing_fields.length > 0 ? (
          <span> Differing: {entry.differing_fields.join(", ")}.</span>
        ) : null}
      </p>
      <div className="mt-2 flex flex-wrap gap-2">
        {(["skip", "add", "replace"] as const).map((choice) => (
          <Button
            key={choice}
            type="button"
            size="sm"
            variant={action === choice ? "default" : "outline"}
            onClick={() => onAction(choice)}
          >
            {choice === "skip" ? "Skip" : choice === "add" ? "Add" : "Replace"}
          </Button>
        ))}
      </div>
      {(entry.remap_fields ?? []).map((field) => (
        <label key={field} className="mt-2 block text-xs">
          <span className="text-muted-foreground">{field} on this machine</span>
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
