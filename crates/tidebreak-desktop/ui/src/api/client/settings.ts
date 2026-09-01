import type {
  ChatGptSignInStatus,
  CustomModelConfig,
  EgressConfig,
  ExecConfigInfo,
  ExecCredentialReadiness,
  ExecProviderKind,
  LocalVoiceInfo,
  ModelCatalog,
  ModelRole,
  ModelRoleInfo,
  ModelSelectionKey,
  ModelVisibility,
  PromptCacheRetention,
  ProviderInfo,
  ProviderKind,
  RuntimeSettings,
  VoiceTranscriptionInfo,
  VoiceTranscriptionModel,
  WebSearchConfigInfo,
  WebSearchCredentialReadiness,
  WebSearchMode,
  WebSearchProviderKind,
} from "../types";
import { type Constructor, HttpCore, throwIfNotOk } from "./http";

/** Providers, models, voice, web search, exec, and the settings document. */
export function withSettingsApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    listProviders(): Promise<{ providers: ProviderInfo[] }> {
      return this.json("/providers", { headers: this.headers() });
    }

    putProvider(
      kind: ProviderKind,
      body: {
        enabled?: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
        models?: CustomModelConfig[];
      },
    ): Promise<ProviderInfo> {
      return this.json(`/providers/${kind}`, {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify(body),
      });
    }

    deleteCredential(kind: ProviderKind): Promise<void> {
      return this.json(`/providers/${kind}/credential`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    openaiChatgptSignIn(): Promise<{ authorization_url: string }> {
      return this.json("/providers/openai/chatgpt/sign-in", {
        method: "POST",
        headers: this.headers(),
      });
    }

    openaiChatgptSignOut(): Promise<void> {
      return this.json("/providers/openai/chatgpt/sign-out", {
        method: "POST",
        headers: this.headers(),
      });
    }

    getOpenaiChatgptStatus(): Promise<ChatGptSignInStatus> {
      return this.json("/providers/openai/chatgpt/status", {
        headers: this.headers(),
      });
    }

    getVoiceTranscription(): Promise<VoiceTranscriptionInfo> {
      return this.json("/voice-transcription", { headers: this.headers() });
    }

    putVoiceTranscription(
      model: VoiceTranscriptionModel,
      localModel?: string,
    ): Promise<VoiceTranscriptionInfo> {
      return this.json("/voice-transcription", {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify({ model, local_model: localModel ?? null }),
      });
    }

    installLocalVoice(model: string): Promise<LocalVoiceInfo> {
      return this.json("/voice-transcription/install", {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ model }),
      });
    }

    async transcribeVoice(audio: Blob): Promise<string> {
      const response = await fetch(`${this.baseUrl}/voice-transcription`, {
        method: "POST",
        headers: {
          ...this.headers(),
          "Content-Type": audio.type || "audio/webm",
        },
        body: audio,
      });
      await throwIfNotOk(response);
      return ((await response.json()) as { text: string }).text;
    }

    /**
     * The selectable catalog, plus one row per model role: what the user pinned
     * it to, and what it resolves to right now — the only way a client can name
     * what "default" or "automatic" means for a role.
     */
    listModels(): Promise<ModelCatalog> {
      return this.json("/models", { headers: this.headers() });
    }

    /**
     * Pin a model role to one model, or pass `null` to return it to automatic
     * resolution against the role's ordered defaults.
     */
    putModelRole(
      role: ModelRole,
      selection: ModelSelectionKey | null,
    ): Promise<ModelRoleInfo> {
      return this.json(`/models/roles/${role}`, {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify({ selection }),
      });
    }

    getSettings(): Promise<RuntimeSettings> {
      return this.json("/settings", { headers: this.headers() });
    }

    /**
     * Update runtime settings. A field absent leaves it unchanged, `null` resets
     * it to the server default, and a value sets it (matching the double-option
     * body the server expects).
     *
     * `model_visibility_overrides` is the exception to "absent leaves it
     * unchanged, present merges": the server replaces the map wholesale, so a
     * writer sends the complete set of deviations it wants persisted.
     */
    putSettings(body: {
      model?: ModelSelectionKey | null;
      max_active_background_agents?: number;
      sandbox_agent_checkin_steps?: number;
      sandbox_agent_error_checkin?: number;
      model_visibility_overrides?: Record<string, ModelVisibility>;
      prompt_cache_retention?: PromptCacheRetention;
      compaction?: {
        threshold_fraction?: number;
        target_fraction?: number;
        min_threshold_tokens?: number;
        protect_recent_messages?: number;
      };
      computer_use_enabled?: boolean;
      code_turn_recaps_enabled?: boolean;
      rewrite_closing_messages?: boolean;
      git_source_control?: {
        auto_rename_branches?: boolean;
        branch_prefix_mode?: "account" | "custom" | "none";
        custom_branch_prefix?: string | null;
      };
    }): Promise<RuntimeSettings> {
      return this.json("/settings", {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify(body),
      });
    }

    getWebSearchConfig(): Promise<WebSearchConfigInfo> {
      return this.json("/web-search", { headers: this.headers() });
    }

    putWebSearchConfig(body: {
      mode?: WebSearchMode;
      provider?: WebSearchProviderKind | null;
      timeout_ms?: number;
      // Explicit null clears the configured instance URL; omitting the field
      // leaves it as it is.
      searxng_base_url?: string | null;
    }): Promise<WebSearchConfigInfo> {
      return this.json("/web-search", {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify(body),
      });
    }

    listWebSearchCredentials(): Promise<{
      credentials: WebSearchCredentialReadiness[];
    }> {
      return this.json("/web-search/credentials", { headers: this.headers() });
    }

    putWebSearchCredential(
      provider: WebSearchProviderKind,
      apiKey: string,
    ): Promise<WebSearchCredentialReadiness> {
      return this.json(`/web-search/credentials/${provider}`, {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify({ api_key: apiKey }),
      });
    }

    deleteWebSearchCredential(
      provider: WebSearchProviderKind,
    ): Promise<WebSearchCredentialReadiness> {
      return this.json(`/web-search/credentials/${provider}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    getExecConfig(): Promise<ExecConfigInfo> {
      return this.json("/code-execution", { headers: this.headers() });
    }

    putExecConfig(body: {
      provider?: ExecProviderKind | null;
      timeout_ms?: number;
      egress?: EgressConfig;
    }): Promise<ExecConfigInfo> {
      return this.json("/code-execution", {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify(body),
      });
    }

    listExecCredentials(): Promise<{
      credentials: ExecCredentialReadiness[];
    }> {
      return this.json("/code-execution/credentials", {
        headers: this.headers(),
      });
    }

    putExecCredential(
      provider: ExecProviderKind,
      apiKey: string,
    ): Promise<ExecCredentialReadiness> {
      return this.json(`/code-execution/credentials/${provider}`, {
        method: "PUT",
        headers: this.headers(true),
        body: JSON.stringify({ api_key: apiKey }),
      });
    }

    deleteExecCredential(
      provider: ExecProviderKind,
    ): Promise<ExecCredentialReadiness> {
      return this.json(`/code-execution/credentials/${provider}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }
  };
}
