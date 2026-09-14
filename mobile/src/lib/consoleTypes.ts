/**
 * The gateway console's wire shapes, member and runtime only.
 *
 * Hand-transcribed from the serializers named on each block, the way the
 * Tidewatch app carried them. The administrator half of the same API is
 * generated from the vendored OpenAPI document (#3400) and deliberately does
 * not live here: these are the surfaces any signed-in account reaches, and
 * they are the ones this slice renders.
 *
 * Two conventions run through the file. Every closed vocabulary is widened
 * with `(string & {})` because the gateway versions them and a newer
 * installation may send a value this build has never seen — rendering the raw
 * slug beats crashing or blanking. And every optional field is optional
 * because an older gateway omits it; absent means "this installation cannot
 * say", never zero.
 */

/** Lifecycle states (`SandboxState`, domain/src/sandbox.rs). */
export type SandboxState =
  | "pending"
  | "provisioning"
  | "running"
  | "completing"
  | "completed"
  | "failed"
  | "cancelled"
  | "expired"
  | "ceiling_exceeded"
  | (string & {});

export const TERMINAL_STATES: ReadonlySet<string> = new Set([
  "completed",
  "failed",
  "cancelled",
  "expired",
  "ceiling_exceeded",
]);

export function isTerminal(state: SandboxState): boolean {
  return TERMINAL_STATES.has(state);
}

/**
 * Derived watch-and-steer phase (mg ADR 0044). Never persisted server-side;
 * the catch-all arms repeat the lifecycle state so a badge is never blank.
 */
export type SandboxPhase =
  | "awaiting_scheduling"
  | "initializing"
  | "bootstrapping"
  | "preparing_workspace"
  | "working"
  | "delivering"
  | "idle"
  | SandboxState;

export type SandboxHarness =
  | "claude_code"
  | "codex"
  | "opencode"
  | "grok_build"
  | "custom"
  | (string & {});

export type ReasoningEffort =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max"
  | "ultra"
  | (string & {});

/** mg ADR 0047: goal keeps resuming after successful turns; turn waits for steer. */
export type SandboxRunMode = "goal" | "turn" | (string & {});

/**
 * Why autonomous resume was refused (`SandboxContinuationGate`). Only
 * `task_complete` ends the run; the rest park it idle, where a steering
 * message opens another turn.
 */
export type SandboxContinuationGate =
  | "max_turns"
  | "exit_status"
  | "turn_mode"
  | "task_complete"
  | "acceptance"
  | "soft_ceiling_wrap_up"
  | (string & {});

/** One event in a sandbox's durable, gap-free, cursor-resumable stream. */
export type SandboxEvent = {
  seq: number;
  kind: string;
  payload: Record<string, unknown>;
  created_at: string;
};

/**
 * Sandbox row as served by the versioned control-plane API (`SandboxView` on
 * `GET /api/v1/admin/sandboxes`). Optional fields double as capability probes:
 * `phase` and `spend_microusd` are absent on gateways older than mg ADR
 * 0044/0047.
 */
export type SandboxView = {
  id: string;
  profile_name: string;
  user_id: string;
  state: SandboxState;
  harness: SandboxHarness;
  mode?: SandboxRunMode;
  task_prompt: string;
  requested_reasoning_effort?: ReasoningEffort | null;
  effective_reasoning_effort?: ReasoningEffort | null;
  repository_url?: string | null;
  repository_ref?: string | null;
  failure_reason?: string | null;
  scheduling_reason?: string | null;
  scheduling_message?: string | null;
  expires_at?: string | null;
  last_activity_at?: string | null;
  completed_at?: string | null;
  created_at: string;
  latest_event_seq: number;
  pending_messages: number;
  spend_microusd?: number | null;
  spend_ceiling_microusd?: number | null;
  /** What the spend figures measure. `model_cost` on current gateways. */
  spend_basis?: "model_cost" | null;
  wall_clock_timeout_seconds?: number | null;
  idle_timeout_seconds?: number | null;
  phase?: SandboxPhase;
  phase_detail?: string | null;
  spawning_conversation_id?: string | null;
  max_turns?: number | null;
  turns_used?: number | null;
  /** Whether a successful turn resumes on its own; false parks the run. */
  may_resume?: boolean | null;
  /** Set when `may_resume` is false: which gate refused the resume. */
  continuation_gate?: SandboxContinuationGate | null;
};

