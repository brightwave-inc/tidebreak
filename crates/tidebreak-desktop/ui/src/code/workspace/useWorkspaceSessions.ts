import type { ApiClient } from "../../api/client";
import type { ModelInfo } from "../../api/types";
import type {
  CodeForkTranscript,
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
  PermissionMode,
  ReasoningEffort,
} from "../../api/types";
import type { CodeConversationTab } from "../CodeCenterTabs";
import {
  clearFirstTurnRecovery,
  type FirstTurnRecovery,
  writeFirstTurnRecovery,
} from "./firstTurnRecovery";
import { attentionMarkForDigest } from "../statusTone";
import { conversationTabLabel } from "./layout";
import { forkFraming } from "../fork";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  gatewayCodeModels,
  preferredCodeModels,
  requiresHarnessModelIds,
} from "../labels";
import { hasLocalHostAuthority } from "../../host";
import { liveCodeSessions } from "../parsers";
import { publishCodeImage } from "../../attachments";
import { toast } from "sonner";
import { uploadImageAttachment } from "../../ImageAttachments";
import { useCodeCatalogStore } from "../CodeCatalogStore";
import { useCodeUiStore } from "../CodeUiStore";
import { useConversationDigests } from "../CodeUpdatesStore";
import { useEffect, useMemo, useRef, useState } from "react";
import type { useNavigate } from "@tanstack/react-router";

/**
 * The agents one workspace runs, and which of them the page shows.
 *
 * A workspace runs several agents (record 55), so the page holds the list
 * and picks one to show rather than tracking a single session. Selection
 * lives in `?task=` so a reload or a shared link returns to the same agent;
 * the first agent is the workspace's default and stays unnamed.
 *
 * Starting an agent is the one multi-step write here: create the session,
 * hand it any images the composer held, then send the first message. Each
 * step checks that the page, the client, and the reader's selection are
 * still the ones the start began with, because a connection change or a
 * click on another tab mid-flight must not land a stranger's result.
 */
