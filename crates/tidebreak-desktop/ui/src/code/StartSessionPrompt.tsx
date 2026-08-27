import { useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import type {
  PermissionMode,
  HarnessDoctorEntry,
  HarnessKind,
  ModelInfo,
  ReasoningEffort,
} from "../api/types";
import type { ComposerWorkspaceFiles } from "@/Composer";
import { CodeComposer } from "./CodeComposer";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { HarnessInstallNote } from "./HarnessInstallNote";
import { HarnessPicker } from "./HarnessPicker";
import {
  canInstallHarnesses,
  useWarmHarnessInstall,
} from "./useHarnessInstall";
import {
  CREATE_PERMISSION_MODE_FIXED,
  createPermissionModes,
  defaultCreatePermissionMode,
  effortLadder,
  gatewayCodeModels,
  harnessCanStartNow,
  harnessUnusableReason,
  preferredCodeModels,
  type CodeModelOption,
} from "./labels";

const NO_ENGINE_EFFORTS: ReasoningEffort[] = [];

const NO_CATALOG_MODELS: ModelInfo[] = [];

/**
 * Create-time harness + permission mode for a workspace with no session.
 *
 * The harness dropdown defaults to the first engine that can start now; the
 * mode list and default follow the selected engine's own capability flags, so
 * start always posts a mode that engine can honor (decisions 0038, 0039).
 *
 * The harness sits in the composer beside the model so it stays next to the
 * draft it controls. An engine this machine has not downloaded yet is still
 * on offer. Picking it starts the download and the composer says so; start
 * stays disabled until the pin lands, because create would otherwise sit on
 * the same npm install with nothing on screen.
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
  selectedMode: PermissionMode | null;
  onSelectMode: (mode: PermissionMode) => void;
  onStart: (
    harness: HarnessKind,
    mode: PermissionMode,
    message: string,
    model?: string,
    draft?: string,
    reasoningEffort?: ReasoningEffort | null,
    fastMode?: boolean,
  ) => Promise<void> | void;
  client?: Pick<ApiClient, "listCodeHarnessModels"> &
    Partial<Pick<ApiClient, "startHarnessInstall" | "getHarnessDoctor">>;
  catalogModels?: ModelInfo[];
  defaultModelKey?: string | null;
  /** Worktree files the first message names — a fork's transcript. */
  workspaceFiles?: ComposerWorkspaceFiles;
}) {
  const lastCreate = useCodeUiStore((state) => state.lastCreate);
  const [picked, setPicked] = useState<HarnessKind | null>(
    lastCreate?.harness ?? null,
  );
  const [modelsByHarness, setModelsByHarness] = useState<
    Partial<Record<HarnessKind, string>>
  >({ ...lastCreate?.modelsByHarness });
  const [effortByHarness, setEffortByHarness] = useState<
    Partial<Record<HarnessKind, ReasoningEffort>>
  >({ ...lastCreate?.reasoningEffortByHarness });
  const [fastByHarness, setFastByHarness] = useState<
    Partial<Record<HarnessKind, boolean>>
  >({ ...lastCreate?.fastModeByHarness });
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const submittedDraft = useRef<string | null>(null);
  const ensureHarnessModels = useCodeCatalogStore(
    (state) => state.ensureHarnessModels,
  );
  const selectable = harnesses.filter((entry) => !harnessUnusableReason(entry));
  const selected =
    harnesses.find(
      (entry) => entry.kind === picked && !harnessUnusableReason(entry),
    ) ??
    selectable.find((entry) => harnessCanStartNow(entry)) ??
    selectable[0];
  const availableModes = selected ? createPermissionModes(selected.caps) : [];
  const mode: PermissionMode =
    selectedMode && availableModes.includes(selectedMode)
      ? selectedMode
      : selected
        ? defaultCreatePermissionMode(selected.caps)
        : "plan";

  const selectedKind = selected?.kind;
  const model = selectedKind ? modelsByHarness[selectedKind] : undefined;
  const engineEfforts =
    useCodeCatalogStore((state) =>
      selectedKind ? state.effortsByHarness[selectedKind] : undefined,
    ) ?? NO_ENGINE_EFFORTS;
  const selectedOption =
    modelOptions.find((option) => option.id === model) ??
    modelOptions.find((option) => option.default) ??
    modelOptions[0];
  const effortLevels = effortLadder(selectedOption, engineEfforts);
  const fastModeAvailable = selectedOption?.fast_mode ?? false;
  const rememberedEffort = selectedKind
    ? effortByHarness[selectedKind]
    : undefined;
  const postedEffort =
    rememberedEffort && effortLevels.includes(rememberedEffort)
      ? rememberedEffort
      : null;
  const postedFastMode = fastModeAvailable
    ? Boolean(selectedKind && fastByHarness[selectedKind])
    : false;
  const installed = Boolean(selected?.found);
  const install = useWarmHarnessInstall(
    canInstallHarnesses(client) ? client : undefined,
    selectedKind,
    true,
    Boolean(selected && !selected.found),
  );

  useEffect(() => {
    if (!selectedKind) {
      setModelOptions([]);
      setModelLoading(false);
      return;
    }
    const apply = (listed: CodeModelOption[]) => {
      setModelOptions(listed);
      setModelsByHarness((current) => {
        const remembered = current[selectedKind];
        const picked =
          remembered && listed.some((option) => option.id === remembered)
            ? remembered
            : (listed.find((option) => option.default)?.id ?? listed[0]?.id);
        if (picked === remembered) return current;
        const next = { ...current };
        if (picked) next[selectedKind] = picked;
        else delete next[selectedKind];
        return next;
      });
    };
    const gateway = gatewayCodeModels(
      catalogModels,
      selectedKind,
      defaultModelKey,
    );
    const native = useCodeCatalogStore.getState().modelsByHarness[selectedKind];
    // Same as the in-workspace composer: always load the harness listing so
    // gateway rows can inherit `fast_mode` (and per-model effort ladders).
    if (native !== undefined) {
      apply(preferredCodeModels(selectedKind, native, gateway));
      setModelLoading(false);
      return;
    }
    if (!client) {
      apply(gateway);
      setModelLoading(false);
      return;
    }
    setModelOptions([]);
    setModelLoading(true);
    let cancelled = false;
    void ensureHarnessModels(client, selectedKind).then((listed) => {
      if (cancelled) return;
      apply(preferredCodeModels(selectedKind, listed, gateway));
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
      <div className="px-4 py-6">
        <p className="text-sm">Start a session on this workspace.</p>
      </div>
      <div className="mt-auto">
        <CodeComposer
          disabled={starting || !selected || !installed}
          running={starting}
          permissionMode={mode}
          availableModes={availableModes}
          harness={selected?.kind}
          model={model}
          modelOptions={modelOptions}
          modelLoading={modelLoading}
          reasoningEffort={postedEffort}
          engineEfforts={engineEfforts}
          fastMode={postedFastMode}
          harnessMenu={
            <HarnessPicker
              harnesses={harnesses}
              value={selected?.kind ?? null}
              disabled={starting}
              variant="composer"
              onChange={(next) => {
                setModelOptions([]);
                setModelLoading(true);
                setPicked(next);
              }}
            />
          }
          footerNote={
            <>
              {selected?.relaunch_composes_permission_mode === false && (
                <p className="text-muted-foreground text-xs">
                  {CREATE_PERMISSION_MODE_FIXED}
                </p>
              )}
              <HarnessInstallNote install={install} />
            </>
          }
          promptScope={workspaceId}
          workspaceFiles={workspaceFiles}
          onModelChange={(next) => {
            if (!selectedKind) return;
            setModelsByHarness((current) => ({
              ...current,
              [selectedKind]: next,
            }));
          }}
          onEffortChange={(next) => {
            if (!selectedKind) return;
            setEffortByHarness((current) => {
              const nextMap = { ...current };
              if (next) nextMap[selectedKind] = next;
              else delete nextMap[selectedKind];
              return nextMap;
            });
          }}
          onFastModeChange={(next) => {
            if (!selectedKind) return;
            setFastByHarness((current) => ({
              ...current,
              [selectedKind]: next,
            }));
          }}
          onModeChange={onSelectMode}
          onSubmitStart={(draft) => {
            submittedDraft.current = draft;
          }}
          onSend={async (message) => {
            if (!selected) return;
            const draft = submittedDraft.current ?? message;
            try {
              if (draft === message) {
                await onStart(
                  selected.kind,
                  mode,
                  message,
                  model,
                  undefined,
                  postedEffort,
                  postedFastMode,
                );
              } else {
                await onStart(
                  selected.kind,
                  mode,
                  message,
                  model,
                  draft,
                  postedEffort,
                  postedFastMode,
                );
              }
            } finally {
              submittedDraft.current = null;
            }
          }}
          onInterrupt={() => undefined}
        />
      </div>
    </div>
  );
}
