import { useMemo, useRef, useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import {
  ApiClient,
  type AgentActivityHistoryEntry,
  type AgentRun,
  type AgentRunProgress,
  type AgentRunTaskPlan,
  type Chat,
  type ExecConfigInfo,
  type ManagedPolicy,
  type ModelInfo,
  type PendingFolderAccessRequest,
  type PendingOutputWritebackRequest,
  type PendingPlanApproval,
  type PendingUserQuestions,
  type PlanDecision,
  type PluginCatalog,
  type PluginEnableUpdate,
  type PromptBody,
  type ProviderInfo,
  type QueuedTurn,
  RENDERER_FOLDER_ACCESS_REASON,
  type SandboxAgentCancellation,
  type TaskPlan,
  type UserQuestionAnswer,
} from "@/api";
import { ChatView } from "@/ChatView";
import { useChatSessionStore } from "@/ChatSessionStore";
import { initialChatSessionState } from "@/ChatSessionReducer";
import {
  useComposerDrafts,
  type ComposerAttachmentDraft,
} from "@/ComposerDrafts";
import type {
  ComposerFiles,
  ComposerFolders,
  ComposerImages,
} from "@/Composer";
import type { ImageAttachment } from "@/ImageAttachments";
import { ManagedPolicyContext } from "@/managedPolicy";
import type { ChatMessage, RetryableTurn } from "@/MessageList";
import { ModelMenu } from "@/ModelMenu";
import { usePendingPrompts } from "@/PendingPrompts";
import { PermissionModeMenu } from "@/PermissionModeMenu";
import { useUiStore, type ActiveTurnSendMode } from "@/UiStore";
import { contextUsageWarning, taskPlan, userQuestions } from "./fixtures";

const CHAT_ID = "storybook-chat";
const ACTIVE_TURN_ID = "turn-storybook";

const storyChat = {
  id: CHAT_ID,
  project_id: null,
  title: "Conversation design review",
  model: "model_gateway::gpt-5.6-sol",
  reasoning_effort: "high",
  permission_mode: "ask",
  network_policy: { mode: "open" },
  attachment_revision: 1,
  root_attachments: [],
  created_at: "2026-08-24T13:00:00Z",
} satisfies Chat;

const storyPolicy = {
  managed: false,
  source: "unmanaged",
  misconfigured: false,
  allow_local_mcp_servers: false,
} satisfies ManagedPolicy;

const storyModels = [
  {
    key: "model_gateway::gpt-5.6-sol",
    id: "gpt-5.6-sol",
    display_name: "GPT-5.6 Sol",
    provider: "model_gateway",
    vendor: "openai",
    verification: "verified",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "medium", "high"],
    multimodal: true,
    available: true,
    recommended: true,
  },
  {
    key: "model_gateway::claude-sonnet-4-6",
    id: "claude-sonnet-4-6",
    display_name: "Claude Sonnet 4.6",
    provider: "model_gateway",
    vendor: "anthropic",
    verification: "verified",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "medium", "high"],
    multimodal: true,
    available: true,
    recommended: true,
  },
] satisfies ModelInfo[];

const storyProviders = [
  {
    kind: "model_gateway",
    enabled: true,
    has_credential: true,
    models: [],
  },
] satisfies ProviderInfo[];

const emptyPlugins: PluginCatalog = { plugins: [], skills: [], prompts: [] };

const imageAttachment: ImageAttachment = {
  id: "image-local",
  name: "compact-layout.png",
  byteLen: 248_000,
  uploadedBytes: 248_000,
  status: "ready",
  previewUrl: null,
  attachmentId: "image-published",
  mediaType: "image/png",
  width: 1440,
  height: 900,
  error: null,
};

const baseMessages: ChatMessage[] = [
  {
    id: "message-user",
    role: "user",
    text: "Audit the conversation flow and keep the dense states easy to scan.",
    createdAt: "2026-08-24T13:02:00Z",
  },
  {
    id: "message-assistant",
    role: "assistant",
    text: "I am grouping routine activity and keeping decisions beside the composer.",
    reasoning:
      "The transcript, queue, and pending prompts need to share one visual hierarchy.",
    sources: [],
    createdAt: "2026-08-24T13:02:10Z",
  },
];

