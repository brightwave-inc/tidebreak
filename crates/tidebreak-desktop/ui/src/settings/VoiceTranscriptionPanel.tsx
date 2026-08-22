import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";

import type {
  ApiClient,
  LocalVoiceModelInfo,
  VoiceTranscriptionInfo,
} from "../api";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useConfirm } from "@/components/ConfirmDialog";
import { attachedRemotely } from "@/host";
import { hostMachineLabel } from "@/remoteMachine";
import { cn } from "@/lib/utils";
import {
  formatModelSize,
  localVoiceProgress,
  selectedLocalVoiceModel,
  useVoiceInputStore,
  voiceSelectionReady,
} from "@/VoiceInputStore";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

const LOCAL_VALUE_PREFIX = "local:";

/**
 * Why no speech model can run where the transcription happens.
 *
 * The server answers `unavailable` when it has no local runner at all, which
 * a browser build and a headless machine both report. Naming the desktop app
 * is the fix in the first case and a false lead in the second, so the two get
 * different sentences.
 */
function localModelsUnavailableCopy(): string {
  return attachedRemotely()
    ? "The machine your work is on cannot run a speech model. Pick a cloud model instead."
    : "Models that run on this computer are only available in the desktop app.";
}

/** What the closed picker says once a choice is made. */
export function voiceSelectionLabel(
  info: VoiceTranscriptionInfo | null,
): string {
  if (!info) return "Loading…";
  if (info.model === "gpt4o_transcribe") return "OpenAI · gpt-4o-transcribe";
  if (info.model === "gemini_flash") return "Google · Gemini 3.6 Flash";
  const local = selectedLocalVoiceModel(info);
  const host = hostMachineLabel();
  return local ? `On ${host} · ${local.label}` : `On ${host}`;
}

export function voiceSelectionValue(
  info: VoiceTranscriptionInfo | null,
): string {
  if (!info) return "";
  return info.model === "local"
    ? `${LOCAL_VALUE_PREFIX}${info.local_model}`
    : info.model;
}

