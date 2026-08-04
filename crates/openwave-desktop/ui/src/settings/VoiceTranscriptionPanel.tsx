import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";

import type { ApiClient, VoiceTranscriptionModel } from "../api";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useVoiceInputStore } from "@/VoiceInputStore";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

export function VoiceTranscriptionPanel({ client }: { client: ApiClient }) {
  const navigate = useNavigate();
  const info = useVoiceInputStore((state) => state.info);
  const loading = useVoiceInputStore((state) => state.loading);
  const error = useVoiceInputStore((state) => state.error);
  const providersPath: string = "/settings/providers";

  useEffect(() => {
    void useVoiceInputStore.getState().load(client);
  }, [client]);

  const localReady = info?.local.state === "ready";
  const selectedReady = info
    ? info.model === "local"
      ? localReady
      : info.model === "gpt4o_transcribe"
        ? info.openai_ready
        : info.gemini_ready
    : false;
  const localProgress =
    info?.local.downloaded_bytes != null && info.local.total_bytes
      ? (info.local.downloaded_bytes / info.local.total_bytes) * 100
      : 0;

  return (
    <SettingsPanel
      title="Voice input"
      description="Choose how microphone recordings become editable message drafts. Audio stays local when the local model is selected."
      busy={loading}
    >
      <SettingsSection>
        <SettingsStatus
          tone={selectedReady ? "ready" : "not-configured"}
          label={selectedReady ? "Ready" : "Setup required"}
          description={
            selectedReady
              ? "The selected voice input model is ready to transcribe recordings."
              : "Install the local model or configure a supported cloud provider before recording."
          }
        />
      </SettingsSection>

      <SettingsSection title="Transcription model">
        <SettingsField
          label="Model"
          hint="Local is recommended and never sends recordings off this device."
        >
          <Select
            value={info?.model ?? "local"}
            disabled={!info || loading}
            onValueChange={(value) =>
              void useVoiceInputStore
                .getState()
                .select(client, value as VoiceTranscriptionModel)
            }
          >
            <SelectTrigger aria-label="Voice input model">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="local">Local · Whisper tiny English</SelectItem>
              {info?.openai_ready && (
                <SelectItem value="gpt4o_transcribe">OpenAI · gpt-4o-transcribe</SelectItem>
              )}
              {info?.gemini_ready && (
                <SelectItem value="gemini_flash">Google · Gemini 3.6 Flash</SelectItem>
              )}
            </SelectContent>
          </Select>
        </SettingsField>
      </SettingsSection>

      <SettingsSection title="Local model">
        <SettingsStatus
          tone={localReady ? "ready" : info?.local.state === "unavailable" ? "disabled" : "not-configured"}
          label={localReady ? "Installed" : info?.local.state === "downloading" ? "Downloading" : "Not installed"}
          description={
            info?.local.error ??
            (localReady
              ? "Whisper tiny English is installed and verified."
              : "Downloads a pinned 31 MB quantized model and verifies its SHA-256 before use.")
          }
        />
        {info?.local.state === "downloading" && <Progress value={localProgress} />}
        {!localReady && info?.local.state !== "unavailable" && (
          <Button
            type="button"
            className="w-fit"
            disabled={loading}
            onClick={() => void useVoiceInputStore.getState().install(client)}
          >
            {info?.local.state === "failed" ? "Retry download" : "Download local model"}
          </Button>
        )}
      </SettingsSection>

      {info && !info.openai_ready && !info.gemini_ready && (
        <SettingsSection title="Cloud models">
          <SettingsStatus
            tone="not-configured"
            label="No cloud voice model configured"
            description="OpenAI gpt-4o-transcribe and Google Gemini become available here when their provider is enabled and credentialed."
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
    </SettingsPanel>
  );
}