const streamingMessages: ChatMessage[] = [
  ...baseMessages,
  {
    id: "message-stream-user",
    role: "user",
    text: "Check the active response and compact pressure next.",
    createdAt: "2026-08-24T13:04:00Z",
  },
  {
    id: "message-stream-assistant",
    role: "assistant",
    text: "The active response keeps the stop action visible while the composer offers queued guidance.",
    sources: [],
    createdAt: "2026-08-24T13:04:08Z",
  },
];

const retryableMessages: ChatMessage[] = [
  {
    id: "message-failure-user",
    role: "user",
    text: "Generate the compact conversation review.",
    files: [
      {
        documentId: "brief-document",
        name: "conversation-brief.pdf",
        mediaType: "application/pdf",
      },
    ],
    invokedSkills: ["browser"],
    createdAt: "2026-08-24T13:08:00Z",
  },
  {
    id: "message-failure",
    role: "turn_failure",
    category: "transient",
    detail: "The provider closed the stream before the response completed.",
    model: { id: "gpt-5.6-sol", provider: "model_gateway" },
    invokedSkills: ["browser"],
  },
];

const pendingApprovalMessages: ChatMessage[] = [
  ...baseMessages,
  {
    id: "message-approval",
    role: "approval",
    callId: "approval-storybook",
    summary: "Run the focused Storybook checks",
    preview: {
      tool: "exec",
      command: "pnpm",
      args: ["storybook:build"],
      cwd: "crates/tidebreak-desktop/ui",
      files: ["src/stories/ChatView.stories.tsx"],
      summary: "Build the conversation stories",
    },
    canApprove: true,
    canRemember: false,
    grantRungs: [],
  },
];

const queuedTurn: QueuedTurn = {
  id: "queued-storybook",
  chat_id: CHAT_ID,
  content: "After this response, verify the 420-pixel layout.",
  attachments: [],
  file_attachments: [],
  invoked_skills: [],
  voice_input_used: false,
  position: 1,
  created_at: "2026-08-24T13:05:00Z",
  updated_at: "2026-08-24T13:05:00Z",
};

const pendingQuestion = {
  ...userQuestions,
  callId: "questions-storybook",
  turnId: ACTIVE_TURN_ID,
  questions: [userQuestions.questions[0]!],
} satisfies PendingUserQuestions;

const pendingPlan = {
  callId: "plan-storybook",
  turnId: ACTIVE_TURN_ID,
  title: "Review the conversation-state coverage plan",
  plan: `## Story coverage

1. Mount the production composer controls and pending decisions.
2. Show folder and output consent beside background-agent progress.
3. Verify desktop and compact layouts before publishing the review.`,
  proposedAt: "2026-08-24T13:09:00Z",
} satisfies PendingPlanApproval;

const pendingFolderAccess = {
  callId: "folder-access-storybook",
  turnId: ACTIVE_TURN_ID,
  reason: RENDERER_FOLDER_ACCESS_REASON,
  folderHint: "documents",
  claimedByDesktop: false,
} satisfies PendingFolderAccessRequest;

const pendingOutputWriteback = {
  callId: "output-writeback-storybook",
  turnId: ACTIVE_TURN_ID,
  mode: "replace",
  claimedByDesktop: false,
} satisfies PendingOutputWritebackRequest;

const agentMessages: ChatMessage[] = [
  ...baseMessages,
  {
    id: "message-agent-request",
    role: "user",
    text: "Delegate the visual audit and keep every result visible here.",
    createdAt: "2026-08-24T13:06:00Z",
  },
  {
    id: "message-agent-running",
    role: "tool",
    callId: "spawn-running",
    name: "spawn_sandbox_agent",
    status: "completed",
    backgroundAgentRunId: "agent-running",
  },
  {
    id: "message-agent-waiting",
    role: "tool",
    callId: "spawn-waiting",
    name: "spawn_sandbox_agent",
    status: "completed",
    backgroundAgentRunId: "agent-waiting",
  },
  {
    id: "message-agent-completed",
    role: "tool",
    callId: "spawn-completed",
    name: "spawn_sandbox_agent",
    status: "completed",
    backgroundAgentRunId: "agent-completed",
  },
  {
    id: "message-agent-failed",
    role: "tool",
    callId: "spawn-failed",
    name: "spawn_sandbox_agent",
    status: "completed",
    backgroundAgentRunId: "agent-failed",
  },
  {
    id: "message-agent-summary",
    role: "assistant",
    text: "The visual audit is running across the conversation surfaces.",
    sources: [],
    createdAt: "2026-08-24T13:06:10Z",
  },
];

