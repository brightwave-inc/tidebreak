import { useEffect, useState } from "react";
import type {
  ApiClient,
  VoiceTranscriptionInfo,
  VoiceTranscriptionModel,
} from "../api";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

export function VoiceTranscriptionPanel({ client }: { client: ApiClient }) {
  const [info, setInfo] = useState<VoiceTranscriptionInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    void client.getVoiceTranscription().then(setInfo).catch((caught) => {
      setError(String(caught));
    });
  }, [client]);

  async function select(model: VoiceTranscriptionModel) {
    setSaving(true);
    setError(null);
    try {
      setInfo(await client.putVoiceTranscription(model));
    } catch (caught) {
      setError(String(caught));
    } finally {
      setSaving(false);
    }
  }

  return (
    <SettingsPanel
      title="Voice transcription"
      description="Choose how recordings become editable message drafts. Local transcription is recommended."
    >
      <SettingsSection title="Model">
        <Select
          value={info?.model ?? "local"}
          disabled={!info || saving}
          onValueChange={(value) => void select(value as VoiceTranscriptionModel)}
        >
          <SelectTrigger aria-label="Voice transcription model">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="local">Local model · recommended</SelectItem>
            <SelectItem value="gpt4o_transcribe" disabled={!info?.openai_ready}>
              OpenAI · gpt-4o-transcribe
            </SelectItem>
          </SelectContent>
        </Select>
        {info?.model === "local" && !info.local_ready && (
          <p className="text-xs text-muted-foreground">
            The pinned local model runner is not included in this build yet.
          </p>
        )}
        {info && !info.openai_ready && (
          <p className="text-xs text-muted-foreground">
            Enable OpenAI and save its credential in Providers to use cloud transcription.
          </p>
        )}
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsSection>
    </SettingsPanel>
  );
}
