import type { ApiClient } from "../../api/client";
import { ArrowDown } from "lucide-react";
import type {
  CodeApprovalSnapshot,
  CodeSessionSnapshot,
  CodeSubagentSummary,
  ModelInfo,
  PermissionMode,
  ReasoningEffort,
} from "../../api/types";
import { CodeComposer } from "../CodeComposer";
import { CodeTranscript } from "../CodeTranscript";
import { FOCUS_RING } from "../interactive";
import { QueueTray, useCodeQueueApi } from "@/QueueTray";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { SessionOriginBanner } from "../SessionOriginBanner";
import {
  SubagentContextBar,
  subagentEmptyState,
  subagentSummaryFromTranscript,
} from "./subagents";
import {
  acquireCodeSessionFromClient,
  releaseCodeSession,
} from "../CodeSessionRegistry";
import {
  applyTurnRewrite,
  mainAgentTranscriptItems,
  subagentTranscriptItems,
} from "../CodeSessionReducer";
import {
  clearFirstTurnRecovery,
  updateFirstTurnRecovery,
  useFirstTurnRecovery,
} from "./firstTurnRecovery";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import {
  codeModelsFromHarnessListing,
  useCodeCatalogStore,
} from "../CodeCatalogStore";
import {
  createPermissionModes,
  gatewayCodeModels,
  harnessCodeModels,
  preferredCodeModels,
  requiresHarnessModelIds,
} from "../labels";
import { followScrollBehavior } from "@/ChatScroll";
import { forkTranscriptFile } from "../fork";
import { submitAcceptedTurn } from "../CodeSessionSend";
import { toast } from "sonner";
import { useCodeUpdatesStore, useSessionDigest } from "../CodeUpdatesStore";
import { useRefreshSignals } from "@/RefreshSignals";
import { useStreamStalled } from "@/useStreamStalled";
import { useTranscriptFollow } from "@/useTranscriptFollow";

/**
 * Stable empty ladder. A fresh `[]` per render is a new snapshot every time,
 * and zustand v5 loops on referentially unstable selector results.
 */
const EMPTY_EFFORTS: readonly ReasoningEffort[] = [];