function agentRun(
  id: string,
  spawnCallId: string,
  status: AgentRun["status"],
  task: string,
): AgentRun {
  const live = [
    "active",
    "queued",
    "running",
    "cancelling",
    "waiting",
    "retry_wait",
    "needs_input",
  ].includes(status);
  return {
    id,
    parent_id: "foreground-storybook",
    spawn_call_id: spawnCallId,
    tier: "background",
    execution_location: "in_process",
    code_execution_provider: "local",
    status,
    model_steps: status === "completed" ? 5 : 2,
    usage: {
      input_tokens: 8_200,
      output_tokens: 1_900,
      cache_read_input_tokens: 5_600,
      cache_creation_input_tokens: 0,
    },
    task,
    started_at: "2026-08-24T13:06:00Z",
    finished_at: live ? null : "2026-08-24T13:10:00Z",
    last_error_code: status === "failed" ? "provider_stream_closed" : null,
    activity: status === "running" ? { kind: "exec", status: "running" } : null,
    submitted_outputs:
      status === "completed"
        ? [
            {
              output_id: "output-visual-review",
              filename: "visual-review.md",
            },
          ]
        : [],
    task_plan:
      status === "running"
        ? {
            completed: 2,
            total: 4,
            current: "Compare compact decision states",
            updated_at: "2026-08-24T13:08:00Z",
          }
        : undefined,
    terminal_text:
      status === "completed"
        ? "The desktop conversation review is complete."
        : status === "failed"
          ? "The compact capture stopped before the final state rendered."
          : null,
    created_at: "2026-08-24T13:05:30Z",
    updated_at: "2026-08-24T13:09:00Z",
  };
}

const agentRuns = [
  agentRun(
    "agent-running",
    "spawn-running",
    "running",
    "Audit the active conversation hierarchy",
  ),
  agentRun(
    "agent-waiting",
    "spawn-waiting",
    "needs_input",
    "Review the compact focus order",
  ),
  agentRun(
    "agent-completed",
    "spawn-completed",
    "completed",
    "Assess production primitives and spacing",
  ),
  agentRun(
    "agent-failed",
    "spawn-failed",
    "failed",
    "Capture the overloaded decision state",
  ),
] satisfies AgentRun[];

const agentProgress = {
  "agent-running": {
    entries: [
      {
        sequence: 1,
        text: "Comparing prompt replacement at desktop and compact widths",
        at: "2026-08-24T13:08:30Z",
      },
    ],
    nextSequence: 1,
  },
} satisfies Record<string, AgentRunProgress>;

const agentTaskPlans = {
  "agent-running": {
    run_id: "agent-running",
    steps: taskPlan.steps,
    updated_at: "2026-08-24T13:08:00Z",
  },
} satisfies Record<string, AgentRunTaskPlan>;

const agentActivity = {
  "agent-running": [
    {
      kind: "web_search",
      outcome: "completed",
      at: "2026-08-24T13:07:00Z",
    },
    {
      kind: "exec",
      outcome: "running",
      at: "2026-08-24T13:08:00Z",
    },
  ],
} satisfies Record<string, AgentActivityHistoryEntry[]>;

type DecisionOutcome = "success" | "pending" | "failure";

type StoryScenario = {
  id: string;
  messages: ChatMessage[];
  draft?: string;
  busy?: boolean;
  activeTurnId?: string | null;
  lastSeq?: number;
  sendMode?: ActiveTurnSendMode;
  attachments?: Partial<ComposerAttachmentDraft>;
  queue?: QueuedTurn[];
  plan?: TaskPlan | null;
  userQuestions?: PendingUserQuestions[];
  planApprovals?: PendingPlanApproval[];
  folderAccess?: PendingFolderAccessRequest[];
  outputWritebacks?: PendingOutputWritebackRequest[];
  questionOutcome?: DecisionOutcome;
  planOutcome?: DecisionOutcome;
  agentRuns?: AgentRun[];
  agentActivity?: Record<string, AgentActivityHistoryEntry[]>;
  agentTaskPlans?: Record<string, AgentRunTaskPlan>;
  agentProgress?: Record<string, AgentRunProgress>;
  agentRunsError?: string;
  nativeHost?: boolean;
};

