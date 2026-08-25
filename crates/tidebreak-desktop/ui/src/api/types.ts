import {
  type AppDetail as WireAppDetail,
  type AppGrantState as WireAppGrantState,
  type AppInvokeRefusalKind,
  type AppGatewayPageOutcome as WireAppGatewayPageOutcome,
  type AppGatewayPageResult as WireAppGatewayPageResult,
  type AppLibrary as WireAppLibrary,
  type AppSummary as WireAppSummary,
  type AppViewSession,
  type ApprovalGrantRung,
  type ApprovalClass,
  type AssistantCitationSnapshot,
  type CitationLocator as WireCitationLocator,
  type ChatMessageSnapshot,
  type AgentActivitySnapshot,
  type AgentActivityHistoryItem,
  type AgentRunProgressLine,
  type AgentRunCancellationSnapshot,
  type AgentRunSnapshot,
  type AgentRunTaskPlan as WireAgentRunTaskPlan,
  type Chat as WireChat,
  type ChatTranscript as WireChatTranscript,
  type ExecConfigInfo as WireExecConfigInfo,
  type ConnectedAppInfo as WireConnectedAppInfo,
  type ConnectedAppsInfo as WireConnectedAppsInfo,
  type CredentialPlacement as WireCredentialPlacement,
  type RestCredentialStatus as WireRestCredentialStatus,
  type SpecPreviewInfo as WireSpecPreviewInfo,
  type SpecPreviewOperation as WireSpecPreviewOperation,
  type ExecCredentialReadiness as WireExecCredentialReadiness,
  type ExecProviderKind as WireExecProviderKind,
  type CodeDeliveryActionResult as WireCodeDeliveryActionResult,
  type CodeDeliveryCheck as WireCodeDeliveryCheck,
  type CodeDeliveryDeploymentStatus as WireCodeDeliveryDeploymentStatus,
  type CodeDeliveryPrAttentionReason as WireCodeDeliveryPrAttentionReason,
  type CodeDeliveryPullRequestAction as WireCodeDeliveryPullRequestAction,
  type CodeDeliveryPullRequestActionBody as WireCodeDeliveryPullRequestActionBody,
  type CodeDeliveryPullRequestDetail as WireCodeDeliveryPullRequestDetail,
  type CodeDeliveryPullRequestQuery as WireCodeDeliveryPullRequestQuery,
  type CodeDeliveryPullRequestSummary as WireCodeDeliveryPullRequestSummary,
  type CodeDeliveryPullRequestFile as WireCodeDeliveryPullRequestFile,
  type CodeDeliveryPullRequestTarget as WireCodeDeliveryPullRequestTarget,
  type CodeDeliveryPullRequestsPage as WireCodeDeliveryPullRequestsPage,
  type CodeDeliveryRepositoriesSnapshot as WireCodeDeliveryRepositoriesSnapshot,
  type CodeDeliveryRunAction as WireCodeDeliveryRunAction,
  type CodeDeliveryRunActionBody as WireCodeDeliveryRunActionBody,
  type CodeDeliveryRunAttentionReason as WireCodeDeliveryRunAttentionReason,
  type CodeDeliveryRunDetail as WireCodeDeliveryRunDetail,
  type CodeDeliveryRunKind as WireCodeDeliveryRunKind,
  type CodeDeliveryRunQuery as WireCodeDeliveryRunQuery,
  type CodeDeliveryRunSummary as WireCodeDeliveryRunSummary,
  type CodeDeliveryRunTarget as WireCodeDeliveryRunTarget,
  type CodeDeliveryRunsPage as WireCodeDeliveryRunsPage,
  type CodeDeliverySourceError as WireCodeDeliverySourceError,
  type CodeDeliveryWorkflowJob as WireCodeDeliveryWorkflowJob,
  type CodeDeliveryWorkspaceLink as WireCodeDeliveryWorkspaceLink,
  type CodeGitHubCapability as WireCodeGitHubCapability,
  type CodeGitHubRepositoryRef as WireCodeGitHubRepositoryRef,
  type CodeGitHubRepositoryTarget as WireCodeGitHubRepositoryTarget,
  type CodePullRequestRelation as WireCodePullRequestRelation,
  type CodeWorkspacePullRequestFact as WireCodeWorkspacePullRequestFact,
  type CodeWorkspacePullRequests as WireCodeWorkspacePullRequests,
  type EgressConfig as WireEgressConfig,
  type CustomModelConfig as WireCustomModelConfig,
  type McpHealth as WireMcpHealth,
  type McpCuration as WireMcpCuration,
  type McpServerDefinition as WireMcpServerDefinition,
  type McpViewSession,
  type GatewayApps as WireGatewayApps,
  type GatewayAppInfo as WireGatewayAppInfo,
  type GatewayStatus as WireGatewayStatus,
  type SignInProgress,
  type StandingGrantSnapshot,
  type ConsentStatementSnapshot,
  type GrantLevel,
  type GrantScope,
  type InboxItemKind,
  type McpServerInfo as WireMcpServerInfo,
  type ManagedPolicy as WireManagedPolicy,
  type ManagedPolicySource as WireManagedPolicySource,
  type McpServersInfo as WireMcpServersInfo,
  type ModelInfo as WireModelInfo,
  type ModelRole as WireModelRole,
  type ModelVisibility as WireModelVisibility,
  type QueuedTurn as WireQueuedTurn,
  type ModelRoleInfo as WireModelRoleInfo,
  type Project as WireProject,
  type ProviderInfo as WireProviderInfo,
  type ProviderAuthMode as WireProviderAuthMode,
  type ProviderKind as WireProviderKind,
  type ChatGptSignInStatus as WireChatGptSignInStatus,
  type PermissionMode as WirePermissionMode,
  type PluginCapability as WirePluginCapability,
  type PluginCatalog as WirePluginCatalog,
  type PluginCategory as WirePluginCategory,
  type PluginEnableUpdate as WirePluginEnableUpdate,
  type PluginInfo as WirePluginInfo,
  type PluginPromptInfo as WirePluginPromptInfo,
  type PluginSkillInfo as WirePluginSkillInfo,
  type PromptBody as WirePromptBody,
  type SkillOrigin as WireSkillOrigin,
  type SkillInstructions as WireSkillInstructions,
  type NetworkPolicy as WireNetworkPolicy,
  type ReasoningEffort as WireReasoningEffort,
  type CompactionRun,
  type CompactionSettings,
  type Settings,
  type StickyChatDefaults as WireStickyChatDefaults,
  type WebSearchConfigInfo as WireWebSearchConfigInfo,
  type WebSearchCredentialReadiness as WireWebSearchCredentialReadiness,
  type WebSearchMode as WireWebSearchMode,
  type WebSearchProviderKind as WireWebSearchProviderKind,
  type UserQuestionType as WireUserQuestionType,
  type ChatTerminalTurnSnapshot,
  type ChatToolActivitySnapshot,
  type ChatToolActivityStatus,
  type ExecBackend,
  type ExecDegradation,
  type ExecFileChangeSummary as WireExecFileChangeSummary,
  type RendererAgentEvent,
  type RendererChatFrame,
  type RendererChatMetadata,
  type RendererRefusal,
  type RendererSequencedEvent,
  type RendererToolName,
  type ResultEntryKind,
  type TaskPlan as WireTaskPlan,
  type TaskPlanStep as WireTaskPlanStep,
  type TaskPlanStepStatus as WireTaskPlanStepStatus,
  type ToolActionPreview,
  type TranscriptImageAttachment as WireTranscriptImageAttachment,
  type TranscriptRole,
  type ToolApprovalKind,
  type Attention as WireAttention,
  type AttentionSource as WireAttentionSource,
  type AttentionState as WireAttentionState,
  type CapLevel as WireCapLevel,
  type CodeAnalyticsDay as WireCodeAnalyticsDay,
  type CodeAnalyticsHarness as WireCodeAnalyticsHarness,
  type CodeAnalyticsModel as WireCodeAnalyticsModel,
  type CodeAnalyticsPricingCoverage as WireCodeAnalyticsPricingCoverage,
  type CodeAnalyticsRange as WireCodeAnalyticsRange,
  type CodeAnalyticsRepository as WireCodeAnalyticsRepository,
  type CodeAnalyticsSnapshot as WireCodeAnalyticsSnapshot,
  type CodeAnalyticsTotals as WireCodeAnalyticsTotals,
  type CodeApprovalSnapshot as WireCodeApprovalSnapshot,
  type CodeApprovalState as WireCodeApprovalState,
  type CodeEvent as WireCodeEvent,
  type CodeRepoSnapshot as WireCodeRepoSnapshot,
  type CodeSessionId as WireCodeSessionId,
  type CodeSessionKind as WireCodeSessionKind,
  type CodeSessionLifecycle as WireCodeSessionLifecycle,
  type CodeSessionActivity as WireCodeSessionActivity,
  type CodeSessionSnapshot as WireCodeSessionSnapshot,
  type CodeSubagentStatus as WireCodeSubagentStatus,
  type CodeSubagentSummary as WireCodeSubagentSummary,
  type CodeTurnId as WireCodeTurnId,
  type CodeTurnSnapshot as WireCodeTurnSnapshot,
  type QueuedCodeTurn as WireQueuedCodeTurn,
  type CodeTurnStatus as WireCodeTurnStatus,
  type CodeUsage as WireCodeUsage,
  type CodeWorkspaceDiff as WireCodeWorkspaceDiff,
  type CodeWorkspaceFiles as WireCodeWorkspaceFiles,
  type CodeWorkspaceHistorySearchMatch as WireCodeWorkspaceHistorySearchMatch,
  type CodeWorkspaceHistorySearchSource as WireCodeWorkspaceHistorySearchSource,
  type CodeWorkspaceSearch as WireCodeWorkspaceSearch,
  type CodeWorkspaceSearchMatch as WireCodeWorkspaceSearchMatch,
  type CodeWorkspaceTree as WireCodeWorkspaceTree,
  type CodeWorkspaceBlob as WireCodeWorkspaceBlob,
  type CodeWorkspacePrSnapshot as WireCodeWorkspacePrSnapshot,
  type CodeTriggerAction as WireCodeTriggerAction,
  type CodeTriggerCondition as WireCodeTriggerCondition,
  type CodeTriggerSnapshot as WireCodeTriggerSnapshot,
  type CodeWatchSnapshot as WireCodeWatchSnapshot,
  type CodeWatchState as WireCodeWatchState,
  type CodeActionSnapshot as WireCodeActionSnapshot,
  type CodeCommitSnapshot as WireCodeCommitSnapshot,
  type CodePushSnapshot as WireCodePushSnapshot,
  type CodeFileChange as WireCodeFileChange,
  type CodeSessionDigest as WireCodeSessionDigest,
  type CodeUpdateNotice as WireCodeUpdateNotice,
  type CodeCloneDefaults as WireCodeCloneDefaults,
  type CodeRepoSource as WireCodeRepoSource,
  type CodeRepoSources as WireCodeRepoSources,
  type CodeGithubRepositories as WireCodeGithubRepositories,
  type CodeGithubRepository as WireCodeGithubRepository,
  type CodeCloneJobSnapshot as WireCodeCloneJobSnapshot,
  type CodeHarnessInstallSnapshot as WireCodeHarnessInstallSnapshot,
  type CodeWorktreeRoot as WireCodeWorktreeRoot,
  type CodeForkTranscript as WireCodeForkTranscript,
  type CodeCheckLog as WireCodeCheckLog,
  type CodeCheckLogError as WireCodeCheckLogError,
  type CodeCheckLogsSnapshot as WireCodeCheckLogsSnapshot,
  type CodeTerminalActivityNotice as WireCodeTerminalActivityNotice,
  type PullRequestDigest as WirePullRequestDigest,
  type PullRequestCheck as WirePullRequestCheck,
  type PullRequestCheckBucket as WirePullRequestCheckBucket,
  type PullRequestComment as WirePullRequestComment,
  type PullRequestCommentKind as WirePullRequestCommentKind,
  type CodePrCommentsSnapshot as WireCodePrCommentsSnapshot,
  type CodePrMergeMethod as WireCodePrMergeMethod,
  type MergeCodePrBody as WireMergeCodePrBody,
  type CodeTerminalRead as WireCodeTerminalRead,
  type CodeTerminalSnapshot as WireCodeTerminalSnapshot,
  type CodeWorkspaceSnapshot as WireCodeWorkspaceSnapshot,
  type Diffstat as WireDiffstat,
  type FileChangeKind as WireFileChangeKind,
  type CodeWorkspaceStatus as WireCodeWorkspaceStatus,
  type FenceReason as WireFenceReason,
  type HarnessCaps as WireHarnessCaps,
  type HarnessDoctorEntry as WireHarnessDoctorEntry,
  type HarnessDoctorReport as WireHarnessDoctorReport,
  type HarnessKind as WireHarnessKind,
  type HarnessNoticeLevel as WireHarnessNoticeLevel,
  type HarnessTier as WireHarnessTier,
  type RepoId as WireRepoId,
  type SequencedCodeEventFrame as WireSequencedCodeEventFrame,
  type ToolDetail as WireToolDetail,
  type ToolOutcome as WireToolOutcome,
  type WorkspaceId as WireWorkspaceId,
} from "../generated/wire";