/** `GET /api/v1/admin/sandboxes` — matching rows, newest first. */
export type SandboxListResponse = {
  data: SandboxView[];
};

/** The scheduler's own reason for the sandboxes it has not placed. */
export type SandboxSchedulingPressure = {
  reason: string;
  message?: string | null;
};

export type SandboxCapacityClass =
  | "ephemeral_storage"
  | "affinity"
  | "taints"
  | (string & {});

export type SandboxSchedulableCapacity = {
  available: number;
  limiting_class?: SandboxCapacityClass | null;
  autoscaling_active?: boolean | null;
  retry_after_seconds: number;
};

/**
 * Concurrency caps and occupancy from `GET /api/v1/admin/sandbox-concurrency`.
 * The `running` totals are what the caps compare — a sandbox the cluster has
 * not placed holds its slot exactly as a running one does — and the
 * `awaiting_scheduling` counts are the subsets no node has accepted yet.
 */
export type SandboxConcurrencyView = {
  user_limit: number;
  user_running: number;
  user_awaiting_scheduling: number;
  user_exhausted: boolean;
  installation_limit: number;
  installation_running: number;
  installation_awaiting_scheduling: number;
  installation_exhausted: boolean;
  user_placed?: number;
  installation_placed?: number;
  schedulable?: SandboxSchedulableCapacity | null;
  scheduling_pressure?: SandboxSchedulingPressure | null;
};

export type SandboxConcurrencyResponse = {
  data: SandboxConcurrencyView;
};

/** Owner-only steering inbox rows; an administrator sees metadata, no bodies. */
export type SandboxMessage = {
  seq: number;
  body?: string;
  interrupt: boolean;
  sender_user_id: string;
  created_at: string;
  delivered: boolean;
};

/**
 * `GET /api/v1/admin/sandboxes/{id}` — the sandbox plus the most recent
 * stretch of its event stream and the steering inbox.
 */
export type SandboxDetailResponse = {
  sandbox: SandboxView;
  /** Events at or after `events_from_seq`, oldest first. At most 500. */
  events: SandboxEvent[];
  events_from_seq?: number;
  events_truncated?: boolean;
  inbox?: SandboxMessage[];
};

/** What the runtime surface answers when a steering message is accepted. */
export type SandboxMessageReceipt = {
  seq: number;
  pending_messages?: number;
};

/**
 * Usage rollups from `GET /api/v1/admin/usage`.
 *
 * Two modes share the endpoint: with no parameters it returns the
 * compatibility view (`by_*` arrays, scope `self` or `installation`); any
 * filter or `group_by` switches to query mode (scope `filtered`, rows in
 * `grouped`, `by_*` empty). The views are cuts of the same events, not
 * partitions — summing across them double-counts.
 */
export type ModelUsageRow = {
  gateway_model_id: string;
  provider_name: string;
  inference_requests: number;
  input_tokens: number;
  output_tokens: number;
  priced_requests: number;
  pending_rate_requests: number;
  provisional_requests: number;
  unpriceable_requests: number;
  /** Metered (installation-billed) spend only — never sum billing classes. */
  estimated_cost_microusd: number;
  subscription_cost_microusd?: number;
  provisioned_cost_microusd?: number;
  credits_cost_microusd?: number;
  subscription_requests?: number;
};

export type GroupedUsageRow = {
  /** Dimension names mapped to recorded values; historical nulls stay null. */
  key: Record<string, string | null>;
  inference_requests: number;
  input_tokens: number;
  output_tokens: number;
  priced_requests: number;
  pending_rate_requests: number;
  provisional_requests: number;
  unpriceable_requests: number;
  estimated_cost_microusd: number;
  subscription_cost_microusd: number;
  provisioned_cost_microusd?: number;
  credits_cost_microusd?: number;
  subscription_requests: number;
  attributed_requests: number;
  unattributed_requests: number;
  last_activity_at?: string | null;
};