function pendingDecision(): Promise<void> {
  return new Promise(() => undefined);
}

class StoryApiClient extends ApiClient {
  private queued: QueuedTurn[];
  private paused = false;

  constructor(private readonly scenario: StoryScenario) {
    super("http://storybook.invalid", "storybook-token");
    this.queued = [...(scenario.queue ?? [])];
  }

  enqueue(content: string): void {
    this.queued.push({
      ...queuedTurn,
      id: `queued-${this.queued.length + 1}`,
      content,
      position: this.queued.length + 1,
    });
  }

  override listPlugins(): Promise<PluginCatalog> {
    return Promise.resolve(emptyPlugins);
  }

  override getPromptBody(name: string): Promise<PromptBody> {
    return Promise.resolve({ name, body: "Review the conversation flow." });
  }

  override setPluginsEnabled(
    _update: PluginEnableUpdate,
  ): Promise<PluginCatalog> {
    return Promise.resolve(emptyPlugins);
  }

  override getExecConfig(): Promise<ExecConfigInfo> {
    return Promise.resolve({
      provider: "local",
      timeout_ms: 30_000,
      available: true,
      has_credential: true,
      providers: [
        { provider: "local", available: true },
        { provider: "e2b", available: true },
        { provider: "daytona", available: true },
      ],
      egress: {
        policy: { mode: "open" },
        enforcement: [],
      },
      detached_admission: [],
    } satisfies ExecConfigInfo);
  }

  override getTaskPlan(_chatId: string): Promise<TaskPlan | null> {
    return Promise.resolve(this.scenario.plan ?? null);
  }

  override listQueuedTurns(): Promise<{
    queued: QueuedTurn[];
    paused: boolean;
  }> {
    return Promise.resolve({ queued: [...this.queued], paused: this.paused });
  }

  override putQueuePaused(_chatId: string, paused: boolean): Promise<void> {
    this.paused = paused;
    return Promise.resolve();
  }

  override patchQueuedTurn(
    _chatId: string,
    turnId: string,
    update: { content?: string; position?: number },
  ): Promise<QueuedTurn> {
    const current = this.queued.find((turn) => turn.id === turnId)!;
    const next = { ...current, ...update };
    this.queued = this.queued.map((turn) => (turn.id === turnId ? next : turn));
    return Promise.resolve(next);
  }

  override deleteQueuedTurn(_chatId: string, turnId: string): Promise<void> {
    this.queued = this.queued.filter((turn) => turn.id !== turnId);
    return Promise.resolve();
  }

  override sendQueuedNow(): Promise<void> {
    this.paused = false;
    this.queued = [];
    return Promise.resolve();
  }

  override cancel(): Promise<void> {
    useChatSessionStore.getState().update((session) => ({
      ...session,
      busy: false,
      activeTurnId: null,
    }));
    return Promise.resolve();
  }

  override steer(): Promise<void> {
    return Promise.resolve();
  }

  override decideApproval(): Promise<void> {
    return Promise.resolve();
  }

  override listPendingUserQuestions(): Promise<PendingUserQuestions[]> {
    return Promise.resolve(this.scenario.userQuestions ?? []);
  }

  override listPendingPlanApprovals(): Promise<PendingPlanApproval[]> {
    return Promise.resolve(this.scenario.planApprovals ?? []);
  }

  override listPendingFolderAccessRequests(): Promise<
    PendingFolderAccessRequest[]
  > {
    return Promise.resolve(this.scenario.folderAccess ?? []);
  }

  override listPendingOutputWritebackRequests(): Promise<
    PendingOutputWritebackRequest[]
  > {
    return Promise.resolve(this.scenario.outputWritebacks ?? []);
  }

  override answerUserQuestions(
    _chatId: string,
    _callId: string,
    _answers: UserQuestionAnswer[],
  ): Promise<void> {
    if (this.scenario.questionOutcome === "pending") {
      return pendingDecision();
    }
    if (this.scenario.questionOutcome === "failure") {
      return Promise.reject(new Error("The question response was not saved."));
    }
    usePendingPrompts.getState().setUserQuestions(CHAT_ID, []);
    return Promise.resolve();
  }