export type {
  ApprovalClass,
  ApprovalGrantRung,
  InboxItemKind,
  GrantLevel,
  GrantScope,
  StandingGrantSnapshot,
  ConsentStatementSnapshot,
  ChatToolActivityStatus,
  TranscriptRole,
  RendererToolName,
  ResultEntryKind,
  ToolActionPreview,
  RendererRefusal,
  ExecBackend,
  ExecDegradation,
};

/**
 * The WebSocket frame and the events it carries, generated from the server's
 * renderer projection.
 *
 * Named for the renderer on the Rust side; the shorter names are what the app
 * has always used, so they are aliased rather than renamed everywhere.
 */
export type SequencedEvent = RendererSequencedEvent;
export type AgentEvent = RendererAgentEvent;

/**
 * Anything the chat socket can deliver: a journaled event at its sequence, or
 * chat metadata that changed outside the journal.
 */
export type ChatFrame = RendererChatFrame;

/** A metadata frame — today only a title the server derived for this chat. */
export type ChatMetadataFrame = RendererChatMetadata;

/** Generated from `ToolApprovalKind`. */
export type RendererApprovalKind = ToolApprovalKind;

/**
 * One source as the renderer may see it, paired with `ChatDocumentDetail` on the
 * server. Deliberately narrower than the catalog summary, which also carries the
 * source's `uri` and index bookkeeping — neither of which the renderer is given,
 * because a `uri` can name a place on the reader's disk.
 */
