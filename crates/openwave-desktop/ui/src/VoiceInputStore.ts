import { create } from "zustand";
import type { ApiClient, VoiceTranscriptionInfo, VoiceTranscriptionModel } from "./api";

type VoiceInputStore = {
  info: VoiceTranscriptionInfo | null;
  loading: boolean;
  error: string | null;
  load: (client: ApiClient) => Promise<VoiceTranscriptionInfo | null>;
  select: (client: ApiClient, model: VoiceTranscriptionModel) => Promise<void>;
  install: (client: ApiClient) => Promise<void>;
};

export const useVoiceInputStore = create<VoiceInputStore>()((set) => ({
  info: null,
  loading: false,
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
  async select(client, model) {
    set({ loading: true, error: null });
    try {
      set({ info: await client.putVoiceTranscription(model), loading: false });
    } catch (caught) {
      set({ loading: false, error: String(caught) });
    }
  },
  async install(client) {
    set((state) => ({
      loading: true,
      error: null,
      info: state.info
        ? {
            ...state.info,
            local: {
              ...state.info.local,
              state: "downloading",
              downloaded_bytes: 0,
            },
          }
        : null,
    }));
    let stopped = false;
    const poll = async () => {
      while (!stopped) {
        await new Promise((resolve) => window.setTimeout(resolve, 500));
        if (stopped) break;
        const info = await client.getVoiceTranscription();
        set({ info });
        if (info.local.state !== "downloading") break;
      }
    };
    const polling = poll().catch(() => undefined);
    try {
      await client.installLocalVoice();
      set({ info: await client.getVoiceTranscription(), loading: false });
    } catch (caught) {
      set({ loading: false, error: String(caught) });
    } finally {
      stopped = true;
      await polling;
    }
  },
}));

export function voiceSelectionReady(info: VoiceTranscriptionInfo | null): boolean {
  if (!info) return false;
  if (info.model === "local") return info.local.state === "ready";
  if (info.model === "gpt4o_transcribe") return info.openai_ready;
  return info.gemini_ready;
}