  override decidePlan(
    _chatId: string,
    _callId: string,
    _decision: PlanDecision,
  ): Promise<void> {
    if (this.scenario.planOutcome === "pending") {
      return pendingDecision();
    }
    if (this.scenario.planOutcome === "failure") {
      return Promise.reject(new Error("The plan decision was not saved."));
    }
    usePendingPrompts.getState().setPlanApprovals(CHAT_ID, []);
    return Promise.resolve();
  }

  override listAgentRuns(): Promise<AgentRun[]> {
    if (this.scenario.agentRunsError) {
      return Promise.reject(new Error(this.scenario.agentRunsError));
    }
    return Promise.resolve(this.scenario.agentRuns ?? []);
  }

  override listAgentRunActivity(
    _chatId: string,
    runId: string,
  ): Promise<AgentActivityHistoryEntry[]> {
    return Promise.resolve(this.scenario.agentActivity?.[runId] ?? []);
  }

  override getAgentRunTaskPlan(
    _chatId: string,
    runId: string,
  ): Promise<AgentRunTaskPlan | null> {
    return Promise.resolve(this.scenario.agentTaskPlans?.[runId] ?? null);
  }

  override listAgentRunProgress(
    _chatId: string,
    runId: string,
    afterSequence = 0,
  ): Promise<AgentRunProgress> {
    const page = this.scenario.agentProgress?.[runId];
    if (!page || afterSequence >= page.nextSequence) {
      return Promise.resolve({ entries: [], nextSequence: afterSequence });
    }
    return Promise.resolve(page);
  }

  override cancelAgentRun(
    _chatId: string,
    runId: string,
  ): Promise<SandboxAgentCancellation> {
    return Promise.resolve({ id: runId, status: "cancelling" });
  }
}

function seedScenario(scenario: StoryScenario): void {
  useChatSessionStore.getState().update(() => ({
    ...initialChatSessionState(),
    messages: scenario.messages,
    busy: scenario.busy ?? false,
    activeTurnId: scenario.activeTurnId ?? null,
    lastSeq: scenario.lastSeq ?? 0,
  }));
  useComposerDrafts.getState().clearDraft(CHAT_ID);
  useComposerDrafts.getState().setDraft(CHAT_ID, scenario.draft ?? "");
  useComposerDrafts
    .getState()
    .setImages(CHAT_ID, scenario.attachments?.images ?? []);
  useComposerDrafts
    .getState()
    .setFiles(CHAT_ID, scenario.attachments?.files ?? []);
  useComposerDrafts
    .getState()
    .setSkills(CHAT_ID, scenario.attachments?.skills ?? []);
  useComposerDrafts
    .getState()
    .setFolders(CHAT_ID, scenario.attachments?.folders ?? []);
  usePendingPrompts.getState().reset(CHAT_ID);
  usePendingPrompts
    .getState()
    .setUserQuestions(CHAT_ID, scenario.userQuestions ?? []);
  usePendingPrompts
    .getState()
    .setPlanApprovals(CHAT_ID, scenario.planApprovals ?? []);
  usePendingPrompts
    .getState()
    .setFolderAccess(CHAT_ID, scenario.folderAccess ?? []);
  usePendingPrompts
    .getState()
    .setOutputWritebacks(CHAT_ID, scenario.outputWritebacks ?? []);
  useUiStore.getState().setActiveTurnSendMode(scenario.sendMode ?? "queue");
}

function storyRouter(children: ReactNode) {
  const rootRoute = createRootRoute();
  const chatRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/c/$chatId",
    validateSearch: (search: Record<string, unknown>) => ({
      focus: typeof search.focus === "string" ? search.focus : undefined,
      at: typeof search.at === "string" ? search.at : undefined,
    }),
    component: () => <>{children}</>,
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings/providers",
    component: () => <>{children}</>,
  });
  return createRouter({
    routeTree: rootRoute.addChildren([chatRoute, settingsRoute]),
    history: createMemoryHistory({ initialEntries: [`/c/${CHAT_ID}`] }),
  });
}