export type UserUsageRow = {
  user_id: string;
  display_name: string;
  email: string;
  inference_requests: number;
  input_tokens: number;
  output_tokens: number;
  /** Below `inference_requests` → the cost is a floor, not a total. */
  priced_requests: number;
  pending_rate_requests: number;
  provisional_requests: number;
  unpriceable_requests: number;
  estimated_cost_microusd: number;
  subscription_cost_microusd: number;
  provisioned_cost_microusd?: number;
  credits_cost_microusd: number;
  subscription_requests: number;
  tool_calls: number;
  app_requests: number;
};

export type ClientUsageRow = {
  /** Null for rows recorded before clients were identified. */
  client_name: string | null;
  inference_requests: number;
  /**
   * Requests that resolved to a conversation. The remainder routed fine but
   * asserted no conversation identifier — why a working harness can look
   * absent from conversation views.
   */
  attributed_requests: number;
  unattributed_requests: number;
  input_tokens: number;
  output_tokens: number;
  priced_requests: number;
  pending_rate_requests: number;
};

export type SandboxUsageRow = {
  sandbox_id: string;
  user_id: string;
  inference_requests: number;
  input_tokens: number;
  output_tokens: number;
  priced_requests: number;
  estimated_cost_microusd: number;
  subscription_cost_microusd: number;
  provisioned_cost_microusd?: number;
  credits_cost_microusd?: number;
  last_activity_at?: string | null;
};

export type ExecutionKindUsageRow = {
  execution_kind: string;
  inference_requests: number;
  input_tokens: number;
  output_tokens: number;
  priced_requests: number;
  pending_rate_requests: number;
  provisional_requests: number;
  unpriceable_requests: number;
  estimated_cost_microusd: number;
  subscription_cost_microusd: number;
  provisioned_cost_microusd?: number;
  credits_cost_microusd?: number;
};

export type AppUsageRow = {
  app_id: string;
  app_name: string;
  tool_calls: number;
  app_requests: number;
};

export type UsageResponse = {
  scope: "self" | "installation" | "filtered" | (string & {});
  by_user?: UserUsageRow[];
  by_model?: ModelUsageRow[];
  by_client?: ClientUsageRow[];
  by_sandbox?: SandboxUsageRow[];
  by_execution_kind?: ExecutionKindUsageRow[];
  by_app?: AppUsageRow[];
  grouped?: GroupedUsageRow[] | null;
  grouped_truncated?: boolean | null;
};

/** The dimensions the filtered usage read cross-tabs one account by. */
export type UsageDimension = "model" | "client" | "sandbox" | "execution_kind";

/** `InferenceProtocol` — what a client speaks to the gateway. */
export type InferenceProtocol =
  | "anthropic_messages"
  | "openai_responses"
  | (string & {});

export type ConnectedAppKind =
  | "datadog"
  | "sentry"
  | "linear"
  | "figma"
  | "slack"
  | "atlassian"
  | "pagerduty"
  | "vercel"
  | "generic_mcp"
  | "rest_api"
  | "model_gateway"
  | "git_forge"
  | "gitlab"
  | (string & {});

/**
 * Only delegated-OAuth apps are ever anything but `ready`: a shared-secret app
 * carries no per-user credential to connect.
 */
export type AppConnection =
  | "ready"
  | "not_connected"
  | "authorization_required"
  | (string & {});

export type CatalogModel = {
  id: string;
  name: string;
  /** Both arms when one catalog row serves both protocols. */
  protocols: InferenceProtocol[];
  aliases?: string[];
  supports_tools: boolean;
  supports_vision: boolean;
  supported_reasoning_efforts?: ReasoningEffort[];
  context_window?: number;
  max_output_tokens?: number;
  provider_name: string;
};

export type CatalogApp = {
  id: string;
  name: string;
  app_kind: ConnectedAppKind;
  enabled: boolean;
  mcp_endpoint_slugs: string[];
  connection: AppConnection;
};

/**
 * `GET /api/v1/me/catalog` — member-scoped: what *this account* may invoke,
 * never the installation's inventory.
 */
export type CatalogResponse = {
  models: CatalogModel[];
  apps: CatalogApp[];
};

/** `GET /api/v1/cli/apps` — the entitled connected apps, identity only. */
export type AppListResponse = {
  apps: {
    id: string;
    name: string;
    app_kind: ConnectedAppKind;
    enabled: boolean;
    mcp_endpoint_slugs?: string[];
  }[];
};