export type DocumentDetail = {
  document_id: string;
  media_type: string;
  title: string | null;
  readable: boolean;
  /**
   * Whether the source kept the bytes it was made from. A source with none —
   * a fetched web page, whose markup is not retained — has no original to draw,
   * so the panel shows the extracted text rather than a viewer that can only
   * fail.
   */
  has_original_bytes: boolean;
  updated_at: string;
  content: string;
};

/**
 * Which machine this client is attached to.
 *
 * `local` is the server the desktop app booted in its own process; `remote` is
 * a server elsewhere that the user attached to with a URL and a token. Host
 * authority — the folder broker, the client executor, native export, computer
 * use — exists only on the local machine, so every caller that reaches the host
 * branches on this rather than on the shape of the base URL.
 */
export type Attachment = "local" | "remote";

/** Connection details from the Tauri host (`server_info` command). */
export type ServerInfo = {
  baseUrl: string;
  token: string;
  attachment: Attachment;
  gatewayAuth: boolean;
};

/** What the shell knows about the current attachment. */
export type RemoteMachineState = {
  attachment: Attachment;
  /** The attached machine's base URL; absent when attached locally. */
  baseUrl: string | null;
};

/**
 * A refused connect attempt.
 *
 * `reason` is the whole contract — one stable string per distinct cause,
 * following the precedent `output_writeback_authority_unavailable` set. The
 * renderer owns the copy and switches on the reason; `detail` is the underlying
 * transport or credential-store text, for logs and support, never for display.
 */
export type RemoteConnectError = {
  reason: RemoteConnectReason;
  detail: string | null;
};

export type RemoteConnectReason =
  | "remote_machine_url_invalid"
  | "remote_machine_requires_tls"
  | "remote_machine_unreachable"
  | "remote_machine_token_refused"
  | "remote_machine_not_a_machine"
  | "remote_machine_token_storage_failed"
  | "remote_machine_gateway_auth_unavailable";

export type ProviderKind = WireProviderKind;
/** Stable provider-scoped key used for new settings and chat overrides. */
export type ModelSelectionKey = `${ProviderKind}::${string}`;

/** A reader's deviation from a model's curated `recommended` default. */
export type ModelVisibility = WireModelVisibility;

/** One message waiting to run as its own turn once the chat is free. */
export type QueuedTurn = WireQueuedTurn;

/** How hard a reasoning-capable model should think before answering. */
export type ReasoningEffort = WireReasoningEffort;

/** How much a chat lets the agent do between approvals. */
export type PermissionMode = WirePermissionMode;

/** Network access granted to code execution in one conversation workspace. */
export type NetworkPolicy = WireNetworkPolicy;

/** The sticky new-chat defaults an unspecified `POST /chats` field seeds. */
export type StickyChatDefaults = WireStickyChatDefaults;

export type ProviderInfo = WireProviderInfo;
export type ProviderAuthMode = WireProviderAuthMode;
export type ChatGptSignInStatus = WireChatGptSignInStatus;

export type VoiceTranscriptionModel =
  | "local"
  | "gpt4o_transcribe"
  | "gemini_flash";
export type LocalVoiceState =
  | "not_installed"
  | "downloading"
  | "ready"
  | "failed"
  | "unavailable";
export type LocalVoiceInfo = {
  state: LocalVoiceState;
  downloaded_bytes: number | null;
  total_bytes: number | null;
  error: string | null;
};
/** One entry of the local speech catalog, with its install state on this device. */
export type LocalVoiceModelInfo = {
  id: string;
  label: string;
  description: string;
  total_bytes: number;
  english_only: boolean;
  recommended: boolean;
  state: LocalVoiceState;
  downloaded_bytes: number | null;
  error: string | null;
};
export type VoiceTranscriptionInfo = {
  model: VoiceTranscriptionModel;
  local_model: string;
  local_models: LocalVoiceModelInfo[];
  openai_ready: boolean;
  gemini_ready: boolean;
};

export type CustomModelConfig = WireCustomModelConfig;

/**
 * A model the picker may offer.
 *
 * `key` is re-branded rather than taken from the wire: the server sends a plain
 * string, and `ModelSelectionKey` is the refinement the app relies on to keep a
 * provider-qualified key distinct from a bare model id. Written out with `Omit`
 * so the one divergence from the generated type is visible.
 */
export type ModelInfo = Omit<WireModelInfo, "key"> & {
  key: ModelSelectionKey;
};

export type ModelRole = WireModelRole;

/** A named model role, its pinned selection, and what it resolves to now. */
export type ModelRoleInfo = WireModelRoleInfo;

/** The catalog plus what each role resolves to (`GET /models`). */
export type ModelCatalog = {
  models: ModelInfo[];
  roles: ModelRoleInfo[];
};

/** Global runtime settings (`GET/PUT /settings`). */
export type RuntimeSettings = Settings;

/** Host-global compaction cadence, as `GET/PUT /settings` carries it. */
export type { CompactionSettings };

