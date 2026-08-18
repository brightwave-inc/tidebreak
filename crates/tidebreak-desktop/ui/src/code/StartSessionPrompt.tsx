import { useEffect, useState } from "react";

import type { ApiClient } from "../api/client";
import type {
  CodePermissionMode,
  HarnessDoctorEntry,
  HarnessKind,
  ModelInfo,
} from "../api/types";
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
 * posts a mode that engine can honor. Unsupervised Auto says so before the
 * session exists (decision 0038).
 */
export function StartSessionPrompt({
  harnesses,
  starting,
  selectedMode,
  onSelectMode,
  onStart,
  client,
  catalogModels = NO_CATALOG_MODELS,
  defaultModelKey = null,
}: {
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
}) {
  const [picked, setPicked] = useState<HarnessKind | null>(null);
  const [model, setModel] = useState<string | undefined>();
  const [modelOptions, setModelOptions] = useState<CodeModelOption[]>([]);
  const ensureHarnessModels = useCodeCatalogStore((state) => state.ensureHarnessModels);
  const ready = harnesses.filter((entry) => !harnessUnusableReason(entry));
  const selected =
    harnesses.find((entry) => entry.kind === picked && !harnessUnusableReason(entry)) ??
    ready[0];
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
      return;
    }
    if (!client) {
      setModelOptions([]);
      setModel(undefined);
      return;
    }
    let cancelled = false;
    void ensureHarnessModels(client, selectedKind).then((listed) => {
      if (cancelled) return;
      setModelOptions(listed);
      setModel(listed.find((option) => option.default)?.id ?? listed[0]?.id);
    });
    return () => {
      cancelled = true;
    };
  }, [catalogModels, client, defaultModelKey, ensureHarnessModels, selectedKind]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-col gap-3 px-4 py-6">
        <p className="text-sm">Start a session on this workspace.</p>
        <HarnessPicker
          harnesses={harnesses}
          value={selected?.kind ?? null}
          disabled={starting}
          onChange={setPicked}
        />
        {mode === "auto" && selected && autoIsUnsupervised(selected.caps) && (
          <p className="text-muted-foreground text-xs">{UNSUPERVISED_AUTO_NOTE}</p>
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