export type CostLimitScopeType =
  | "installation"
  | "user"
  | "team"
  | "gateway_model"
  | "connected_app"
  | (string & {});

export type CostLimitWindow = "daily" | "weekly" | "monthly" | (string & {});

/** `all` covers every metered request; `sandbox` only detached-agent spend. */
export type CostLimitAppliesTo = "all" | "sandbox" | (string & {});

export type CostLimitSource = "database" | "configuration" | (string & {});

/**
 * One cap from `GET /api/v1/admin/cost-limits/status`. That read is
 * deliberately not administrator-only: it answers "which caps can refuse me,
 * and how much room is left" for the calling account. `limit_microusd: 0` is a
 * total denial of the scope's metered inference, not "unlimited".
 */
export type CostLimitView = {
  id: string;
  scope_type: CostLimitScopeType;
  scope_id?: string | null;
  scope_label: string;
  applies_to: CostLimitAppliesTo;
  window: CostLimitWindow;
  limit_microusd: number;
  enabled: boolean;
  source: CostLimitSource;
  current_spend_microusd: number;
  /** Server-computed; 1.0 when the limit is 0, so a denial reads as full. */
  used_fraction: number;
  warning_reached: boolean;
  exceeded: boolean;
  window_start: string;
  resets_at: string;
};

export type CostLimitListResponse = {
  data: CostLimitView[];
};

export type SharedAppStatus =
  | "draft"
  | "published"
  | "disabled"
  | (string & {});

export type SharedAppDescription = {
  id: string;
  /** Compare against the signed-in account to tell your drafts from a teammate's. */
  owner_user_id: string;
  name: string;
  slug: string;
  status: SharedAppStatus;
  current_revision?: number;
  created_at: string;
  updated_at: string;
};

export type SharedAppListResponse = {
  shared_apps: SharedAppDescription[];
  truncated: boolean;
};

/** Names, never addresses: endpoints and credentials stay on the server. */
export type SharedAppBindingView = {
  connected_app_id: string;
  /** Absent when the bound app's snapshot row is gone (app deleted). */
  display_name?: string;
  operation_ids: string[];
  /**
   * Whether *this* viewer reaches that app in their own right. A shared app
   * confers reachability to itself, never to what it calls, so false means
   * every call through this binding is refused for them.
   */
  viewer_reachable: boolean;
};

export type SharedAppConsentState = {
  required: boolean;
  /** The revision an acceptance must pin; absent when the app serves none. */
  revision_id?: string;
};

export type SharedAppManifestParameter = {
  name: string;
  kind: string;
};

/**
 * Detail flattens the list description and adds the current revision's
 * manifest view. `manifest_revision` — not `current_revision` — is the one to
 * show: a republish between the two reads makes them differ.
 */
export type SharedAppDetailResponse = SharedAppDescription & {
  title?: string;
  manifest_revision?: number;
  manifest_revision_id?: string;
  parameters: SharedAppManifestParameter[];
  bindings: SharedAppBindingView[];
  consent: SharedAppConsentState;
};

export type SharedAppConsentResponse = {
  shared_app_id: string;
  revision_id: string;
  revision: number;
};

export type ConversationCostEvent = {
  id: string;
  occurred_at: string;
  gateway_model_id: string;
  provider_name: string;
  /** `priced`, `pending_rate`, `provisional`, or `unpriceable`. */
  cost_state: string;
  billing_class?:
    | "billed"
    | "notional"
    | "credits"
    | "provisioned"
    | (string & {});
  estimated_cost_microusd?: number | null;
  /** `completed`, `incomplete`, `truncated`, `failed`; null is not "completed". */
  terminal_state?: string | null;
  finish_reason?: string | null;
  /** Null is "uncounted", which is not a counted zero. */
  tool_call_count?: number | null;
  duration_ms: number;
};

/**
 * `GET /api/v1/admin/conversations/{id}`. Members read only their own;
 * someone else's id answers 404 deliberately, so a 404 is not "no such
 * conversation". Each cost field names one payer partition and nothing here
 * may be summed into a single "cost".
 */