/** What one on-demand compaction did (`POST /chats/{id}/compact`). */
export type { CompactionRun };

export type Project = WireProject;

/**
 * One file a project shares with the conversations filed under it.
 *
 * Narrower than the catalog summary the route serves: the renderer is given
 * neither the source `uri`, which can name a place on the reader's disk, nor
 * the ownership columns it already knows from the route it asked.
 */
export type ProjectDocument = {
  document_id: string;
  media_type: string;
  title: string | null;
  source_byte_len: number | null;
  readable: boolean;
  created_at: string;
  updated_at: string;
};

/** One page of a project's files, newest first. */
export type ProjectDocumentPage = {
  documents: ProjectDocument[];
  next_cursor: string | null;
};

/** The fixed, host-owned search providers supported by this build. */
export type WebSearchProviderKind = WireWebSearchProviderKind;

/** Which search a chat gets: the model provider's, this host's, or none. */
export type WebSearchMode = WireWebSearchMode;

/** Non-secret web-search policy and readiness for its selected provider. */
export type WebSearchConfigInfo = WireWebSearchConfigInfo;

/** Readiness only: the API never returns an existing provider key. */
export type WebSearchCredentialReadiness = WireWebSearchCredentialReadiness;

/** The fixed, host-owned code-execution providers supported by this build. */
export type ExecProviderKind = WireExecProviderKind;

/** Non-secret code-execution selection, timeout policy, and host readiness. */
export type ExecConfigInfo = WireExecConfigInfo;

/** Readiness only: the API never returns a saved managed-provider key. */
export type ExecCredentialReadiness = WireExecCredentialReadiness;

/**
 * Host-owned egress policy for the managed sandboxes. Never a secret: only
 * domain patterns and CIDR blocks, or `open` for today's unrestricted egress.
 */
export type EgressConfig = WireEgressConfig;

export type McpHealth = WireMcpHealth;

/** Typed stdio process data. Values are argv entries, never shell source. */
export type McpServerDefinition = WireMcpServerDefinition;

/**
 * Renderer-safe health projection. No environment value of any kind is sent:
 * `env` and `env_from` carry names, and the values behind them live in the
 * host process or the OS credential store.
 */
export type McpServerInfo = WireMcpServerInfo;

export type McpServersInfo = WireMcpServersInfo;

/**
 * The curated-list entry a configured MCP server matched. Present means
 * Tidebreak has driven the server end to end; absent is the community tier.
 */
export type McpCuration = WireMcpCuration;

/** The Connected apps listing: per-kind projections, both kinds. */
export type ConnectedAppsInfo = WireConnectedAppsInfo;
export type ConnectedAppInfo = WireConnectedAppInfo;
export type RestCredentialStatus = WireRestCredentialStatus;
export type SpecPreviewInfo = WireSpecPreviewInfo;
export type SpecPreviewOperation = WireSpecPreviewOperation;
/** Where a stored REST credential is injected: `"bearer"` or a named header. */
export type CredentialPlacement = WireCredentialPlacement;

/**
 * What a REST connected-app upsert does about the credential: clear it, keep
 * the stored one unchanged, or store a new value. The value travels only in
 * this request body and is never read back by any route.
 */
export type RestCredentialUpdate =
  | "none"
  | "keep"
  | { set: { value: string; placement: CredentialPlacement } };

/** The resolved managed-mode policy; read-only for the renderer. */
export type ManagedPolicy = WireManagedPolicy;
export type ManagedPolicySource = WireManagedPolicySource;

export type Chat = WireChat;

/**
 * One visible, durable transcript entry in conversation order.
 *
 * Generated apart from `citations`, which is deliberately wider than the wire:
 * the server always serializes it, as `[]` when empty. It is optional here
 * because the transcript is not run through a validator — it arrives as a parsed
 * cast — so the `?` is what makes the compiler demand a guard at the one place
 * that reads it. Narrowing it to match the wire would delete that guard rather
 * than earn it, so the override is spelled out instead of hidden.
 */
export type ChatMessage = Omit<
  ChatMessageSnapshot,
  "citations" | "image_attachments"
> & {
  citations?: ChatMessageCitation[];
  /** Omitted by the server when this message has no images. */
  image_attachments?: ChatMessageImageAttachment[];
};

/**
 * A bounded, renderer-safe evidence snapshot owned by one assistant message.
 *
 * Ownership is positional: the server nests each citation under its message and
 * deliberately skips `message_id` on the wire. Do not reintroduce it.
 */
export type ChatMessageCitation = AssistantCitationSnapshot;
export type CitationLocator = WireCitationLocator;

/** One durable image identity in a historical user message. */
export type ChatMessageImageAttachment = WireTranscriptImageAttachment;

/**
 * A fixed, terminal tool-card projection with no canonical tool data.
 *
 * `tool` is the allowlisted renderer name rather than display copy: the renderer
 * derives a live call's wording from its name, and carrying prose here meant a
 * second copy of it plus an inverse lookup, where a change on either side
 * silently broke hydration.
 */
export type ChatToolActivity = ChatToolActivitySnapshot;

/** One terminal turn's status and renderer-safe streamed presentation. */
export type ChatTerminalTurn = ChatTerminalTurnSnapshot;
export type ExecFileChangeSummary = WireExecFileChangeSummary;

export type ExecFileUndoStatus =
  | "restored"
  | "deleted"
  | "already_undone"
  | "stale"
  | "not_available"
  | "snapshot_missing"
  | "unavailable";

export type ExecFileUndoOutcome = {
  snapshot_id: string;
  folder_name: string;
  relative_path: string;
  status: ExecFileUndoStatus;
};

export type ChatTranscript = WireChatTranscript;

/** A durable foreground coordinator or sandboxed background run. */
/** A durable foreground coordinator or sandboxed background run. */
export type AgentRun = AgentRunSnapshot;

/**
 * The checklist the agent keeps for a long turn: replaced whole on every
 * update, and the only place the steps themselves are carried — the event
 * stream only says the plan moved on.
 */
export type TaskPlan = WireTaskPlan;

/**
 * The run-scoped twin of {@link TaskPlan}: one background run's checklist,
 * carrying no turn because a run is one delegated task start to finish.
 */
export type AgentRunTaskPlan = WireAgentRunTaskPlan;
export type TaskPlanStep = WireTaskPlanStep;
export type TaskPlanStepStatus = WireTaskPlanStepStatus;

/** The complete renderer projection returned by one sandbox stop request. */
/** The complete renderer projection returned by one sandbox stop request. */
export type SandboxAgentCancellation = AgentRunCancellationSnapshot;