export function useWorkspaceSessions({
  workspaceId,
  client,
  models,
  defaultModelKey,
  taskParam,
  navigate,
  focusConversationPane,
}: {
  workspaceId: string;
  client: ApiClient;
  models: ModelInfo[];
  defaultModelKey: string | null;
  /** `?task=` from the route: the session the link names, if any. */
  taskParam: string | undefined;
  navigate: ReturnType<typeof useNavigate>;
  /** Bring the conversation tab to the front of the center strip. */
  focusConversationPane: () => void;
}) {
  const clientRef = useRef(client);
  clientRef.current = client;
  const catalog = useCodeCatalogStore();
  const mountedRef = useRef(true);
  const selectionRevisionRef = useRef(0);
  const startRequestRef = useRef(0);
  const catalogWorkspace = catalog.workspaces.find(
    (candidate) => candidate.id === workspaceId,
  );
  const [workspace, setWorkspace] = useState<CodeWorkspaceSnapshot | null>(
    () => catalogWorkspace ?? null,
  );
  const catalogRepo = catalogWorkspace
    ? catalog.repos.find(
        (candidate) => candidate.id === catalogWorkspace.repo_id,
      )
    : undefined;
  const [repo, setRepo] = useState<CodeRepoSnapshot | null>(
    () => catalogRepo ?? null,
  );
  /**
   * Every session the server knows about here, conversations and watches alike.
   */
  const [sessions, setSessions] = useState<CodeSessionSnapshot[]>(() => {
    const remembered = catalog.sessionsByWorkspace[workspaceId];
    return remembered ? [remembered] : [];
  });
  const [activeSessionId, setActiveSessionId] = useState<string | null>(
    catalog.sessionsByWorkspace[workspaceId]?.id ?? null,
  );
  const [closedConversationIds, setClosedConversationIds] = useState<
    Set<string>
  >(() => new Set());

  // Optimistic catalog mutations also govern the open page. Archive can hide
  // the rail card before filesystem cleanup finishes; reflecting that same
  // snapshot here stops the composer from offering work against a checkout
  // that is being removed. A failed request puts the original snapshot back.
  useEffect(() => {
    if (!catalogWorkspace) return;
    setWorkspace((current) =>
      current?.id === catalogWorkspace.id ? catalogWorkspace : current,
    );
  }, [catalogWorkspace]);

  useEffect(() => {
    if (!catalogRepo) return;
    setRepo((current) =>
      current?.id === catalogRepo.id ? catalogRepo : current,
    );
  }, [catalogRepo]);
  /** True while the reader is filling in a new agent that has no session yet. */
  const [draftAgent, setDraftAgent] = useState(false);
  /**
   * True once the server's session list has arrived for this workspace.
   *
   * `?task=` can only be judged against a loaded list. Before it lands, a
   * param that names nothing looks exactly like one naming a session the page
   * has not heard of yet, and clearing it would drop a good link on reload.
   */
  const [sessionsLoaded, setSessionsLoaded] = useState(false);
  /**
   * The transcript a fork wrote, waiting for the draft agent to send it.
   *
   * It survives an engine change and a rewritten framing line, and clears
   * once a session starts or the reader closes the draft.
   */
  const [forkSource, setForkSource] = useState<CodeForkTranscript | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);
  const [starting, setStarting] = useState(false);
  const rememberedSession = useCodeCatalogStore(
    (state) => state.sessionsByWorkspace[workspaceId] ?? null,
  );
  // A create remembers the session before this page's list returns, and
  // before the merge effect below runs. Fold it in during render so dropping
  // the startup overlay does not flash the empty start prompt for a frame.
  const listedSessions = useMemo(() => {
    if (!rememberedSession) return sessions;
    if (sessions.some((entry) => entry.id === rememberedSession.id)) {
      return sessions;
    }
    return [rememberedSession, ...sessions];
  }, [rememberedSession, sessions]);
  const conversations = useMemo(
    () => liveCodeSessions(listedSessions),
    [listedSessions],
  );
  const session = useMemo(() => {
    const selected = listedSessions.find(
      (entry) => entry.id === activeSessionId,
    );
    if (selected) return selected;
    if (draftAgent || activeSessionId) return null;
    return conversations[0] ?? null;
  }, [activeSessionId, conversations, draftAgent, listedSessions]);
  const conversationDigests = useConversationDigests(workspaceId);

  useEffect(() => {
    startRequestRef.current += 1;
    setStarting(false);
  }, [client]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      startRequestRef.current += 1;
    };
  }, []);

  // A session started elsewhere — the new-workspace dialog, say — reaches the
  // page through the catalog before the list request comes back.
  useEffect(() => {
    if (!rememberedSession) return;
    setSessions((current) =>
      current.some((entry) => entry.id === rememberedSession.id)
        ? current
        : [...current, rememberedSession],
    );
  }, [rememberedSession]);

  // `?task=` names the session to show: a sibling agent, a watch child
  // opened from the rail, or an archived conversation from search. The param
  // is a request, not a fact — a link outlives the agent it points at, so it
  // holds only while that agent is still listed.
  const namedTask = useMemo(() => {
    if (!taskParam) return null;
    return sessions.find((entry) => entry.id === taskParam) ?? null;
  }, [sessions, taskParam]);

  // A param that names no listed session is stale. Drop it so the fallback
  // below runs and the URL stops naming an agent that is not there. Replace
  // rather than push, so Back does not lead to the same dead link. An ended
  // session is still listed, so archive search can open its transcript.
  useEffect(() => {
    if (!sessionsLoaded || !taskParam || namedTask) return;
    openWorkspaceTask(undefined, { replace: true });
  }, [namedTask, sessionsLoaded, taskParam]);

  // A named session wins over the default selection below.
  useEffect(() => {
    if (!namedTask) return;
    setActiveSessionId(namedTask.id);
    setDraftAgent(false);
  }, [namedTask]);

  // Nothing named, or the shown agent ended: fall back to the first one. The
  // check reads the live conversations, not every session, so an agent that
  // ended under the reader does not stay selected.
  useEffect(() => {
    if (namedTask || draftAgent) return;
    const shown = conversations.some((entry) => entry.id === activeSessionId);
    if (activeSessionId && shown) return;
    setActiveSessionId(conversations[0]?.id ?? null);
  }, [activeSessionId, conversations, draftAgent, namedTask]);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    setSessionsLoaded(false);
    void (async () => {
      try {
        const [next, listed] = await Promise.all([
          client.getCodeWorkspace(workspaceId),
          client.listCodeWorkspaceSessions(workspaceId),
        ]);
        if (cancelled) return;
        setWorkspace(next);
        const catalogState = useCodeCatalogStore.getState();
        catalogState.upsertWorkspace(next);
        // Create navigates as soon as the workspace exists, so a session the
        // dialog just started can land in the catalog before this list does.
        const remembered = catalogState.sessionsByWorkspace[workspaceId];
        setSessions(
          remembered && !listed.some((entry) => entry.id === remembered.id)
            ? [remembered, ...listed]
            : listed,
        );
        setSessionsLoaded(true);
        // The card and the rail show one agent per workspace, so the catalog
        // remembers the first — the one the workspace was started with.
        const first = liveCodeSessions(listed)[0];
        if (first) catalogState.rememberSession(first);
        const nextRepo = await client.getCodeRepo(next.repo_id);
        if (!cancelled) setRepo(nextRepo);
      } catch (err) {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Could not load this workspace"));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, workspaceId, reloadToken]);

  async function startSession(
    harness: HarnessKind,
    permissionMode: PermissionMode,
    message: string,
    model?: string,
    draft = message,
    reasoningEffort?: ReasoningEffort | null,
    fastMode = false,
  ) {
    const request = startRequestRef.current + 1;
    startRequestRef.current = request;
    const startedWithClient = client;
    const startedAtSelection = selectionRevisionRef.current;
    const startedWithFork = forkSource;
    const isCurrent = () =>
      mountedRef.current &&
      startRequestRef.current === request &&
      clientRef.current === startedWithClient;
    setStarting(true);
    try {
      let created: CodeSessionSnapshot;
      try {
        const gateway = gatewayCodeModels(models, harness, defaultModelKey);
        const native =
          requiresHarnessModelIds(harness) || gateway.length === 0
            ? await catalog.ensureHarnessModels(startedWithClient, harness)
            : [];
        if (!isCurrent()) {
          throw new Error(
            "The Code connection changed before the session started. Send the message again.",
          );
        }
        const listed = preferredCodeModels(harness, native, gateway);
        const posted =
          model ?? listed.find((option) => option.default)?.id ?? listed[0]?.id;
        created = await startedWithClient.createCodeSession(workspaceId, {
          harness,
          permission_mode: permissionMode,
          model: posted,
          ...(reasoningEffort ? { reasoning_effort: reasoningEffort } : {}),
          ...(fastMode ? { fast_mode: true } : {}),
        });
      } catch (err) {
        if (isCurrent()) {
          toast.error(friendlyErrorMessage(err, "Could not start a session"));
        }
        throw err;
      }

      const heldImages = useCodeUiStore
        .getState()
        .takeComposerImages(workspaceId);

      const recovery: FirstTurnRecovery = {
        id: `${created.id}:${request}`,
        sessionId: created.id,
        draft,
        forkSource: startedWithFork,
        message: "Sending your first message…",
        status: "sending",
      };
      if (!isCurrent()) {
        if (heldImages && heldImages.length > 0) {
          useCodeUiStore
            .getState()
            .offerComposerPrompt(workspaceId, draft, heldImages);
        }
        const message =
          "The Code connection changed after the session was created. Send the message again.";
        writeFirstTurnRecovery(startedWithClient, {
          ...recovery,
          message,
          status: "failed",
        });
        throw new Error(message);
      }
      writeFirstTurnRecovery(startedWithClient, recovery);

      if (conversations.length === 0) catalog.rememberSession(created);
      setSessions((current) =>
        current.some((entry) => entry.id === created.id)
          ? current
          : [...current, created],
      );
      setForkSource((current) =>
        current === startedWithFork ? null : current,
      );
      if (selectionRevisionRef.current === startedAtSelection) {
        setActiveSessionId(created.id);
        setDraftAgent(false);
        // The first agent stays at a clean URL; a sibling names itself so a
        // reload comes back to the tab the reader was on.
        if (conversations.length > 0) openWorkspaceTask(created.id);
      }

      try {
        const attachments = heldImages?.length
          ? await Promise.all(
              heldImages.map(async (file) => {
                const published = hasLocalHostAuthority()
                  ? await publishCodeImage(created.id, file)
                  : await uploadImageAttachment(
                      startedWithClient,
                      created.id,
                      file,
                      {
                        onProgress: () => undefined,
                        signal: new AbortController().signal,
                        path: (id) =>
                          `/sessions/${encodeURIComponent(id)}/attachments/images`,
                      },
                    );
                return {
                  blob_id: published.attachmentId,
                  media_type: published.mediaType,
                };
              }),
            )
          : [];
        if (attachments.length > 0) {
          await startedWithClient.submitCodeTurn(
            created.id,
            message,
            undefined,
            attachments,
          );
        } else {
          await startedWithClient.submitCodeTurn(created.id, message);
        }
        clearFirstTurnRecovery(startedWithClient, created.id, recovery.id);
      } catch (err) {
        if (heldImages && heldImages.length > 0) {
          useCodeUiStore
            .getState()
            .offerComposerPrompt(workspaceId, draft, heldImages);
        }
        const detail = friendlyErrorMessage(err, "Try sending it again.");
        writeFirstTurnRecovery(startedWithClient, {
          ...recovery,
          message: `The first message was not sent. Review it, then choose Send to try again. ${detail}`,
          status: "failed",
        });
        if (isCurrent()) {
          toast.error(
            `Session started, but the first message was not sent. ${detail}`,
          );
        }
      }
    } finally {
      if (isCurrent()) setStarting(false);
    }
  }

  async function reap() {
    if (!session) return;
    try {
      const next = await client.reapCodeSession(session.id);
      catalog.rememberSession(next);
      setSessions((current) =>
        current.map((entry) => (entry.id === next.id ? next : entry)),
      );
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not reap the session"));
    }
  }

  /**
   * Attach a child task's transcript (or the conversation when undefined).
   *
   * `replace` is for corrections rather than choices: clearing a stale param
   * should not leave a history entry the reader can walk back into.
   */
  function openWorkspaceTask(
    sessionId: string | undefined,
    options?: { replace?: boolean },
  ) {
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
      replace: options?.replace ?? false,
      search: (current: Record<string, unknown>) => ({
        ...current,
        task: sessionId,
        subagent: undefined,
      }),
    });
  }

  /**
   * Show one of the workspace's agents, or the unstarted draft when null.
   *
   * Selection lives in `?task=` so a reload or a shared link returns to the
   * same agent. The first one is the workspace's default, so it stays unnamed.
   */
  function selectConversation(sessionId: string | null) {
    selectionRevisionRef.current += 1;
    focusConversationPane();
    if (sessionId === null) {
      setDraftAgent(true);
      setActiveSessionId(null);
      openWorkspaceTask(undefined);
      return;
    }
    setDraftAgent(false);
    setActiveSessionId(sessionId);
    openWorkspaceTask(
      sessionId === conversations[0]?.id ? undefined : sessionId,
    );
  }

  /** Add a tab for an agent the reader has not filled in yet. */
  function newConversation() {
    setForkSource(null);
    selectConversation(null);
  }

  /**
   * Hand one agent's transcript to a fresh one.
   *
   * The server writes the fork into private storage — the condensed
   * transcript plus a full record per turn — so the child reads it from an
   * absolute path whatever engine it turns out to be. `atTurnId` forks at
   * the end of that turn; omitted, the fork covers the whole conversation.
   * Nothing is sent here: the draft tab opens with the transcript attached
   * and framing lines the reader edits first.
   */
  async function forkConversation(sessionId: string, atTurnId?: string) {
    try {
      const written = await client.forkCodeSession(sessionId, atTurnId);
      setForkSource(written);
      selectConversation(null);
      useCodeUiStore
        .getState()
        .offerComposerPrompt(workspaceId, forkFraming(written));
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not fork this agent"));
    }
  }

  /** Close a conversation tab without ending its agent or workspace. */
  function closeConversation(sessionId: string | null) {
    if (sessionId === null) {
      setDraftAgent(false);
      setForkSource(null);
    } else {
      const index = conversations.findIndex((entry) => entry.id === sessionId);
      if (index <= 0) return;
      setClosedConversationIds((current) => {
        const next = new Set(current);
        next.add(sessionId);
        return next;
      });
      if (sessionId !== activeSessionId) return;
    }
    const first = conversations[0];
    if (first) selectConversation(first.id);
    else setActiveSessionId(null);
  }

  /** Filter the parent transcript to one harness-owned child. */
  function openWorkspaceSubagent(callId: string | undefined) {
    void navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId },
      search: (current: Record<string, unknown>) => ({
        ...current,
        task: undefined,
        subagent: callId,
      }),
    });
  }

  /**
   * The agent tab the strip should mark selected.
   *
   * A watch child opened from the rail is a drill-in with its own back bar, not
   * a peer tab, so the first agent stays selected underneath it.
   */
  const activeConversationId = draftAgent
    ? null
    : (conversations.find((entry) => entry.id === session?.id)?.id ??
      conversations[0]?.id ??
      null);
  const conversationTabs = useMemo<CodeConversationTab[]>(() => {
    const tabs: CodeConversationTab[] = conversations
      .filter(
        (entry, index) => index === 0 || !closedConversationIds.has(entry.id),
      )
      .map((entry) => {
        const index = conversations.findIndex(
          (conversation) => conversation.id === entry.id,
        );
        const digest = conversationDigests[entry.id];
        return {
          id: entry.id,
          label: conversationTabLabel(entry, index, conversations),
          harness: entry.harness_kind,
          attention: attentionMarkForDigest(digest),
          closable: index > 0,
        };
      });
    // A draft has no engine yet, so it wears the generic agent glyph. It is
    // also the one closable tab: nothing is running behind it. A workspace
    // with no agents at all still gets one, so the strip always names the
    // panel below it.
    if (draftAgent || tabs.length === 0) {
      tabs.push({
        id: null,
        label: tabs.length === 0 ? "Main agent" : "New agent",
        closable: tabs.length > 0,
      });
    }
    return tabs;
  }, [closedConversationIds, conversationDigests, conversations, draftAgent]);

  /** The picker shows for a first agent and for every one added after it. */
  const startingNewAgent = draftAgent || !session;

  return {
    workspace,
    repo,
    session,
    conversations,
    conversationTabs,
    activeConversationId,
    draftAgent,
    startingNewAgent,
    starting,
    forkSource,
    clearForkSource: () => setForkSource(null),
    error,
    retry: () => setReloadToken((token) => token + 1),
    startSession,
    reap,
    selectConversation,
    newConversation,
    forkConversation,
    closeConversation,
    openWorkspaceTask,
    openWorkspaceSubagent,
  };
}