export function CodeSessionPane({
  session,
  workspaceId,
  client,
  catalogModels,
  defaultModelKey,
  disabled,
  onOpenTurnDiff,
  onForkFromTurn,
  onFileIssue,
  subagentCallId,
  subagentSummary,
  onBackFromSubagent,
  composerOverride,
}: {
  session: CodeSessionSnapshot;
  workspaceId: string;
  client: ApiClient;
  catalogModels: ModelInfo[];
  defaultModelKey: string | null;
  disabled: boolean;
  /** Scope the review sidebar to one turn's changes, from a turn's diffstat. */
  onOpenTurnDiff?: (turnId: string) => void;
  /** Fork this conversation at the end of one turn, from its seam row. */
  onForkFromTurn?: (turnId: string) => void;
  /** Turn a failed turn or engine error into a Tidebreak issue or fix. */
  onFileIssue?: () => void;
  /** The spanning Task call to inspect inside this still-mounted session. */
  subagentCallId?: string;
  /** Current bounded rail summary, when the Task is still in the digest. */
  subagentSummary?: CodeSubagentSummary;
  onBackFromSubagent?: () => void;
  /**
   * Replace the composer. A watch task's transcript is read-along: the sweep
   * drives its turns, so the seat where the user would type carries the watch
   * controls instead.
   */
  composerOverride?: ReactNode;
}) {
  const follow = useTranscriptFollow();
  const store = useRegisteredCodeSession(session.id, client);
  const firstTurnRecovery = useFirstTurnRecovery(client, session.id);
  const items = store((state) => state.items);
  const busy = store((state) => state.busy);
  const hydrated = store((state) => state.hydrated);
  const animateStreaming = store((state) => state.animateStreaming);
  const connectionState = store((state) => state.connectionState);
  const lastUsage = store((state) => state.lastUsage);
  // The reducer's own applied-event cursor is the activity signal the stall
  // timer wants: every delta, tool result, and boundary advances it.
  const lastSeq = store((state) => state.lastSeq);
  const transcriptSubagent = useMemo(
    () =>
      subagentCallId
        ? subagentSummaryFromTranscript(items, subagentCallId)
        : null,
    [items, subagentCallId],
  );
  const selectedSubagent = subagentCallId
    ? (subagentSummary ?? transcriptSubagent)
    : null;
  const turnRewrites = useCodeUpdatesStore(
    (state) => state.turnRewrites[session.id],
  );
  const transcriptItems = useMemo(() => {
    const base = subagentCallId
      ? subagentTranscriptItems(items, subagentCallId)
      : mainAgentTranscriptItems(items);
    if (!turnRewrites) return base;
    let next = base;
    for (const [turnId, notice] of Object.entries(turnRewrites)) {
      const stored = next.some(
        (item) =>
          item.kind === "assistant" &&
          item.turnId === turnId &&
          item.parentCallId === null &&
          Boolean(item.rewrite),
      );
      if (stored && notice.state !== "rewritten") continue;
      next = applyTurnRewrite(next, turnId, {
        rewrite: notice.rewrite,
        rewriteState: notice.state,
      });
    }
    return next;
  }, [items, subagentCallId, turnRewrites]);
  const transcriptBusy = subagentCallId
    ? selectedSubagent?.status === "running"
    : busy;
  const streamStalled = useStreamStalled(transcriptBusy, lastSeq);
  const storeLifecycle = store((state) => state.lifecycle);
  // Archive search opens ended sessions. Hydration then writes idle because
  // the journal has no ended event, and that would resurrect the composer.
  const lifecycle =
    session.lifecycle === "ended"
      ? "ended"
      : (storeLifecycle ?? session.lifecycle);
  const [approvals, setApprovals] = useState<
    Record<string, CodeApprovalSnapshot>
  >({});
  const [decidingId, setDecidingId] = useState<string | null>(null);
  const [approvalError, setApprovalError] = useState<string | undefined>();
  const sessionQueue = useCodeQueueApi(client, session.id);
  // No `?? []` fallback here: a fresh array is a new snapshot every render,
  // and zustand v5 loops on referentially unstable snapshots.
  const cachedModels = useCodeCatalogStore(
    (state) => state.modelsByHarness[session.harness_kind],
  );
  const rememberHarnessModels = useCodeCatalogStore(
    (state) => state.rememberHarnessModels,
  );
  // The ladder a code session runs on belongs to the engine, not to whichever
  // catalog the model row came from.
  const engineEfforts =
    useCodeCatalogStore(
      (state) => state.effortsByHarness[session.harness_kind],
    ) ?? EMPTY_EFFORTS;
  const modelOptions = useMemo(() => {
    const gateway = gatewayCodeModels(
      catalogModels,
      session.harness_kind,
      defaultModelKey,
    );
    const listed =
      requiresHarnessModelIds(session.harness_kind) &&
      cachedModels === undefined
        ? []
        : preferredCodeModels(
            session.harness_kind,
            cachedModels ?? [],
            gateway,
          );
    if (
      !session.model ||
      listed.some((option) => option.id === session.model)
    ) {
      return listed;
    }
    // Historical or engine-default sessions can name a model that is hidden
    // from today's catalog. Keep that truthful current model visible instead
    // of silently labeling the session as whichever row is now default.
    return [
      ...harnessCodeModels(
        [{ id: session.model, label: session.model }],
        session.harness_kind,
      ),
      ...listed,
    ];
  }, [
    cachedModels,
    catalogModels,
    defaultModelKey,
    session.harness_kind,
    session.model,
  ]);
  const inferred = modelOptions.find((option) => option.default)?.id;
  const [model, setModel] = useState(session.model ?? inferred);
  // The recap is derived after a turn completes and published on the digest
  // channel rather than the journal, so the transcript reads it from here
  // instead of from an item the reducer built.
  const sessionDigest = useSessionDigest(workspaceId, session.id);
  type SessionSettings = {
    permissionMode: PermissionMode;
    reasoningEffort: ReasoningEffort | null;
    fastMode: boolean;
  };
  const settingsFromSession = useCallback(
    (snapshot: CodeSessionSnapshot): SessionSettings => ({
      permissionMode: snapshot.permission_mode,
      reasoningEffort: snapshot.reasoning_effort ?? null,
      fastMode: snapshot.fast_mode,
    }),
    [],
  );
  const initialSettings: SessionSettings = {
    permissionMode: session.permission_mode,
    reasoningEffort: session.reasoning_effort ?? null,
    fastMode: session.fast_mode,
  };
  // One confirmed baseline plus ordered optimistic patches keeps a full
  // response from an older write from erasing a choice that is still queued.
  const [settings, setSettings] = useState(initialSettings);
  const settingsRef = useRef(initialSettings);
  const confirmedSettingsRef = useRef(initialSettings);
  const pendingSettingsWritesRef = useRef(
    new Map<number, Partial<SessionSettings>>(),
  );
  const settingsWriteQueueRef = useRef<Promise<void>>(Promise.resolve());
  const settingsWriteGenerationRef = useRef(0);
  const settingsScopeRef = useRef(session.id);
  const [settingsPending, setSettingsPending] = useState(false);
  const pendingReasoningEffortRef = useRef<{
    value: ReasoningEffort | null;
  } | null>(null);
  const reconcileSettings = useCallback(() => {
    let next = { ...confirmedSettingsRef.current };
    for (const patch of pendingSettingsWritesRef.current.values()) {
      next = { ...next, ...patch };
    }
    const pendingReasoningEffort = pendingReasoningEffortRef.current;
    if (pendingReasoningEffort) {
      next.reasoningEffort = pendingReasoningEffort.value;
    }
    settingsRef.current = next;
    setSettings(next);
  }, []);

  function queueSettingsWrite(
    patch: Partial<SessionSettings>,
    write: () => Promise<CodeSessionSnapshot>,
    failureMessage: string,
  ) {
    const scope = session.id;
    const generation = ++settingsWriteGenerationRef.current;
    pendingSettingsWritesRef.current.set(generation, patch);
    reconcileSettings();
    setSettingsPending(true);

    const result = settingsWriteQueueRef.current.then(() => {
      if (settingsScopeRef.current !== scope) return null;
      return write();
    });
    settingsWriteQueueRef.current = result.then(
      () => undefined,
      () => undefined,
    );
    void result.then(
      (updated) => {
        if (!updated || settingsScopeRef.current !== scope) return;
        confirmedSettingsRef.current = settingsFromSession(updated);
        pendingSettingsWritesRef.current.delete(generation);
        reconcileSettings();
        setSettingsPending(pendingSettingsWritesRef.current.size > 0);
      },
      (err) => {
        if (settingsScopeRef.current !== scope) return;
        pendingSettingsWritesRef.current.delete(generation);
        reconcileSettings();
        setSettingsPending(pendingSettingsWritesRef.current.size > 0);
        toast.error(friendlyErrorMessage(err, failureMessage));
      },
    );
  }

  useEffect(() => {
    setModel(session.model ?? inferred);
  }, [inferred, session.model]);

  useEffect(() => {
    if (settingsScopeRef.current !== session.id) {
      settingsScopeRef.current = session.id;
      settingsWriteGenerationRef.current += 1;
      pendingSettingsWritesRef.current.clear();
      settingsWriteQueueRef.current = Promise.resolve();
      pendingReasoningEffortRef.current = null;
      setSettingsPending(false);
    }
    // A refreshed row can still carry the stored effort while a mid-turn
    // choice waits for its first submission. Reconciliation keeps that choice
    // and any queued writes on top of the confirmed row.
    confirmedSettingsRef.current = settingsFromSession(session);
    reconcileSettings();
  }, [
    reconcileSettings,
    session.id,
    session.permission_mode,
    session.reasoning_effort,
    session.fast_mode,
    settingsFromSession,
  ]);

  useEffect(() => {
    // An empty list is a finished fetch: this engine advertised no models.
    // Treating [] as "not yet loaded" remembers a new [] forever.
    //
    // The fetch runs even when a gateway catalog already supplies the rows,
    // because this route is also where the engine's effort ladder comes from
    // and a gateway row carries the chat catalog's instead.
    if (cachedModels !== undefined) return;
    let cancelled = false;
    void client.listCodeHarnessModels(session.harness_kind).then(
      (listed) => {
        if (cancelled) return;
        rememberHarnessModels(
          session.harness_kind,
          codeModelsFromHarnessListing(listed, session.harness_kind),
          listed.reasoning_efforts,
        );
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [cachedModels, client, rememberHarnessModels, session.harness_kind]);
  const doctorEntry = useCodeCatalogStore(
    (state) =>
      state.doctor?.harnesses.find(
        (entry) => entry.kind === session.harness_kind,
      ) ?? null,
  );
  // Doctor caps decide what this engine's picker offers; without a doctor
  // row yet, show everything and let the server refuse.
  const availableModes: PermissionMode[] = doctorEntry
    ? createPermissionModes(doctorEntry.caps)
    : ["plan", "ask", "auto", "allow"];
  const steeringSupported = doctorEntry?.caps.mid_turn_steering === "supported";
  const turnRunning = busy || lifecycle === "running";
  const composerHistory = useMemo(
    () =>
      items
        .flatMap((item) =>
          item.kind === "user" && item.text.trim() ? [item.text] : [],
        )
        .reverse(),
    [items],
  );

  // `items` is a fresh array on every streamed delta, so keying the fetch on it
  // would list approvals again for every token of a turn. Only an approval
  // appearing or changing state can change what the list would return.
  const approvalKey = useMemo(
    () =>
      items
        .filter((item) => item.kind === "approval")
        .map((item) => `${item.approvalId}:${item.state}`)
        .join(","),
    [items],
  );

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const rows = await client.listCodeApprovals({ sessionId: session.id });
        if (cancelled) return;
        const next: Record<string, CodeApprovalSnapshot> = {};
        for (const row of rows) next[row.id] = row;
        setApprovals(next);
      } catch {
        // The journal still surfaces the card; the body loads on the next poll.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, session.id, approvalKey]);

  // This pane re-renders on every streamed delta, so a callback written inline
  // in the transcript's props would be a new identity each time and would
  // re-render every row in the transcript with it.
  const decideApproval = useCallback(
    async (
      approvalId: string,
      decision: "approve" | "deny",
      feedback?: string,
    ) => {
      setDecidingId(approvalId);
      setApprovalError(undefined);
      try {
        const next = await client.decideCodeApproval(approvalId, {
          decision,
          feedback,
        });
        setApprovals((current) => ({ ...current, [approvalId]: next }));
      } catch (err) {
        setApprovalError(
          friendlyErrorMessage(err, "Could not record that decision"),
        );
      } finally {
        setDecidingId(null);
      }
    },
    [client],
  );

  function send(
    message: string,
    attachments?: readonly { blob_id: string; media_type: string }[],
  ) {
    const pendingReasoningEffort = pendingReasoningEffortRef.current;
    const recoveryAtSend = firstTurnRecovery;
    // Sending is a deliberate return to the tail: whatever the reader was
    // reading, they now want to watch their own turn run.
    follow.armFollow();
    follow.requestSmoothFollow();
    // Outcome and refusal both belong to the composer: it says whether the
    // message ran or queued, and it holds the draft when the server refuses.
    // A queued outcome needs no state here — the tray reads the durable queue
    // on the signal below and shows the row.
    return submitAcceptedTurn(store.getState().update, () =>
      pendingReasoningEffort
        ? client.submitCodeTurn(
            session.id,
            message,
            model ?? undefined,
            attachments,
            pendingReasoningEffort.value,
          )
        : client.submitCodeTurn(
            session.id,
            message,
            model ?? undefined,
            attachments,
          ),
    ).then((outcome) => {
      if (outcome.kind === "queued") {
        useRefreshSignals.getState().signal("queuedTurns");
      }
      if (pendingReasoningEffortRef.current === pendingReasoningEffort) {
        pendingReasoningEffortRef.current = null;
      }
      if (recoveryAtSend?.status === "failed") {
        clearFirstTurnRecovery(client, session.id, recoveryAtSend.id);
      }
      return outcome;
    });
  }

  function changePermissionMode(mode: PermissionMode) {
    queueSettingsWrite(
      { permissionMode: mode },
      () => client.setCodeSessionPermissionMode(session.id, mode),
      "Could not change the mode",
    );
  }

  function changeReasoningEffort(effort: ReasoningEffort | null) {
    // A running turn keeps the effort it started with. The selected level
    // rides on the next submission, where the server also makes it sticky.
    if (turnRunning) {
      pendingReasoningEffortRef.current = { value: effort };
      settingsRef.current = { ...settingsRef.current, reasoningEffort: effort };
      setSettings(settingsRef.current);
      return;
    }
    pendingReasoningEffortRef.current = null;
    queueSettingsWrite(
      { reasoningEffort: effort },
      () => client.setCodeSessionReasoningEffort(session.id, effort),
      "Could not change the reasoning",
    );
  }

  function changeFastMode(fastMode: boolean) {
    queueSettingsWrite(
      { fastMode },
      () => client.setCodeSessionFastMode(session.id, fastMode),
      "Could not change fast mode",
    );
  }

  async function steer(message: string) {
    const expectedTurnId = store.getState().activeTurnId;
    if (!expectedTurnId) {
      throw new Error("The active turn changed. Try Redirect again.");
    }
    await client.steerCodeSession(session.id, expectedTurnId, message);
  }

  async function interrupt() {
    try {
      await client.interruptCodeSession(session.id);
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not interrupt"));
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {subagentCallId && (
        <SubagentContextBar
          name={selectedSubagent?.name ?? "Subagent unavailable"}
          status={selectedSubagent?.status ?? "unavailable"}
          onBack={onBackFromSubagent}
        />
      )}
      {!subagentCallId && session.external_origin && (
        <SessionOriginBanner
          origin={session.external_origin}
          executionLocation={session.execution_location}
        />
      )}
      <div className={cn("message-view", follow.fadeClass)}>
        {connectionState === "reconnecting" && (
          <p
            role="status"
            className="text-info-foreground pointer-events-none absolute inset-x-0 top-2 z-[1] text-center text-xs [animation:code-reveal_140ms_ease-out] motion-reduce:animate-none"
          >
            Reconnecting to the session…
          </p>
        )}
        <CodeTranscript
          items={transcriptItems}
          sessionId={session.id}
          hydrated={hydrated}
          busy={transcriptBusy}
          streamStalled={streamStalled}
          animateStreaming={animateStreaming}
          approvals={approvals}
          decidingId={decidingId}
          approvalError={approvalError}
          onOpenTurnDiff={onOpenTurnDiff}
          onForkFromTurn={subagentCallId ? undefined : onForkFromTurn}
          onFileIssue={subagentCallId ? undefined : onFileIssue}
          onReveal={follow.pauseFollow}
          scrollRef={follow.scrollRef}
          contentRef={follow.contentRef}
          onScroll={follow.onScroll}
          onDecide={decideApproval}
          recap={sessionDigest?.recap}
          emptyState={
            subagentCallId
              ? subagentEmptyState(selectedSubagent?.status)
              : undefined
          }
        />
        <button
          type="button"
          className={cn(
            "border-border text-foreground bg-background hover:bg-accent pointer-events-none absolute bottom-3 left-1/2 z-[1] inline-flex -translate-x-1/2 cursor-pointer items-center justify-center rounded-full border p-2 opacity-0 shadow transition-[opacity,background-color] duration-[140ms] ease-out motion-reduce:transition-none",
            FOCUS_RING,
            follow.scrolledAway && "pointer-events-auto opacity-100",
          )}
          aria-label="Scroll to latest"
          aria-hidden={!follow.scrolledAway}
          tabIndex={follow.scrolledAway ? 0 : -1}
          onClick={() => follow.armFollow(followScrollBehavior(false))}
        >
          <ArrowDown size={16} />
        </button>
      </div>
      {composerOverride}
      {lifecycle !== "ended" && !composerOverride && !subagentCallId && (
        <>
          <div className="shrink-0 px-[clamp(0.5rem,4%,5rem)]">
            <QueueTray
              queue={sessionQueue}
              active={turnRunning}
              onStop={interrupt}
            />
          </div>
          <CodeComposer
            running={turnRunning}
            disabled={disabled || firstTurnRecovery?.status === "sending"}
            permissionMode={settings.permissionMode}
            availableModes={availableModes}
            reasoningEffort={settings.reasoningEffort}
            fastMode={settings.fastMode}
            settingsPending={settingsPending}
            engineEfforts={engineEfforts}
            harness={session.harness_kind}
            model={model ?? undefined}
            modelOptions={modelOptions}
            modelLoading={
              requiresHarnessModelIds(session.harness_kind) &&
              cachedModels === undefined
            }
            promptScope={workspaceId}
            sessionId={session.id}
            history={composerHistory}
            slashCommands={doctorEntry?.commands}
            searchPaths={(query) =>
              client
                .listCodeWorkspaceTree(workspaceId, { query })
                .then((tree) => tree.paths)
            }
            workspaceFiles={
              firstTurnRecovery?.forkSource
                ? {
                    items: [forkTranscriptFile(firstTurnRecovery.forkSource)],
                    onRemove: () =>
                      updateFirstTurnRecovery(
                        client,
                        session.id,
                        firstTurnRecovery.id,
                        (current) => ({ ...current, forkSource: null }),
                      ),
                  }
                : undefined
            }
            recovery={
              firstTurnRecovery
                ? {
                    id: firstTurnRecovery.id,
                    draft: firstTurnRecovery.draft,
                  }
                : undefined
            }
            onModelChange={setModel}
            onModeChange={
              doctorEntry?.relaunch_composes_permission_mode === false &&
              session.harness_resume_ref
                ? undefined
                : changePermissionMode
            }
            onEffortChange={
              doctorEntry?.caps.reasoning_levels === "unsupported"
                ? undefined
                : changeReasoningEffort
            }
            onFastModeChange={changeFastMode}
            contextUsage={
              lastUsage
                ? {
                    // The engine's own reading of the prompt still resident
                    // after its last model call. The four counts below are the
                    // turn's spend across every call, which on a long turn runs
                    // to several times this.
                    contextTokens: lastUsage.context_tokens,
                    spend: {
                      input: lastUsage.input_tokens,
                      output: lastUsage.output_tokens,
                      cacheRead: lastUsage.cache_read_input_tokens,
                      cacheWrite: lastUsage.cache_creation_input_tokens,
                    },
                    contextWindow: catalogModels.find(
                      (entry) => entry.id === model || entry.key === model,
                    )?.context_window,
                    modelName:
                      modelOptions.find((option) => option.id === model)
                        ?.label ??
                      model ??
                      undefined,
                  }
                : null
            }
            onSend={send}
            onSteer={steeringSupported ? steer : undefined}
            onInterrupt={interrupt}
          />
          {firstTurnRecovery && (
            <p
              role={firstTurnRecovery.status === "failed" ? "alert" : "status"}
              className={cn(
                "mx-auto w-full max-w-3xl px-2 pt-1 text-xs",
                firstTurnRecovery.status === "failed"
                  ? "text-critical-foreground"
                  : "text-muted-foreground",
              )}
            >
              {firstTurnRecovery.message}
            </p>
          )}
        </>
      )}
    </div>
  );
}

export function useRegisteredCodeSession(sessionId: string, client: ApiClient) {
  const storeRef = useRef<ReturnType<
    typeof acquireCodeSessionFromClient
  > | null>(null);
  if (storeRef.current === null) {
    storeRef.current = acquireCodeSessionFromClient(sessionId, client);
  }
  useEffect(() => {
    return () => {
      releaseCodeSession(sessionId);
      storeRef.current = null;
    };
  }, [sessionId, client]);
  return storeRef.current;
}