/**
 * Live agent activity is intentionally a closed vocabulary. The server never
 * sends tool inputs, results, host paths, grants, executor identities, leases,
 * provider identities, or diagnostics.
 */
/**
 * Live agent activity is intentionally a closed vocabulary. The server never
 * sends tool inputs, results, host paths, grants, executor identities, leases,
 * provider identities, or diagnostics.
 */
export type AgentActivity = AgentActivitySnapshot;

/**
 * One settled or live step in a background run's ordered activity history.
 *
 * The wire may also carry an additive typed `detail`, kept only when
 * {@link parseAgentActivityHistory} can validate it as a bounded headline whose
 * tag matches the entry's own kind. An entry whose detail fails that check is
 * still rendered, without the headline.
 */
export type AgentActivityHistoryEntry = AgentActivityHistoryItem;

/**
 * One line of live progress a background run published.
 *
 * The text is the run's own bounded narration, the same class of prose the
 * terminal result carries. `sequence` is the resume cursor: order is the
 * contract, and gaps are possible once the server's retention trims the
 * oldest lines.
 */
export type AgentRunProgressEntry = AgentRunProgressLine;

/** A page of live progress plus the cursor to resume from. */
export type AgentRunProgress = {
  entries: AgentRunProgressEntry[];
  nextSequence: number;
};

/**
 * What a call produced.
 *
 * The other exception. A command's output is the whole reason to run it;
 * withholding it leaves the transcript asserting that something happened
 * without ever showing what.
 *
 * Hand-written and camelCase, unlike its snake_case wire form. The Rust type is
 * carried in the journal, so renaming its fields would stop existing chats from
 * loading — the remap in `parseToolResultPreview` is the cheaper side to keep.
 * That remap reads the generated wire type, so a field renamed in Rust breaks
 * there at compile time rather than silently producing `undefined`.
 */
export type ToolResultPreview =
  | {
      tool: "exec";
      exitCode: number | null;
      timedOut: boolean;
      outputTruncated: boolean;
      stdout: string;
      stderr: string;
      images?: {
        attachmentId: string;
        mediaType: string;
        width: number;
        height: number;
      }[];
      /** Durable outputs the command's output/ files created or updated. */
      outputs?: ResultEntry[];
      /**
       * How the execution backend fell short of its intended setup, when it
       * did. Sent on the first command that degrades and not on the ones
       * after it, so a conversation says this once.
       */
      degraded?: ExecDegradation;
      /** Which backend ran the command, when the server said. */
      backend?: ExecBackend;
    }
  | {
      /** Web search is available after the reader configures a provider. */
      tool: "web_search_provider_required";
    }
  | {
      /** A reference to an MCP Apps view — never markup. The document is
       * fetched separately and rendered only inside the sandboxed frame. */
      tool: "mcp_app";
      server: string;
      resourceUri: string;
    }
  | {
      /** What a call found, read, or wrote, as the list of things it was. */
      tool: "entries";
      entries: ResultEntry[];
      /** What the same call could not do. */
      failures: ResultFailure[];
      /** Rows the server bounded away, counted rather than shown. */
      elided: number;
    }
  | {
      /** The answers a parked question call was resolved with. */
      tool: "user_questions";
      answers: AnsweredUserQuestion[];
      /** Whatever the reader added on their own, when they added any. */
      additionalContext: string | null;
    }
  | {
      /** The decision a parked plan proposal was resolved with. */
      tool: "plan_decision";
      title: string;
      plan: string;
      accepted: boolean;
      /** What the reader asked to change, when they sent it back. */
      feedback: string | null;
    }
  | {
      /** A computer-use screen capture: the image plus how many controls were
       * marked on it. The pixels live in the blob store, referenced here. */
      tool: "screen_capture";
      image: {
        attachmentId: string;
        mediaType: string;
        width: number;
        height: number;
      };
      markCount: number;
    };

/**
 * One question as it was asked, with what the reader chose.
 *
 * A question with neither a selection nor an answer was skipped, which the
 * recap says out loud rather than omitting the row.
 */
export type AnsweredUserQuestion = {
  question: string;
  /** Option labels, in the order the question listed them. */
  selected: string[];
  customAnswer: string | null;
};

/** One row of a listed result. */
export type ResultEntry = {
  kind: ResultEntryKind;
  label: string;
  detail: string | null;
  meta: string | null;
  /** The document's media type, when the row is a document with one. */
  mediaType: string | null;
  /**
   * The durable record this row opens, when its kind names somewhere to go —
   * an output row the outputs panel, an app row the apps library.
   */
  targetId: string | null;
  /**
   * The public page this row opens, when it names one. Re-checked here rather
   * than trusted: it is the only projected field that can send a reader out of
   * the application, so a non-web address never reaches the host's opener.
   */
  url: string | null;
};

/** One thing a listed call could not do. */
export type ResultFailure = {
  /** What failed, when the tool could name it. */
  label: string | null;
  /** Why, in the tool's own words. */
  error: string;
};

/** The entries-shaped result the list card renders. */
export type EntriesResultPreview = Extract<
  ToolResultPreview,
  { tool: "entries" }
>;

/** The exec-shaped result the command card renders. */
export type ExecResultPreview = Extract<ToolResultPreview, { tool: "exec" }>;

/** A single-use frame address for one MCP Apps view. */
export type McpViewSessionInfo = McpViewSession;
export type { McpViewSession };

/** Progress of the one in-flight gateway browser sign-in. */
export type GatewaySignInProgress = SignInProgress;

/** Renderer-safe model-gateway connection state; never token material. */
export type GatewayStatus = WireGatewayStatus;

/** The signed-in user's entitled connected apps, fetched live per request. */
export type GatewayApps = WireGatewayApps;

/** One entitled connected app and the MCP endpoint slugs that carry it. */
export type GatewayAppInfo = WireGatewayAppInfo;

/**
 * Opaque envelope for a sandboxed MCP App view; never rendered directly.
 *
 * Hand-written on purpose: the payload's JSON fields are untyped passthrough
 * for the view, which the wire-type generator's precision guard refuses.
 */
export type McpAppPayload = {
  arguments?: unknown;
  content: string;
  structured_content?: unknown;
  is_error: boolean;
};

/** Everything installed, bundle by bundle, in the enable state it is in. */
export type PluginCatalog = WirePluginCatalog;

/** One bundle: its identity, its host-derived badges, and its members. */
export type PluginInfo = WirePluginInfo;

/** One skill, inside a bundle or standing alone. */
export type PluginSkillInfo = WirePluginSkillInfo;