function StoryChat({
  client,
  scenario,
}: {
  client: StoryApiClient;
  scenario: StoryScenario;
}) {
  const [chat, setChat] = useState<Chat>(() => ({ ...storyChat }));
  const draftRef = useRef(scenario.draft ?? "");
  const images: ComposerImages = {
    items: scenario.attachments?.images ?? [],
    error: null,
    unsupportedModel: null,
    onAttachFiles: fn(),
    onRemove: (id) =>
      useComposerDrafts.getState().setImages(
        CHAT_ID,
        (scenario.attachments?.images ?? []).filter((image) => image.id !== id),
      ),
    onRetry: fn(),
  };
  const files: ComposerFiles = {
    items: scenario.attachments?.files ?? [],
    attaching: false,
    onAttach: fn(),
    onRemove: (documentId) =>
      useComposerDrafts.getState().setFiles(
        CHAT_ID,
        (scenario.attachments?.files ?? []).filter(
          (file) => file.documentId !== documentId,
        ),
      ),
  };
  const folderItems: ComposerFolders["items"] = [
    {
      rootId: "folder-research",
      displayName: "Research notes",
      status: "connected",
      availableInFutureChats: true,
      statements: [],
    },
  ];
  const folders: ComposerFolders = {
    items: folderItems,
    pendingIds: scenario.attachments?.folders ?? [],
    working: false,
    error: null,
    onAttach: fn(),
    onRemove: fn(),
  };

  const updateDraft = (value: string) => {
    draftRef.current = value;
    useComposerDrafts.getState().setDraft(CHAT_ID, value);
  };
  const submitDraft = async () => {
    const text = draftRef.current.trim();
    if (!text) return;
    useChatSessionStore.getState().update((session) => ({
      ...session,
      messages: [
        ...session.messages,
        {
          id: `submitted-${session.messages.length}`,
          role: "user",
          text,
          createdAt: "2026-08-24T13:12:00Z",
        },
      ],
    }));
    updateDraft("");
  };
  const queueDraft = async () => {
    const text = draftRef.current.trim();
    if (!text) return;
    client.enqueue(text);
    updateDraft("");
  };
  const retryTurn = (turn: RetryableTurn) => updateDraft(turn.text);

  return (
    <ManagedPolicyContext.Provider value={storyPolicy}>
      <div className="h-screen min-h-0 bg-page-background">
        <div className="mx-auto h-full max-w-6xl border-x border-border bg-background">
          <ChatView
            key={scenario.id}
            client={client}
            chat={chat}
            hydrated
            nativeHost={scenario.nativeHost ?? false}
            deletingChat={false}
            draftRef={draftRef}
            composerModelMenu={
              <ModelMenu
                models={storyModels}
                value={chat.model}
                defaultKey={storyModels[0]!.key}
                providers={storyProviders}
                onSetUpProvider={() => undefined}
                onChange={(model) =>
                  setChat((current) => ({ ...current, model }))
                }
              />
            }
            composerPermissionMenu={
              <PermissionModeMenu
                scopeKey={chat.id}
                value={chat.permission_mode}
                onChange={(permission_mode) =>
                  setChat((current) => ({ ...current, permission_mode }))
                }
              />
            }
            contextUsage={contextUsageWarning}
            composerImages={images}
            files={files}
            folders={folders}
            voiceInputUsed={false}
            onVoiceInputAccepted={fn()}
            attachError={null}
            onDraftChange={updateDraft}
            onSelectPrompt={(prompt) => updateDraft(prompt)}
            onSend={submitDraft}
            onQueue={queueDraft}
            onRetryTurn={retryTurn}
            onOpenAgentPanel={fn()}
            onOpenOutput={fn()}
          />
        </div>
      </div>
    </ManagedPolicyContext.Provider>
  );
}

function ChatViewStory({ scenario }: { scenario: StoryScenario }) {
  const [client] = useState(() => {
    seedScenario(scenario);
    return new StoryApiClient(scenario);
  });
  const router = useMemo(
    () => storyRouter(<StoryChat client={client} scenario={scenario} />),
    [client, scenario],
  );
  return <RouterProvider router={router as never} />;
}

async function expectBusyComposerControlsContained(canvasElement: HTMLElement) {
  const canvas = within(canvasElement);
  const input = await canvas.findByRole("textbox", { name: "Message" });
  const composer = input.closest("form");
  if (!composer) throw new Error("The message input has no composer form");
  const queue = canvas.getByRole("button", {
    name: "Queue message for after this response",
  });
  const stop = canvas.getByRole("button", { name: "Stop response" });
  const viewport = canvasElement.ownerDocument.documentElement;

  await expect(queue).toBeVisible();
  await expect(stop).toBeVisible();

  await waitFor(() => {
    const composerRect = composer.getBoundingClientRect();
    expect(composerRect.left).toBeGreaterThanOrEqual(0);
    expect(composerRect.right).toBeLessThanOrEqual(viewport.clientWidth);
    for (const control of [queue, stop]) {
      const controlRect = control.getBoundingClientRect();
      expect(controlRect.left).toBeGreaterThanOrEqual(composerRect.left);
      expect(controlRect.right).toBeLessThanOrEqual(composerRect.right);
      expect(controlRect.top).toBeGreaterThanOrEqual(composerRect.top);
      expect(controlRect.bottom).toBeLessThanOrEqual(composerRect.bottom);
    }
    expect(composer.scrollWidth).toBeLessThanOrEqual(composer.clientWidth);
  });
}

