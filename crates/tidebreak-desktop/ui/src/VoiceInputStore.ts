import { create } from "zustand";
import type {
  ApiClient,
  LocalVoiceModelInfo,
  VoiceTranscriptionInfo,
  VoiceTranscriptionModel,
} from "./api";

type VoiceInputStore = {
  info: VoiceTranscriptionInfo | null;
  loading: boolean;
  /** The model id currently downloading, so the picker can show its progress. */
  installing: string | null;
  error: string | null;
  load: (client: ApiClient) => Promise<VoiceTranscriptionInfo | null>;
  select: (
    client: ApiClient,
    model: VoiceTranscriptionModel,
    localModel?: string,
  ) => Promise<void>;
  install: (client: ApiClient, model: string) => Promise<void>;
};

export const useVoiceInputStore = create<VoiceInputStore>()((set, get) => ({
  info: null,
  loading: false,
  installing: null,
  error: null,
  async load(client) {
    set({ loading: true, error: null });
    try {
      const info = await client.getVoiceTranscription();
      set({ info, loading: false });
      return info;
    } catch (caught) {
      set({ loading: false, error: String(caught) });
      return null;
    }
  },
  async select(client, model, localModel) {
    set({ loading: true, error: null });
    try {
      set({
        info: await client.putVoiceTranscription(model, localModel),
        loading: false,
      });
    } catch (caught) {
      set({ loading: false, error: String(caught) });
    }
  },
  /**
   * Download one catalog model, then select it.
   *
   * Progress is only known on the server side of the download, so the bar is
   * fed by polling the info endpoint while the install request is in flight.
   */
  async install(client, model) {
    set({ loading: true, installing: model, error: null });
    let stopped = false;
    const poll = async () => {
      while (!stopped) {
        await new Promise((resolve) => window.setTimeout(resolve, 500));
        if (stopped) break;
        const info = await client.getVoiceTranscription();
        set({ info });
        if (localVoiceModel(info, model)?.state !== "downloading") break;
      }
    };
    const polling = poll().catch(() => undefined);
    try {
      await client.installLocalVoice(model);
      stopped = true;
      await polling;
      set({ installing: null });
      await get().select(client, "local", model);
    } catch (caught) {
      set({ loading: false, installing: null, error: String(caught) });
      const info = await client.getVoiceTranscription().catch(() => null);
      if (info) set({ info });
    } finally {
      stopped = true;
      await polling;
    }
  },
}));

export function localVoiceModel(
  info: VoiceTranscriptionInfo | null,
  id: string,
): LocalVoiceModelInfo | undefined {
  return info?.local_models.find((model) => model.id === id);
}

export function selectedLocalVoiceModel(
  info: VoiceTranscriptionInfo | null,
): LocalVoiceModelInfo | undefined {
  return info ? localVoiceModel(info, info.local_model) : undefined;
}

export function voiceSelectionReady(
  info: VoiceTranscriptionInfo | null,
): boolean {
  if (!info) return false;
  if (info.model === "local") {
    return selectedLocalVoiceModel(info)?.state === "ready";
  }
  if (info.model === "gpt4o_transcribe") return info.openai_ready;
  return info.gemini_ready;
}

/** A model's size, for the confirmation dialog and the picker rows. */
export function formatModelSize(bytes: number): string {
  if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
  return `${Math.round(bytes / 1_000_000)} MB`;
}

export function localVoiceProgress(model: LocalVoiceModelInfo): number {
  if (!model.total_bytes) return 0;
  return Math.min(
    100,
    ((model.downloaded_bytes ?? 0) / model.total_bytes) * 100,
  );
}