/** One reusable prompt, bundled or standalone, as the catalog lists it. */
export type PluginPromptInfo = WirePluginPromptInfo;

/** One prompt's insertable text, fetched when the user picks it. */
export type PromptBody = WirePromptBody;

/** What a bundle can do, derived by the host from what it contains. */
export type PluginCapability = WirePluginCapability;

/** What kind of work a bundle is for. */
export type PluginCategory = WirePluginCategory;

/** A merge patch over enable flags: absent names are left alone. */
export type PluginEnableUpdate = WirePluginEnableUpdate;

/** Where a skill package was loaded from; host-derived, never claimed. */
export type SkillOrigin = WireSkillOrigin;

/** One skill's instruction body, fetched on demand for its detail view. */
export type SkillInstructions = WireSkillInstructions;

/** The Apps library listing: every live local app, newest activity first. */
export type AppLibrary = WireAppLibrary;

/** One Apps library row. */
export type AppSummary = WireAppSummary;

/** One app's detail: summary fields plus its revision history. */
export type AppDetail = WireAppDetail;

/** Renderer-safe grant state for one app: the consent sheet's whole input. */
export type AppGrantState = WireAppGrantState;

/** Where an app's page lives at the gateway, with the gateway's own words. */
export type AppGatewayPageResult = WireAppGatewayPageResult;

/** The closed set of answers the app page branches on. */
export type AppGatewayPageOutcome = WireAppGatewayPageOutcome;

/** A single-use frame address for one stored app revision. */
export type AppViewSessionInfo = AppViewSession;
export type { AppViewSession };

/**
 * Result of a granted REST-operation invoke, forwarded verbatim to the
 * sandboxed frame.
 *
 * Hand-written on purpose, like [`McpAppPayload`] and for the same reason:
 * the response is opaque passthrough the renderer never interprets, which
 * the wire-type generator's precision guard refuses.
 *
 * An executed operation is opaque passthrough: whatever HTTP status the API
 * answered (4xx/5xx included) with `is_error: false` and the raw response
 * body base64-encoded in `body_base64` so binary responses survive JSON. A
 * refused or failed execution is `is_error: true` with the server's closed
 * refusal text in `error` and no response fields.
 */
export type AppRestInvokeResult = {
  status?: number;
  content_type?: string;
  body_base64?: string;
  is_error: boolean;
  error?: string;
};

/**
 * Result of a granted folder invoke — the `folder` sibling of the other two,
 * hand-written for the same reason. Exactly one payload half is present per
 * operation: `entries` for a list, `content_base64` for a read, `replaced`
 * for a write; failures are `is_error: true` with the host's closed failure
 * text in `error`.
 */
export type AppFolderInvokeResult = {
  entries?: Array<{ name: string; directory: boolean }>;
  content_base64?: string;
  replaced?: boolean;
  is_error: boolean;
  error?: string;
};

export type { AppInvokeRefusalKind };

export const APP_INVOKE_REFUSAL_KINDS: readonly AppInvokeRefusalKind[] = [
  "app_not_found",
  "not_pinned",
  "consent_required",
  "unknown_tool",
  "gateway_unavailable",
  "gateway_authorization_required",
];

/**
 * A typed refusal from `POST /apps/{id}/invoke`: the one invoke failure the
 * renderer branches on (`consent_required` re-opens the grant sheet) rather
 * than merely reporting.
 */
export class AppInvokeRefusalError extends Error {
  constructor(
    readonly kind: AppInvokeRefusalKind,
    message: string,
  ) {
    super(message);
    this.name = "AppInvokeRefusalError";
  }
}

/**
 * Approval kinds a human may approve from the renderer.
 *
 * A total map over the generated union, not a list of the approvable ones: the
 * server decides this and the renderer cross-checks its answer, so a kind added
 * server-side has to be classified here deliberately rather than defaulting to
 * "not approvable" and failing every snapshot that carries it.
 */
const APPROVABLE_KINDS = {
  search_may_share_query_and_excerpts: true,
  web_search_may_share_query: true,
  web_extract_may_fetch_url: true,
  exec_may_run_networked_command: true,
  external_mcp_may_call_server: true,
  workspace_may_modify_files: true,
  delegate_may_run_background_agent: true,
  // Approvable once per app; the durable consent is the broker's per-app grant,
  // not a standing grant here.
  computer_may_control_app: true,
  unsupported: false,
} as const satisfies Record<RendererApprovalKind, boolean>;

export function isApprovableKind(kind: RendererApprovalKind): boolean {
  return APPROVABLE_KINDS[kind];
}

/** Approval kinds whose authority is stable enough to remember by tool name. */
export function isRememberableKind(kind: RendererApprovalKind): boolean {
  // Computer-use control is excluded alongside external MCP: its durable
  // consent is the host broker's per-app grant, so a name-keyed renderer grant
  // would be a second, drifting spelling of it.
  return (
    isApprovableKind(kind) &&
    kind !== "external_mcp_may_call_server" &&
    kind !== "computer_may_control_app"
  );
}

/** A strict renderer-safe snapshot used to recover a parked approval. */
export type PendingToolApproval = {
  callId: string;
  turnId: string;
  action: RendererToolName;
  approval: RendererApprovalKind;
  class: "read_only" | "workspace" | "sensitive";
  /** What the parked call will do, when its tool projects a preview. */
  preview: ToolActionPreview | null;
  canApprove: boolean;
  canRemember: boolean;
  /** Complete standing-grant ladder the server will honor for this call. */
  grantRungs: ApprovalGrantRung[];
  /** Where the Auto-mode judge stands, or null when no judge was engaged. */
  autoJudgeStatus: "judging" | "approved" | "declined" | null;
};

export type FolderAccessHint = "documents" | "downloads";

/** A validated, pending request that the renderer may safely present. */
export type PendingFolderAccessRequest = {
  callId: string;
  turnId: string;
  reason: string;
  folderHint: FolderAccessHint | null;
  claimedByDesktop: boolean;
};

/** How a write-back means to land in the connected folder. */
export type OutputWriteMode = "create" | "replace";

/** Renderer-safe write-back prompt. Canonical output, root, and path stay native. */
export type PendingOutputWritebackRequest = {
  callId: string;
  turnId: string;
  mode: OutputWriteMode;
  claimedByDesktop: boolean;
};

/**
 * One thing waiting on the reader, wherever it parked.
 *
 * Deliberately as opaque as the per-chat attention summary: enough to triage
 * and to navigate back, never the question, the plan, or the arguments. Those
 * are read from the conversation the item points at, by the card that owns
 * them — which is also the only place the item can be answered.
 */