export type ConversationView = {
  id: string;
  title?: string | null;
  user_id: string;
  user_display_name: string;
  user_email: string;
  harness: string;
  ephemeral: boolean;
  /** Latest observed slug — a display handle, never a spend key. */
  repo_slug?: string | null;
  repo_ref?: string | null;
  /** Above one means `repo_slug` names only the most recent of several. */
  project_slug_count: number;
  first_seen_at: string;
  last_activity_at: string;
  inference_requests: number;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  priced_request_count: number;
  pending_rate_request_count: number;
  provisional_request_count: number;
  unpriceable_request_count: number;
  estimated_cost_microusd: number;
  subscription_cost_microusd?: number;
  provisioned_cost_microusd?: number;
  credits_cost_microusd?: number;
  last_turn_estimated_cost_microusd?: number | null;
  last_turn_subscription_cost_microusd?: number | null;
  last_turn_provisioned_cost_microusd?: number | null;
  last_turn_credits_cost_microusd?: number | null;
  model_count: number;
  provider_count: number;
  /** At most 500, newest first. */
  events: ConversationCostEvent[];
  events_truncated: boolean;
};

export type ModelProviderKind =
  | "bedrock"
  | "anthropic"
  | "openai"
  | "google_vertex"
  | "xai"
  | "opencode_go"
  | "openai_compatible"
  | "development_fixture"
  | (string & {});

export type SubscriptionLimitState =
  | "available"
  | "cooling_down"
  | "exhausted_reauth"
  | (string & {});

/** One normalized provider quota window; raw upstream headers never cross. */
export type SubscriptionUsageWindow = {
  /** Provider window key, e.g. `5h`, `7d`, `7d-opus`, `primary`. */
  key: string;
  label: string;
  /** Clamped 0..=100 server-side. */
  used_percent: number;
  /** `allowed`, `allowed_warning`, `rejected`; absent on older snapshots. */
  status?: string | null;
  /** Quota family this window meters; absent reads as binding-wide. */
  model_scope?: string | null;
  resets_at_unix_seconds?: number | null;
  window_minutes?: number | null;
  observed_at_unix_seconds?: number | null;
};

export type SubscriptionCooldown = {
  /** Absent gates every quota family. */
  model_scope?: string | null;
  until_unix_seconds: number;
};

/** Independent facts about one account, not a state machine. */
export type SubscriptionBinding = {
  binding_id: string;
  label: string;
  /** False for a teammate's shared account, addressed `<owner_email>/<label>`. */
  is_own: boolean;
  owner_email?: string | null;
  shared_via_team_slug?: string | null;
  shared_via_team_name?: string | null;
  account_hint?: string | null;
  plan_hint?: string | null;
  limit_state?: SubscriptionLimitState | null;
  cooldowns: SubscriptionCooldown[];
  fallback_to_metered: boolean;
  reauthorization_required: boolean;
  /** Whether the gateway has a stable account-usage source for this kind. */
  usage_supported: boolean;
  /** Latest stored snapshot, most frequent reset first. */
  usage_windows: SubscriptionUsageWindow[];
  usage_updated_at_unix_seconds?: number | null;
  reset_credits_supported: boolean;
};

export type SubscriptionProvider = {
  provider_id: string;
  name: string;
  provider_kind: ModelProviderKind;
  connect_available: boolean;
  api_key_connect: boolean;
  sharing_enabled: boolean;
  bindings: SubscriptionBinding[];
};

export type SubscriptionListResponse = {
  providers: SubscriptionProvider[];
};

/**
 * `GET /api/v1/cli/subscriptions/{binding_id}/usage` — a live re-read.
 * Deliberately OpenAI-only and owner-only: another provider kind answers 422
 * and a borrowed binding 404, so this refines the listing's snapshot for the
 * accounts that have it rather than replacing it.
 */
export type SubscriptionUsageResponse = {
  usage_windows: SubscriptionUsageWindow[];
  usage_updated_at_unix_seconds: number;
  reset_credits_available?: number | null;
};

/** Integer micro-US-dollars, read as money. `null` is unknown, not zero. */
export function formatMicroUsd(micro: number | null | undefined): string {
  if (micro == null) {
    return "—";
  }
  return `$${(micro / 1_000_000).toFixed(2)}`;
}