export function VoiceTranscriptionPanel({ client }: { client: ApiClient }) {
  const navigate = useNavigate();
  const info = useVoiceInputStore((state) => state.info);
  const loading = useVoiceInputStore((state) => state.loading);
  const installing = useVoiceInputStore((state) => state.installing);
  const error = useVoiceInputStore((state) => state.error);
  const { confirm, dialog: confirmDialog } = useConfirm();
  const providersPath: string = "/settings/providers";

  useEffect(() => {
    void useVoiceInputStore.getState().load(client);
  }, [client]);

  const ready = voiceSelectionReady(info);
  const selectedLocal = selectedLocalVoiceModel(info);
  const downloading = info?.local_models.find(
    (model) => model.state === "downloading" || model.id === installing,
  );

  /**
   * A model the reader has not downloaded is still offered: choosing it is how
   * you ask for it. The confirmation says what will be fetched and what it
   * costs on disk before anything is written.
   */
  async function choose(value: string) {
    if (!value.startsWith(LOCAL_VALUE_PREFIX)) {
      await useVoiceInputStore
        .getState()
        .select(client, value as "gpt4o_transcribe" | "gemini_flash");
      return;
    }
    const id = value.slice(LOCAL_VALUE_PREFIX.length);
    const model = info?.local_models.find((entry) => entry.id === id);
    if (!model) return;
    if (model.state === "ready") {
      await useVoiceInputStore.getState().select(client, "local", id);
      return;
    }
    const accepted = await confirm({
      title: `Download ${model.label}?`,
      description: `${model.description} The download is about ${formatModelSize(
        model.total_bytes,
      )} and uses the same amount of disk once installed. It is downloaded to ${hostMachineLabel()}, and recordings transcribed with it never leave there.`,
      confirmLabel: "Download",
    });
    if (accepted) await useVoiceInputStore.getState().install(client, id);
  }

  return (
    <SettingsPanel
      title="Voice input"
      description={`Choose how microphone recordings become editable message drafts. Recordings are transcribed on ${hostMachineLabel()}, and the audio stays there unless you pick a cloud model.`}
      busy={loading}
    >
      <SettingsSection>
        <SettingsStatus
          tone={ready ? "ready" : "not-configured"}
          label={ready ? "Ready" : "Setup required"}
          description={
            ready
              ? "The selected voice input model is ready to transcribe recordings."
              : `Download a model that runs on ${hostMachineLabel()}, or configure a supported cloud provider.`
          }
        />
      </SettingsSection>

      <SettingsSection
        title="Transcription model"
        description={`Models that run on ${hostMachineLabel()} come first. One that has not been downloaded yet is still selectable — choosing it starts the download.`}
      >
        <SettingsField
          label="Model"
          hint="A larger model transcribes more accurately and takes longer per recording."
        >
          <Select
            value={voiceSelectionValue(info)}
            disabled={!info || loading}
            onValueChange={(value) => void choose(value)}
          >
            <SelectTrigger aria-label="Voice input model">
              <SelectValue placeholder="Loading…">
                {voiceSelectionLabel(info)}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                <SelectLabel>On {hostMachineLabel()}</SelectLabel>
                {(info?.local_models ?? []).map((model) => (
                  <LocalModelItem key={model.id} model={model} />
                ))}
              </SelectGroup>
              {(info?.openai_ready || info?.gemini_ready) && (
                <SelectGroup>
                  <SelectLabel>Cloud</SelectLabel>
                  {info?.openai_ready && (
                    <SelectItem value="gpt4o_transcribe">
                      OpenAI · gpt-4o-transcribe
                    </SelectItem>
                  )}
                  {info?.gemini_ready && (
                    <SelectItem value="gemini_flash">
                      Google · Gemini 3.6 Flash
                    </SelectItem>
                  )}
                </SelectGroup>
              )}
            </SelectContent>
          </Select>
        </SettingsField>

        {downloading?.state === "downloading" && (
          <div className="flex flex-col gap-1.5" role="status">
            <span className="text-sm text-muted-foreground">
              {`Downloading ${downloading.label} — ${formatModelSize(
                downloading.downloaded_bytes ?? 0,
              )} of ${formatModelSize(downloading.total_bytes)}`}
            </span>
            <Progress value={localVoiceProgress(downloading)} />
          </div>
        )}

        {selectedLocal?.state === "failed" && (
          <div className="flex flex-col items-start gap-2">
            <SettingsError>
              {selectedLocal.error ??
                `${selectedLocal.label} did not download.`}
            </SettingsError>
            <Button
              type="button"
              variant="outline"
              disabled={loading}
              onClick={() =>
                void useVoiceInputStore
                  .getState()
                  .install(client, selectedLocal.id)
              }
            >
              Retry download
            </Button>
          </div>
        )}

        {selectedLocal?.state === "not_installed" && (
          <Button
            type="button"
            className="w-fit"
            disabled={loading}
            onClick={() =>
              void useVoiceInputStore
                .getState()
                .install(client, selectedLocal.id)
            }
          >
            {`Download ${selectedLocal.label}`}
          </Button>
        )}

        {selectedLocal?.state === "unavailable" && (
          <SettingsStatus
            tone="disabled"
            label="Not available here"
            description={localModelsUnavailableCopy()}
          />
        )}
      </SettingsSection>

      {info && !info.openai_ready && !info.gemini_ready && (
        <SettingsSection title="Cloud models">
          <SettingsStatus
            tone="not-configured"
            label="No cloud voice model configured"
            description="OpenAI gpt-4o-transcribe and Google Gemini appear in the picker once their provider is enabled and credentialed."
          />
          <Button
            type="button"
            variant="outline"
            className="w-fit"
            onClick={() => void navigate({ to: providersPath })}
          >
            Open Providers
          </Button>
        </SettingsSection>
      )}
      {error && <SettingsError>{error}</SettingsError>}
      {confirmDialog}
    </SettingsPanel>
  );
}

/**
 * A catalog row. A model that is not on disk is ghosted rather than disabled:
 * it is a real choice, it just costs a download first.
 */
function LocalModelItem({ model }: { model: LocalVoiceModelInfo }) {
  const installed = model.state === "ready";
  return (
    <SelectItem
      value={`${LOCAL_VALUE_PREFIX}${model.id}`}
      className={cn("items-start py-2", !installed && "opacity-60")}
    >
      <span className="flex flex-col gap-0.5">
        <span className="flex flex-wrap items-center gap-2">
          <span className="font-medium">{model.label}</span>
          <span className="text-xs text-muted-foreground">
            {formatModelSize(model.total_bytes)}
          </span>
          {model.recommended && (
            <span className="rounded-full border px-1.5 text-[0.68rem] text-muted-foreground">
              Recommended
            </span>
          )}
          {!installed && (
            <span className="text-xs text-muted-foreground">
              {model.state === "downloading"
                ? "Downloading…"
                : "Not downloaded"}
            </span>
          )}
        </span>
        <span className="text-xs text-muted-foreground">
          {model.description}
        </span>
      </span>
    </SelectItem>
  );
}