/**
 * Which conversation an inbox entry belongs to.
 *
 * Tagged because chat and code still have separate id spaces. Decision 48
 * step 5 collapses this to one id when the entities merge; until then the tag
 * is the only place either surface's shape shows through.
 */
export type InboxConversation =
  | { surface: "chat"; chatId: string }
  | { surface: "code"; sessionId: string; workspaceId: string };

/** One parked call behind an entry's attention. */
export type InboxItem = {
  turnId: string;
  callId: string;
  kind: InboxItemKind;
  action: RendererToolName | null;
  requestedAt: string;
};

/** One conversation waiting on the reader, and why. */
export type InboxEntry = {
  conversation: InboxConversation;
  title: string | null;
  attention: Attention;
  /** Empty for a code conversation, whose approvals have their own route. */
  items: InboxItem[];
  waitingSince: string;
};

/** A stable key for an entry, whichever surface it lives on. */
export function inboxConversationKey(conversation: InboxConversation): string {
  return conversation.surface === "chat"
    ? `chat:${conversation.chatId}`
    : `code:${conversation.sessionId}`;
}

/** Opaque prompt state used to mark another chat as needing attention. */
export type PendingChatPrompt = {
  chatId: string;
  questionCallIds: string[];
  planCallIds: string[];
  folderAccessCallIds: string[];
  outputWritebackCallIds: string[];
};

export type UserQuestionOption = {
  id: string;
  label: string;
  description: string;
};

export type UserQuestion = {
  id: string;
  header: string;
  question: string;
  options: UserQuestionOption[];
  questionType: WireUserQuestionType;
  allowFreeForm: boolean;
};

/** Closed renderer projection of one durable question continuation. */
export type PendingUserQuestions = {
  callId: string;
  turnId: string;
  questions: UserQuestion[];
  askedAt: string;
};

export type UserQuestionAnswer = {
  questionId: string;
  selectedOptionIds: string[];
  customAnswer?: string;
};

/** Closed renderer projection of one durable plan continuation. */
export type PendingPlanApproval = {
  callId: string;
  turnId: string;
  title: string;
  plan: string;
  proposedAt: string;
};

export type PlanDecision =
  | { decision: "accept" }
  | { decision: "reject"; feedback?: string };

/**
 * The only consent prose a folder-access prompt will render.
 *
 * `parseFolderAccessRequest` rejects any request whose `reason` is not
 * byte-identical to this, so no server-authored text can reach a consent prompt.
 * The server holds the same literal; exported so a test can compare the two
 * across the language boundary rather than trusting they match.
 */
export const RENDERER_FOLDER_ACCESS_REASON =
  "The assistant needs read access to files outside the folders connected to this conversation.";

/** How far a source download has got, when its length is known. */
export type FileDownloadProgress = {
  loaded: number;
  total: number;
  /** `loaded` over `total`, as 0–100. */
  percentage: number;
};

/** A registered local git repository. */
export type CodeRepoSnapshot = WireCodeRepoSnapshot;
export type RepoId = WireRepoId;

/** Local code activity and cost estimates. */
export type CodeAnalyticsSnapshot = WireCodeAnalyticsSnapshot;
export type CodeAnalyticsRange = WireCodeAnalyticsRange;
export type CodeAnalyticsTotals = WireCodeAnalyticsTotals;
export type CodeAnalyticsDay = WireCodeAnalyticsDay;
export type CodeAnalyticsRepository = WireCodeAnalyticsRepository;
export type CodeAnalyticsModel = WireCodeAnalyticsModel;
export type CodeAnalyticsHarness = WireCodeAnalyticsHarness;
export type CodeAnalyticsPricingCoverage = WireCodeAnalyticsPricingCoverage;

/** GitHub-only install-wide delivery contracts. */
export type CodeGitHubCapability = WireCodeGitHubCapability;
export type CodeGitHubRepositoryRef = WireCodeGitHubRepositoryRef;
export type CodeGitHubRepositoryTarget = WireCodeGitHubRepositoryTarget;
export type CodePullRequestRelation = WireCodePullRequestRelation;
export type CodeWorkspacePullRequestFact = WireCodeWorkspacePullRequestFact;
export type CodeWorkspacePullRequests = WireCodeWorkspacePullRequests;
export type CodeDeliverySourceError = WireCodeDeliverySourceError;
export type CodeDeliveryWorkspaceLink = WireCodeDeliveryWorkspaceLink;
export type CodeDeliveryCheck = WireCodeDeliveryCheck;
export type CodeDeliveryPrAttentionReason = WireCodeDeliveryPrAttentionReason;
export type CodeDeliveryPullRequestSummary = WireCodeDeliveryPullRequestSummary;
export type CodeDeliveryPullRequestDetail = WireCodeDeliveryPullRequestDetail;
export type CodeDeliveryPullRequestQuery = WireCodeDeliveryPullRequestQuery;
export type CodeDeliveryPullRequestsPage = WireCodeDeliveryPullRequestsPage;
export type CodeDeliveryPullRequestTarget = WireCodeDeliveryPullRequestTarget;
export type CodeDeliveryPullRequestFile = WireCodeDeliveryPullRequestFile;
export type CodeDeliveryPullRequestAction = WireCodeDeliveryPullRequestAction;
export type CodeDeliveryPullRequestActionBody =
  WireCodeDeliveryPullRequestActionBody;
export type CodeDeliveryRunKind = WireCodeDeliveryRunKind;
export type CodeDeliveryRunAttentionReason = WireCodeDeliveryRunAttentionReason;
export type CodeDeliveryRunSummary = WireCodeDeliveryRunSummary;
export type CodeDeliveryRunDetail = WireCodeDeliveryRunDetail;
export type CodeDeliveryRunQuery = WireCodeDeliveryRunQuery;
export type CodeDeliveryRunsPage = WireCodeDeliveryRunsPage;
export type CodeDeliveryRunTarget = WireCodeDeliveryRunTarget;
export type CodeDeliveryRunAction = WireCodeDeliveryRunAction;
export type CodeDeliveryRunActionBody = WireCodeDeliveryRunActionBody;
export type CodeDeliveryWorkflowJob = WireCodeDeliveryWorkflowJob;
export type CodeDeliveryDeploymentStatus = WireCodeDeliveryDeploymentStatus;
export type CodeDeliveryRepositoriesSnapshot =
  WireCodeDeliveryRepositoriesSnapshot;
export type CodeDeliveryActionResult = WireCodeDeliveryActionResult;

/** One isolated worktree + branch on a repo. */
export type CodeWorkspaceSnapshot = WireCodeWorkspaceSnapshot;
export type WorkspaceId = WireWorkspaceId;
export type CodeWorkspaceStatus = WireCodeWorkspaceStatus;

