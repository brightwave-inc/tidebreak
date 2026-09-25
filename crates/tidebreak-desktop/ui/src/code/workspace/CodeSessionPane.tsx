import type { ApiClient } from "../../api/client";
import { ArrowDown } from "lucide-react";
import type {
  CodeApprovalSnapshot,
  CodeApprovalDecision,
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
  retryCodeSessionConnection,
} from "../CodeSessionRegistry";
import { ConnectionStatus } from "@/ConnectionNotice";
import {
  applySessionTreeSnapshot,
  applyTurnRewrite,
  mainAgentTranscriptItems,
  subagentTranscriptItems,
} from "../CodeSessionReducer";
import { CodeSessionTree } from "./CodeSessionTree";
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
import { splitReviewComments } from "../diff/reviewComments";
import { sendCodeTurn, turnIdNamed } from "../CodeSessionSend";
import { toast } from "sonner";
import { useCodeUpdatesStore, useSessionDigest } from "../CodeUpdatesStore";
import { useStreamStalled } from "@/useStreamStalled";
import { useTranscriptFollow } from "@/useTranscriptFollow";
import { EarlierHistoryNotice } from "@/search/EarlierHistoryNotice";
import { TranscriptFindBar } from "@/search/TranscriptFindBar";
import { TranscriptFindOverlay } from "@/search/TranscriptFindOverlay";
import { useCodeTranscriptSearch } from "@/search/useCodeTranscriptSearch";

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
  onOpenWorkspaceDiff,
  onForkFromTurn,
  onRestoreBeforeTurn,
  onUndoRestore,
  undoUnavailableReason,
  onFileIssue,
  subagentCallId,
  subagentSummary,
  onBackFromSubagent,
  composerOverride,
}: {
  session: CodeSessionSnapshot;
  workspaceId?: string;
  client: ApiClient;
  catalogModels: ModelInfo[];
  defaultModelKey: string | null;
  disabled: boolean;
  /** Scope the review sidebar to one turn's changes, from a turn's diffstat. */
  onOpenTurnDiff?: (turnId: string) => void;
  /** Open the workspace diff, from a review row whose findings are there. */
  onOpenWorkspaceDiff?: () => void;
  /** Fork this conversation at the end of one turn, from its seam row. */
  onForkFromTurn?: (turnId: string) => void;
  /** Put the worktree back to before one turn, from its seam row. */
  onRestoreBeforeTurn?: (turnId: string) => void;
  /** Put back what one restore replaced, from the restore's own row. */
  onUndoRestore?: (restoreId: string) => void;
  /** Why the worktree cannot be changed right now, such as a running turn. */
  undoUnavailableReason?: string;
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
  const canContribute = session.access !== "view";
  const canManage = session.is_owner !== false;
  const follow = useTranscriptFollow();
  const store = useRegisteredCodeSession(session.id, client);
  const firstTurnRecovery = useFirstTurnRecovery(client, session.id);
  const items = store((state) => state.items);
  // The first accepted turn carried the fork's transcript, so the chip that
  // waited beside a refused first message is done.
  useEffect(() => {
    if (!firstTurnRecovery) return;
    const sent = items.some((item) => item.kind === "user");
    if (!sent) return;
    clearFirstTurnRecovery(client, session.id, firstTurnRecovery.id);
  }, [client, firstTurnRecovery, items, session.id]);
  const treeChildren = store((state) => state.children);
  const treeWait = store((state) => state.wait);
  useEffect(() => {
    store.getState().update((state) => {
      if (state.lastSeq > 0) return state;
      return applySessionTreeSnapshot(state, session);
    });
  }, [session, store]);
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
  // Find in this session, and the palette's jumps into it. An event older
  // than the journal this transcript replayed opens its own window of it.
  const transcriptSearch = useCodeTranscriptSearch({
    client,
    sessionId: session.id,
    shared: session.is_owner === false,
    hydrated,
    items: transcriptItems,
    scrollElement: follow.scrollElement,
    pauseFollow: follow.pauseFollow,
  });
  const history = transcriptSearch.history;
  const findBar = transcriptSearch.find;
  const historyItems = useMemo(
    () => (history ? mainAgentTranscriptItems(history.items) : null),
    [history],
  );
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
  const [approvalErrorId, setApprovalErrorId] = useState<string | null>(null);
  const queuedTurnFor = useCallback(
    (diff: string) => turnIdNamed(session.id, diff),
    [session.id],
  );
  const sessionQueue = useCodeQueueApi(
    client,
    session.id,
    workspaceId,
    queuedTurnFor,
  );
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
  // Recall brings back what the reader typed. Diff comments went with the
  // message, but they were written on the diff and are not typed again.
  const composerHistory = useMemo(
    () =>
      items
        .flatMap((item) => {
          if (item.kind !== "user") return [];
          const typed = splitReviewComments(item.text).prose;
          return typed.trim() ? [typed] : [];
        })
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
      decision: CodeApprovalDecision,
      feedback?: string,
    ) => {
      setDecidingId(approvalId);
      setApprovalErrorId(approvalId);
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
    const pendingReasoningEffort = canManage
      ? pendingReasoningEffortRef.current
      : null;
    const requestedModel = canManage ? (model ?? undefined) : undefined;
    const recoveryAtSend = firstTurnRecovery;
    // Sending is a deliberate return to the tail: whatever the reader was
    // reading, an earlier stretch a search opened included, they now want to
    // watch their own turn run and decide what it asks.
    transcriptSearch.leaveHistory();
    follow.armFollow();
    follow.requestSmoothFollow();
    // Outcome and refusal both belong to the composer: it says whether the
    // message ran or queued, and it holds the draft when the server refuses.
    // The server answers once the message is accepted, so this settles long
    // before the turn does.
    return sendCodeTurn({
      client,
      sessionId: session.id,
      message,
      attachments,
      model: requestedModel,
      ...(pendingReasoningEffort
        ? { reasoningEffort: pendingReasoningEffort.value }
        : {}),
    }).then((outcome) => {
      if (pendingReasoningEffortRef.current === pendingReasoningEffort) {
        pendingReasoningEffortRef.current = null;
      }
      if (recoveryAtSend) {
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
      throw new Error("The active turn changed. Try steer again.");
    }
    // A steer lands in the running turn, at the tail.
    transcriptSearch.leaveHistory();
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
          origins={session.external_origins}
          executionLocation={session.execution_location}
          actsAs={session.acts_as}
        />
      )}
      {!subagentCallId && (
        <CodeSessionTree nodes={treeChildren} wait={treeWait} />
      )}
      <div
        className={cn("message-view", follow.fadeClass)}
        onFocusCapture={findBar.activate}
        onPointerDownCapture={findBar.activate}
      >
        {findBar.open && (
          <TranscriptFindOverlay scrollElement={follow.scrollElement}>
            <TranscriptFindBar
              ref={findBar.inputRef}
              className="pointer-events-auto"
              query={findBar.query}
              onQueryChange={findBar.setQuery}
              state={findBar.state}
              onOlder={findBar.older}
              onNewer={findBar.newer}
              onClose={findBar.close}
            />
          </TranscriptFindOverlay>
        )}
        <CodeTranscript
          key={historyItems ? "history" : "live"}
          items={historyItems ?? transcriptItems}
          sessionId={session.id}
          hydrated={hydrated}
          busy={historyItems ? false : transcriptBusy}
          streamStalled={historyItems ? false : streamStalled}
          animateStreaming={historyItems ? false : animateStreaming}
          approvals={approvals}
          decidingId={decidingId}
          approvalError={approvalError}
          approvalErrorId={approvalErrorId}
          onOpenTurnDiff={onOpenTurnDiff}
          onOpenWorkspaceDiff={onOpenWorkspaceDiff}
          onForkFromTurn={
            subagentCallId || historyItems ? undefined : onForkFromTurn
          }
          onRestoreBeforeTurn={
            subagentCallId || historyItems ? undefined : onRestoreBeforeTurn
          }
          onUndoRestore={
            subagentCallId || historyItems ? undefined : onUndoRestore
          }
          undoUnavailableReason={undoUnavailableReason}
          onFileIssue={subagentCallId ? undefined : onFileIssue}
          onReveal={follow.pauseFollow}
          scrollRef={follow.scrollRef}
          contentRef={follow.contentRef}
          onScroll={follow.onScroll}
          onDecide={canContribute && !historyItems ? decideApproval : undefined}
          recap={historyItems ? undefined : sessionDigest?.recap}
          revealItemId={transcriptSearch.revealItemId}
          trailingNotice={
            historyItems ? (
              <EarlierHistoryNotice
                label="This is an earlier part of the conversation. Newer activity is not shown here."
                onLeave={() => {
                  transcriptSearch.leaveHistory();
                  follow.armFollow(followScrollBehavior(false));
                }}
              />
            ) : undefined
          }
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
      {/* The same notice a conversation shows above its composer, so a
          dropped session socket reads the same in both halves of the app. */}
      <div className="shrink-0 px-[clamp(0.5rem,4%,5rem)] empty:hidden">
        <ConnectionStatus
          connection={connectionState}
          onRetryNow={() => retryCodeSessionConnection(session.id)}
          // As wide as the composer it sits on, like the queue tray.
          className="mx-auto mb-2 w-full max-w-3xl"
        />
      </div>
      {composerOverride}
      {!canContribute && !composerOverride && (
        <p className="text-muted-foreground border-t border-border px-4 py-3 text-sm">
          You have view access to this conversation.
        </p>
      )}
      {canContribute &&
        lifecycle !== "ended" &&
        !composerOverride &&
        !subagentCallId && (
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
              disabled={disabled}
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
              promptScope={workspaceId ?? session.id}
              sessionId={session.id}
              reviewWorkspaceId={workspaceId}
              history={composerHistory}
              slashCommands={doctorEntry?.commands}
              searchPaths={
                workspaceId
                  ? (query) =>
                      client
                        .listCodeWorkspaceTree(workspaceId, { query })
                        .then((tree) => tree.paths)
                  : undefined
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
              onModelChange={canManage ? setModel : undefined}
              onModeChange={
                !canManage ||
                (doctorEntry?.relaunch_composes_permission_mode === false &&
                  session.harness_resume_ref)
                  ? undefined
                  : changePermissionMode
              }
              onEffortChange={
                !canManage ||
                doctorEntry?.caps.reasoning_levels === "unsupported"
                  ? undefined
                  : changeReasoningEffort
              }
              onFastModeChange={canManage ? changeFastMode : undefined}
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
          </>
        )}
    </div>
  );
}

export function useRegisteredCodeSession(sessionId: string, client: ApiClient) {
  type Registration = {
    sessionId: string;
    client: ApiClient;
    store: ReturnType<typeof acquireCodeSessionFromClient>;
  };
  const registrationRef = useRef<Registration | null>(null);
  // Acquire during render so the first paint already has the store. Every
  // acquire is released exactly once by the effect cleanup that closes over
  // it below, so a key change never releases the registration that replaced
  // it, and StrictMode's simulated unmount/remount (release, then rerun)
  // reacquires instead of rendering from a disposed controller.
  const current = registrationRef.current;
  if (current?.sessionId !== sessionId || current.client !== client) {
    registrationRef.current = {
      sessionId,
      client,
      store: acquireCodeSessionFromClient(sessionId, client),
    };
  }
  useEffect(() => {
    let registration = registrationRef.current;
    if (
      registration === null ||
      registration.sessionId !== sessionId ||
      registration.client !== client
    ) {
      registration = {
        sessionId,
        client,
        store: acquireCodeSessionFromClient(sessionId, client),
      };
      registrationRef.current = registration;
    }
    const owned = registration;
    return () => {
      releaseCodeSession(owned.sessionId);
      if (registrationRef.current === owned) registrationRef.current = null;
    };
  }, [sessionId, client]);
  return registrationRef.current!.store;
}