const meta = {
  title: "Conversation/ChatView",
  component: ChatViewStory,
  parameters: { layout: "fullscreen" },
  render: ({ scenario }) => (
    <ChatViewStory key={scenario.id} scenario={scenario} />
  ),
} satisfies Meta<typeof ChatViewStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AttachmentsAndDraft: Story = {
  args: {
    scenario: {
      id: "attachments-and-draft",
      messages: baseMessages,
      draft:
        "Use the attached layout, brief, skill, and research folder to tighten the compact conversation hierarchy.",
      attachments: {
        images: [imageAttachment],
        files: [
          {
            documentId: "document-storybook",
            displayName: "conversation-review.pdf",
            mediaType: "application/pdf",
            byteLen: 482_000,
          },
        ],
        skills: ["browser"],
        folders: ["folder-research"],
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const composer = await canvas.findByRole("textbox", { name: "Message" });
    await userEvent.click(composer);
    await expect(composer).toHaveFocus();
  },
};

export const ActiveStreamingQueueAndStop: Story = {
  args: {
    scenario: {
      id: "active-streaming-queue-stop",
      messages: streamingMessages,
      draft: "Also compare the queued state against the live transcript.",
      busy: true,
      activeTurnId: ACTIVE_TURN_ID,
      lastSeq: 12,
      sendMode: "queue",
      queue: [queuedTurn],
      plan: taskPlan,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("button", { name: "Stop response" }),
    ).toBeEnabled();
    await userEvent.click(
      canvas.getByRole("button", {
        name: "Queue message for after this response",
      }),
    );
    await waitFor(
      () =>
        expect(
          canvas.getByText(
            "Also compare the queued state against the live transcript.",
          ),
        ).toBeInTheDocument(),
      { timeout: 3_000 },
    );
    await expect(
      canvas.getByRole("button", { name: "Stop response" }),
    ).toBeEnabled();
  },
};

export const PendingApprovalReturnsToComposer: Story = {
  args: {
    scenario: {
      id: "pending-approval",
      messages: pendingApprovalMessages,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const composer = await canvas.findByRole("textbox", { name: "Message" });
    await userEvent.click(
      await canvas.findByRole("button", { name: "Yes, run it once" }),
    );
    await waitFor(() =>
      expect(
        canvas.queryByRole("group", { name: "Approval choices" }),
      ).not.toBeInTheDocument(),
    );
    await userEvent.click(composer);
    await expect(composer).toHaveFocus();
  },
};

export const PendingQuestionReturnsToComposer: Story = {
  args: {
    scenario: {
      id: "pending-question-return",
      messages: baseMessages,
      userQuestions: [pendingQuestion],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByText("Conversation states", { exact: true }),
    );
    await userEvent.click(canvas.getByRole("button", { name: "Continue" }));
    const composer = await canvas.findByRole("textbox", { name: "Message" });
    await userEvent.click(composer);
    await expect(composer).toHaveFocus();
  },
};

export const PendingPlanReturnsToComposer: Story = {
  args: {
    scenario: {
      id: "pending-plan-return",
      messages: baseMessages,
      planApprovals: [pendingPlan],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Execute plan" }),
    );
    const composer = await canvas.findByRole("textbox", { name: "Message" });
    await userEvent.click(composer);
    await expect(composer).toHaveFocus();
  },
};

export const CompactCrowdedPromptReplacement: Story = {
  args: {
    scenario: {
      id: "compact-crowded-prompt-replacement",
      messages: baseMessages,
      plan: taskPlan,
      userQuestions: [pendingQuestion],
      planApprovals: [pendingPlan],
    },
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const CompactPlanDecisionFailure: Story = {
  args: {
    scenario: {
      id: "compact-plan-decision-failure",
      messages: baseMessages,
      planApprovals: [pendingPlan],
      planOutcome: "failure",
    },
  },
  globals: { viewport: { value: "compact", isRotated: false } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Execute plan" }),
    );
    await expect(await canvas.findByRole("alert")).toHaveTextContent(
      "The plan decision was not saved.",
    );
    await expect(
      canvas.getByRole("button", { name: "Execute plan" }),
    ).toBeEnabled();
  },
};

const crowdedAgentScenario = {
  id: "crowded-agent-lifecycle",
  messages: agentMessages,
  draft: "Summarize the final agent findings after every decision is resolved.",
  plan: taskPlan,
  folderAccess: [pendingFolderAccess],
  outputWritebacks: [pendingOutputWriteback],
  agentRuns,
  agentActivity,
  agentTaskPlans,
  agentProgress,
  nativeHost: true,
} satisfies StoryScenario;

export const CrowdedDecisionsAndAgentLifecycle: Story = {
  args: { scenario: crowdedAgentScenario },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText(
        "Comparing prompt replacement at desktop and compact widths",
      ),
    ).toBeInTheDocument();
    await expect(
      canvas.getByRole("region", { name: "Background agents" }),
    ).toBeInTheDocument();
  },
};

export const CompactCrowdedDecisionsAndAgentLifecycle: Story = {
  args: {
    scenario: {
      ...crowdedAgentScenario,
      id: "compact-crowded-agent-lifecycle",
    },
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const RetryableFailure: Story = {
  args: {
    scenario: {
      id: "retryable-failure",
      messages: retryableMessages,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Retry" }));
    await expect(
      await canvas.findByRole("textbox", { name: "Message" }),
    ).toHaveValue("Generate the compact conversation review.");
  },
};

export const CompactPressure: Story = {
  args: {
    scenario: {
      id: "compact-pressure",
      messages: streamingMessages,
      draft:
        "Keep the approval, queue, task plan, attachments, context meter, and stop action readable without hiding the response.",
      busy: true,
      activeTurnId: ACTIVE_TURN_ID,
      lastSeq: 18,
      queue: [queuedTurn],
      plan: taskPlan,
      attachments: {
        images: [imageAttachment],
        files: [
          {
            documentId: "compact-document",
            displayName: "compact-review.pdf",
            mediaType: "application/pdf",
            byteLen: 482_000,
          },
        ],
        skills: ["browser"],
        folders: ["folder-research"],
      },
    },
  },
  globals: { viewport: { value: "compact", isRotated: false } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expectBusyComposerControlsContained(canvasElement);
    await expect(
      canvas.getByRole("button", {
        name: /Context 4 1 image · 1 file · 1 folder · 1 skill Show details/i,
      }),
    ).toHaveAttribute("aria-expanded", "false");
  },
};

export const MinimumWindowBusyContext: Story = {
  args: {
    scenario: {
      id: "minimum-window-busy-context",
      messages: streamingMessages,
      draft:
        "Keep the queued guidance and stop action visible while this response runs.",
      busy: true,
      activeTurnId: ACTIVE_TURN_ID,
      lastSeq: 18,
      queue: [queuedTurn],
      attachments: {
        images: [imageAttachment],
        files: [
          {
            documentId: "minimum-window-document",
            displayName: "conversation-review.pdf",
            mediaType: "application/pdf",
            byteLen: 482_000,
          },
        ],
        skills: ["browser"],
        folders: ["folder-research"],
      },
    },
  },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const context = await canvas.findByRole("button", {
      name: /Context 4 1 image · 1 file · 1 folder · 1 skill Show details/i,
    });
    await expect(context).toBeVisible();
    await expect(context).toHaveAttribute("aria-expanded", "false");
    await expectBusyComposerControlsContained(canvasElement);
    await userEvent.click(context);
    await expect(context).toHaveAttribute("aria-expanded", "true");
    await expect(canvas.getByText("conversation-review.pdf")).toBeVisible();
    await userEvent.click(
      canvas.getByRole("button", {
        name: /Context 4 1 image · 1 file · 1 folder · 1 skill Hide details/i,
      }),
    );
    await expect(
      canvas.getByRole("button", {
        name: /Context 4 1 image · 1 file · 1 folder · 1 skill Show details/i,
      }),
    ).toHaveAttribute("aria-expanded", "false");
    context.blur();
  },
};