/** One durable conversation with an external coding engine. */
export type CodeSessionSnapshot = WireCodeSessionSnapshot;
export type CodeSessionId = WireCodeSessionId;
export type CodeSessionKind = WireCodeSessionKind;
export type CodeSessionLifecycle = WireCodeSessionLifecycle;
export type FenceReason = WireFenceReason;
export type Attention = WireAttention;
export type AttentionState = WireAttentionState;
export type AttentionSource = WireAttentionSource;

/** One user→engine cycle. */
export type CodeTurnSnapshot = WireCodeTurnSnapshot;
/** A queued follow-up row; its id becomes the promoted turn's id. */
export type QueuedCodeTurn = WireQueuedCodeTurn;
export type CodeTurnId = WireCodeTurnId;
export type CodeTurnStatus = WireCodeTurnStatus;
export type CodeUsage = WireCodeUsage;
export type Diffstat = WireDiffstat;
export type FileChangeKind = WireFileChangeKind;
export type CodeFileChange = WireCodeFileChange;
export type CodeWorkspaceFiles = WireCodeWorkspaceFiles;
export type CodeWorkspaceHistorySearchMatch =
  WireCodeWorkspaceHistorySearchMatch;
export type CodeWorkspaceHistorySearchSource =
  WireCodeWorkspaceHistorySearchSource;
export type CodeWorkspaceSearch = WireCodeWorkspaceSearch;
export type CodeWorkspaceSearchMatch = WireCodeWorkspaceSearchMatch;
export type CodeWorkspaceTree = WireCodeWorkspaceTree;
export type CodeWorkspaceBlob = WireCodeWorkspaceBlob;
export type CodeWorkspaceDiff = WireCodeWorkspaceDiff;
export type CodeWorkspacePrSnapshot = WireCodeWorkspacePrSnapshot;
/** One durable watch task on a workspace's pull request. */
export type CodeTriggerAction = WireCodeTriggerAction;
export type CodeTriggerCondition = WireCodeTriggerCondition;
export type CodeTriggerSnapshot = WireCodeTriggerSnapshot;
export type CodeWatchSnapshot = WireCodeWatchSnapshot;
export type CodeWatchState = WireCodeWatchState;
export type CodeSessionActivity = WireCodeSessionActivity;
/** A harness subagent riding a session's digest (ADR 0052). */
export type CodeSubagentSummary = WireCodeSubagentSummary;
export type CodeSubagentStatus = WireCodeSubagentStatus;
export type CodeCommitSnapshot = WireCodeCommitSnapshot;
export type CodePushSnapshot = WireCodePushSnapshot;
export type CodeActionSnapshot = WireCodeActionSnapshot;
export type PullRequestDigest = WirePullRequestDigest;
export type PullRequestCheck = WirePullRequestCheck;
export type PullRequestCheckBucket = WirePullRequestCheckBucket;
export type PullRequestComment = WirePullRequestComment;
export type PullRequestCommentKind = WirePullRequestCommentKind;
/** The PR conversation, read live from the host and never persisted. */
export type CodePrCommentsSnapshot = WireCodePrCommentsSnapshot;
export type CodePrMergeMethod = WireCodePrMergeMethod;
/** Body of POST /code/workspaces/{id}/pr/merge. */
export type MergeCodePrBody = WireMergeCodePrBody;
export type CodeTerminalSnapshot = WireCodeTerminalSnapshot;
export type CodeTerminalRead = WireCodeTerminalRead;
export type CodeTerminalActivityNotice = WireCodeTerminalActivityNotice;
export type CodeSessionDigest = WireCodeSessionDigest;
export type CodeUpdateNotice = WireCodeUpdateNotice;
export type CodeCloneDefaults = WireCodeCloneDefaults;
export type CodeRepoSource = WireCodeRepoSource;
export type CodeRepoSources = WireCodeRepoSources;
export type CodeGithubRepositories = WireCodeGithubRepositories;
export type CodeGithubRepository = WireCodeGithubRepository;
export type CodeCloneJobSnapshot = WireCodeCloneJobSnapshot;
/** Where the warm install of one pinned engine stands. */
export type CodeHarnessInstallSnapshot = WireCodeHarnessInstallSnapshot;
/** Where new code worktrees land, and what clearing the setting returns to. */
export type CodeWorktreeRoot = WireCodeWorktreeRoot;
/** The transcript file a fork wrote into the worktree, ready to hand on. */
export type CodeForkTranscript = WireCodeForkTranscript;
/** One failing check's job log, downloaded and waiting on disk. */
export type CodeCheckLog = WireCodeCheckLog;
/** One failing check whose job log could not be read. */
export type CodeCheckLogError = WireCodeCheckLogError;
/** The failing job logs written for a workspace's pull request. */
export type CodeCheckLogsSnapshot = WireCodeCheckLogsSnapshot;

/** Subscription quota windows exposed by Model Gateway or a direct harness. */
export type CodeSubscriptionUsage = {
  source: "model_gateway" | "direct" | "unavailable";
  providers: CodeSubscriptionUsageProvider[];
  diagnostics: string[];
};

export type CodeSubscriptionUsageProvider = {
  id: string;
  label: string;
  accounts: CodeSubscriptionUsageAccount[];
};

export type CodeSubscriptionUsageAccount = {
  id: string;
  label: string;
  is_own: boolean;
  state: string;
  updated_at_unix_seconds?: number;
  windows: CodeSubscriptionUsageWindow[];
};

export type CodeSubscriptionUsageWindow = {
  key: string;
  label: string;
  used_percent: number;
  resets_at_unix_seconds?: number;
  status?: string;
  model_scope?: string;
};

/** Journaled engine event and the sequenced WebSocket frame that carries it. */
export type CodeApprovalSnapshot = WireCodeApprovalSnapshot;
export type CodeApprovalState = WireCodeApprovalState;
export type CodeEvent = WireCodeEvent;
export type SequencedCodeEventFrame = WireSequencedCodeEventFrame;
export type ToolDetail = WireToolDetail;
export type ToolOutcome = WireToolOutcome;
export type HarnessNoticeLevel = WireHarnessNoticeLevel;

/** Doctor report for every registered engine. */
export type HarnessDoctorReport = WireHarnessDoctorReport;
export type HarnessDoctorEntry = WireHarnessDoctorEntry;
export type HarnessKind = WireHarnessKind;
export type HarnessTier = WireHarnessTier;
export type HarnessCaps = WireHarnessCaps;
export type CapLevel = WireCapLevel;
