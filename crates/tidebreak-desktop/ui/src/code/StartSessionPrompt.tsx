import { useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import type {
  CodePermissionMode,
  HarnessDoctorEntry,
  HarnessKind,
  ModelInfo,
} from "../api/types";
import type { ComposerWorkspaceFiles } from "@/Composer";
import { CodeComposer } from "./CodeComposer";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { HarnessPicker } from "./HarnessPicker";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  gatewayCodeModels,
  harnessUnusableReason,
  type CodeModelOption,
  ALLOW_ALL_NOTE,
  UNSUPERVISED_AUTO_NOTE,
} from "./labels";

const NO_CATALOG_MODELS: ModelInfo[] = [];

/**
 * Create-time harness + permission mode for a workspace with no session.
 *
 * The harness dropdown defaults to the first ready engine; the mode list and
 * default follow the selected engine's own capability flags, so start always
 * posts a mode that engine can honor, and an unsupervised one says so before
 * the session exists (decisions 0038, 0039).
 *
 * Cmd+Enter starts from anywhere on this surface, matching the new-workspace
 * dialog. The draft lives in the composer, so the shortcut goes through the
 * composer's own send button: one submit path, one set of disabled rules.
 */
export function StartSessionPrompt({
  workspaceId,
  harnesses,
  starting,
  selectedMode,
  onSelectMode,
  onStart,
  client,
  catalogModels = NO_CATALOG_MODELS,
  defaultModelKey = null,
  workspaceFiles,
}: {
  workspaceId: string;
  harnesses: HarnessDoctorEntry[];
  starting: boolean;
  selectedMode: CodePermissionMode | null;
  onSelectMode: (mode: CodePermissionMode) => void;
  onStart: (
    harness: HarnessKind,
    mode: CodePermissionMode,
    message: string,
    model?: string,
  ) => Promise<void> | void;
  client?: Pick<ApiClient, "listCodeHarnessModels">;
  catalogModels?: ModelInfo[];
  defaultModelKey?: string | null;
  /** Worktree files the first message names — a fork's transcript. */
  workspaceFiles?: ComposerWorkspaceFiles;
}) {
  const [picked, setPicked] = useState<HarnessKind | null>(null);
  const [model, setModel] = useState<string | undefined>();
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const ensureHarnessModels = useCodeCatalogStore(
    (state) => state.ensureHarnessModels,
  );
  const ready = harnesses.filter((entry) => !harnessUnusableReason(entry));
  const selected =
    harnesses.find(
      (entry) => entry.kind === picked && !harnessUnusableReason(entry),
    ) ?? ready[0];
  const availableModes = selected ? createPermissionModes(selected.caps) : [];
  const mode: CodePermissionMode =
    selectedMode && availableModes.includes(selectedMode)
      ? selectedMode
      : selected
        ? defaultCreatePermissionMode(selected.caps)
        : "plan";

  const selectedKind = selected?.kind;

  useEffect(() => {
    if (!selectedKind) {
      setModelOptions([]);
      setModel(undefined);
      setModelLoading(false);
      return;
    }
    const gateway = gatewayCodeModels(
      catalogModels,
      selectedKind,
      defaultModelKey,
    );
    if (gateway.length > 0) {
      setModelOptions(gateway);
      setModel(gateway.find((option) => option.default)?.id ?? gateway[0]?.id);
      setModelLoading(false);
      return;
    }
    if (!client) {
      setModelOptions([]);
      setModel(undefined);
      setModelLoading(false);
      return;
    }
    setModelOptions([]);
    setModel(undefined);
    setModelLoading(true);
    let cancelled = false;
    void ensureHarnessModels(client, selectedKind).then((listed) => {
      if (cancelled) return;
      setModelOptions(listed);
      setModel(listed.find((option) => option.default)?.id ?? listed[0]?.id);
      setModelLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [
    catalogModels,
    client,
    defaultModelKey,
    ensureHarnessModels,
    selectedKind,
  ]);

  return (
    <div
      ref={root}
      className="flex min-h-0 flex-1 flex-col"
      onKeyDownCapture={(event) => {
        if (event.key !== "Enter" || !(event.metaKey || event.ctrlKey)) return;
        const send = root.current?.querySelector<HTMLButtonElement>(
          'button[aria-label="Send message"]',
        );
        if (!send || send.disabled) return;
        event.preventDefault();
        send.click();
      }}
    >
      <div className="flex flex-col gap-3 px-4 py-6">
        <p className="text-sm">Start a session on this workspace.</p>
        <HarnessPicker
          harnesses={harnesses}
          value={selected?.kind ?? null}
          disabled={starting}
          onChange={(next) => {
            setModelOptions([]);
            setModel(undefined);
            setModelLoading(true);
            setPicked(next);
          }}
        />
        {mode === "auto" && selected && autoIsUnsupervised(selected.caps) && (
          <p className="text-muted-foreground text-xs">
            {UNSUPERVISED_AUTO_NOTE}
          </p>
        )}
        {mode === "allow" && (
          <p className="text-muted-foreground text-xs">{ALLOW_ALL_NOTE}</p>
        )}
      </div>
      <div className="mt-auto">
        <CodeComposer
          disabled={starting || !selected}
          running={starting}
          permissionMode={mode}
          availableModes={availableModes}
          harness={selected?.kind}
          model={model}
          modelOptions={modelOptions}
          modelLoading={modelLoading}
          promptScope={workspaceId}
          workspaceFiles={workspaceFiles}
          onModelChange={setModel}
          onModeChange={onSelectMode}
          onSend={async (message) => {
            if (!selected) return;
            await onStart(selected.kind, mode, message, model);
          }}
          onInterrupt={() => undefined}
        />
      </div>
    </div>
  );
}
