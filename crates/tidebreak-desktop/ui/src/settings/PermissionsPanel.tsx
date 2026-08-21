import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { toast } from "sonner";

import type {
  ApiClient,
  ConsentStatementSnapshot,
  GrantScope,
  RendererToolName,
} from "../api";
import { useConfirm } from "../components/ConfirmDialog";
import { hostErrorMessage } from "@/remoteMachine";
import {
  grantFolderCapability,
  listCapabilityConsents,
  listConnectedFolders,
  revokeCapabilityConsent,
  type ConnectedFolder,
} from "../host";
import { folderReach, folderStatements } from "../FolderAccess";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "../NativePickerLatch";
import { useRefreshSignals } from "../RefreshSignals";
import { Button } from "@/components/ui/button";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

// The "what may the agent do without asking" surface, rendered from the
// unified consent read model: standing tool grants served by the server and
// host-broker capability grants reported over the Tauri boundary, as one list
// of statements grouped by what they reach. A consent the reader cannot find
// is a one-way door; this page is where it is found.

const TOOL_LABELS: Partial<Record<RendererToolName, string>> = {
  exec: "Commands",
  search: "Document search",
  web_search: "Web search",
  web_extract: "Web pages",
  write_file: "Workspace files",
  spawn_sandbox_agent: "Background agents",
};

export function toolGrantLabel(action: RendererToolName): string {
  return TOOL_LABELS[action] ?? action;
}

const CAPABILITY_LABELS: Record<
  Extract<
    ConsentStatementSnapshot["verb"],
    { kind: "capability" }
  >["capability"],
  string
> = {
  list_roots: "List folders",
  read_files: "Read files",
  write_files: "Write files",
  execute_commands: "Run commands",
};

/** The verb line: the class of action this statement allows. */
export function verbLabel(verb: ConsentStatementSnapshot["verb"]): string {
  return verb.kind === "tool"
    ? toolGrantLabel(verb.action)
    : CAPABILITY_LABELS[verb.capability];
}

/**
 * The scope line, worded as the width of what was agreed to. Mirrors the
 * approval card's rungs: the exact action, a run of command tokens, an
 * executable with any arguments, or the whole tool.
 */
export function grantScopeLabel(
  scope: GrantScope,
  action: RendererToolName,
): string {
  switch (scope.scope) {
    case "exact_action": {
      if (scope.tool === "exec") {
        return [scope.command, ...scope.args].join(" ");
      }
      // A page grant is for one address, and an address read back in quotes
      // reads as a phrase rather than as the place it will fetch.
      if (scope.tool === "web_extract") {
        return scope.url;
      }
      // Retained for old durable grants: workspace writes are granted about a
      // place (`path_subtree`) rather than exactly, so no current mint stores
      // this scope; named anyway so the vocabulary stays total.
      if (scope.tool === "write_file") {
        return scope.path;
      }
      // Delegation is only ever granted for the whole tool — a model-authored
      // task would never match a second time — so this is named for totality
      // rather than because a mint stores it.
      if (scope.tool === "delegate_agent") {
        return scope.task;
      }
      return `“${scope.query}”`;
    }
    case "any_args_for":
      return `${scope.command} …`;
    case "command_prefix":
      return `${scope.tokens.join(" ")} …`;
    case "path_subtree":
      return `Writes under ${scope.prefix}`;
    case "whole_tool":
      switch (action) {
        case "exec":
          return "Any command";
        case "search":
          return "Every document search";
        case "web_search":
          return "Every web search";
        case "web_extract":
          return "Every web page";
        case "spawn_sandbox_agent":
          return "Every background agent";
        default:
          return `Every ${toolGrantLabel(action)} call`;
      }
  }
}

/**
 * The resource line: what the statement's verb is allowed to touch. Host
 * resources carry the same safe folder identity the folders surface shows —
 * a display name, never an absolute path.
 */
export function resourceLabel(statement: ConsentStatementSnapshot): string {
  const { resource, verb } = statement;
  switch (resource.kind) {
    case "action_scope":
      return grantScopeLabel(
        resource.scope,
        verb.kind === "tool" ? verb.action : "other",
      );
    case "host_subject":
      return "Connected folders";
    case "host_root":
      return (
        resource.display_name ?? `Folder ${shortOpaqueId(resource.root_id)}`
      );
    case "host_path_subtree": {
      const folder =
        resource.display_name ?? `Folder ${shortOpaqueId(resource.root_id)}`;
      return `${folder}/${resource.relative}`;
    }
  }
}

const METHOD_PHRASES: Record<ConsentStatementSnapshot["method"], string> = {
  approval_card: "from an approval card",
  folder_picker: "with the folder picker",
  permission_dialog: "in a permission dialog",
  operator_config: "by operator configuration",
  carried_forward: "carried forward from an earlier approval",
};

export function shortOpaqueId(id: string): string {
  return id.length <= 10 ? id : `${id.slice(0, 6)}…${id.slice(-4)}`;
}

/** The identity of whatever a statement reaches, for grouping. */
export function levelKey(statement: ConsentStatementSnapshot): string {
  return statement.level.level === "chat"
    ? `chat:${statement.level.chat_id}`
    : `project:${statement.level.project_id}`;
}

/**
 * Whether a statement's subject can still be resolved in product state.
 * Missing titles are normal for untitled chats; the caller passes known live
 * ids when it wants to distinguish deleted subjects from untitled ones.
 */
export function isMissingSubject(
  statement: ConsentStatementSnapshot,
  known?: { chatIds?: ReadonlySet<string>; projectIds?: ReadonlySet<string> },
): boolean {
  if (!known) return false;
  if (statement.level.level === "chat") {
    return known.chatIds ? !known.chatIds.has(statement.level.chat_id) : false;
  }
  return known.projectIds
    ? !known.projectIds.has(statement.level.project_id)
    : false;
}

/**
 * What a group of statements applies to, named the way the reader chose it. A
 * project statement says so out loud: it reaches conversations that have not
 * been started yet. When `known` is provided, subjects missing from product
 * state are labeled as deleted rather than as opaque ids.
 */
export function levelLabel(
  statement: ConsentStatementSnapshot,
  known?: { chatIds?: ReadonlySet<string>; projectIds?: ReadonlySet<string> },
): string {
  const title = statement.level_title?.trim();
  if (statement.level.level === "project") {
    if (isMissingSubject(statement, known)) {
      return `Deleted project ${shortOpaqueId(statement.level.project_id)}`;
    }
    return title
      ? `Everything in ${title}`
      : `Everything in project ${shortOpaqueId(statement.level.project_id)}`;
  }
  if (isMissingSubject(statement, known)) {
    return `Deleted work ${shortOpaqueId(statement.level.chat_id)}`;
  }
  return title || `Work ${shortOpaqueId(statement.level.chat_id)}`;
}

/** Statements that reach one conversation: its own grants plus project grants. */
export function statementsForChat(
  statements: readonly ConsentStatementSnapshot[],
  chat: { id: string; project_id: string | null },
): ConsentStatementSnapshot[] {
  return statements.filter((statement) => {
    if (statement.level.level === "chat") {
      return statement.level.chat_id === chat.id;
    }
    return (
      chat.project_id != null && statement.level.project_id === chat.project_id
    );
  });
}

function grantedAtLabel(statement: ConsentStatementSnapshot): string {
  const when = new Date(statement.granted_at);
  if (Number.isNaN(when.getTime())) return "";
  const date = when.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
  return `Added ${date} ${METHOD_PHRASES[statement.method]}`;
}

/** A statement's revocation identity, unique across both stores. */
export function handleKey(statement: ConsentStatementSnapshot): string {
  return statement.handle.kind === "tool_grant"
    ? `tool_grant:${statement.handle.call_id}`
    : `capability_grant:${statement.handle.grant_id}`;
}

/** A restore row's identity, in the same namespace as revocation handles so
 * one busy row can be named whichever kind it is. */
export function restoreKey(folder: { rootId: string }): string {
  return `restore_read:${folder.rootId}`;
}

/** Statements in listing order, bucketed by what they reach, order preserved. */
export function groupByLevel(
  statements: readonly ConsentStatementSnapshot[],
  known?: { chatIds?: ReadonlySet<string>; projectIds?: ReadonlySet<string> },
): { key: string; label: string; statements: ConsentStatementSnapshot[] }[] {
  const groups = new Map<
    string,
    { key: string; label: string; statements: ConsentStatementSnapshot[] }
  >();
  for (const statement of statements) {
    const key = levelKey(statement);
    const group = groups.get(key);
    if (group) group.statements.push(statement);
    else
      groups.set(key, {
        key,
        label: levelLabel(statement, known),
        statements: [statement],
      });
  }
  return [...groups.values()];
}

/** One line of the list: what it reaches, what it allows, and its one action. */
function AccessRow({
  title,
  subtitle,
  meta,
  action,
}: {
  title: string;
  subtitle: string;
  meta?: string;
  action: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{title}</p>
        <p className="text-muted-foreground mt-0.5 truncate text-sm">
          {subtitle}
        </p>
        {meta && (
          <p className="text-muted-foreground mt-1 truncate text-xs">{meta}</p>
        )}
      </div>
      {action}
    </div>
  );
}

function StatementRow({
  statement,
  busy,
  onRevoke,
}: {
  statement: ConsentStatementSnapshot;
  busy: boolean;
  onRevoke: () => void;
}) {
  return (
    <AccessRow
      title={resourceLabel(statement)}
      subtitle={verbLabel(statement.verb)}
      meta={grantedAtLabel(statement) || undefined}
      action={
        <Button variant="ghost" size="sm" disabled={busy} onClick={onRevoke}>
          Revoke
        </Button>
      }
    />
  );
}

export type PermissionsPanelProps = {
  client: ApiClient;
  /**
   * When set, only statements that reach this conversation (chat-level grants
   * plus applicable project grants) are shown, and the chrome is panel-sized
   * rather than the settings page shell.
   */
  chat?: { id: string; project_id: string | null };
  /** Live chat ids, used to label deleted subjects on the global surface. */
  knownChatIds?: ReadonlySet<string>;
  /** Live project ids, used to label deleted subjects on the global surface. */
  knownProjectIds?: ReadonlySet<string>;
};

export function PermissionsPanel({
  client,
  chat,
  knownChatIds,
  knownProjectIds,
}: PermissionsPanelProps) {
  const [statements, setStatements] = useState<
    ConsentStatementSnapshot[] | null
  >(null);
  /**
   * Folders this chat has attached but cannot read.
   *
   * Revoking read leaves the attachment alone, so this is a real state with
   * no way forward: nothing restores a folder's access on its own, and
   * attaching it again deliberately does not. The panel promises anything
   * revoked can be asked for again, and these rows are what makes that true.
   * Only computed with a chat in hand — a widening is granted to one
   * conversation's subject, and the settings page has no conversation.
   */
  const [unreadableFolders, setUnreadableFolders] = useState<ConnectedFolder[]>(
    [],
  );
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();
  const reloadGenerationRef = useRef(0);
  const known =
    knownChatIds || knownProjectIds
      ? { chatIds: knownChatIds, projectIds: knownProjectIds }
      : undefined;

  const chatId = chat?.id ?? null;
  const chatProjectId = chat?.project_id ?? null;
  const scopeKey = chatId
    ? `chat:${chatId}:project:${chatProjectId ?? "none"}`
    : "global";
  const scopeKeyRef = useRef(scopeKey);
  scopeKeyRef.current = scopeKey;

  const reload = useCallback(async () => {
    const generation = ++reloadGenerationRef.current;
    try {
      const chatScope = chatId
        ? { id: chatId, project_id: chatProjectId }
        : null;
      const [tool, capability, folders] = await Promise.all([
        client.listConsentStatements(),
        listCapabilityConsents(),
        chatScope ? listConnectedFolders(chatScope) : Promise.resolve([]),
      ]);
      if (generation !== reloadGenerationRef.current) return;
      const all = [...tool, ...capability];
      setStatements(chatScope ? statementsForChat(all, chatScope) : all);
      setUnreadableFolders(
        chatScope
          ? folders.filter(
              (folder) =>
                folder.status === "connected" &&
                !folderReach(
                  folderStatements(all, folder.rootId, chatScope),
                ).includes("read_files"),
            )
          : [],
      );
      setError(null);
    } catch {
      if (generation !== reloadGenerationRef.current) return;
      setError("Saved approvals could not be loaded.");
    }
  }, [client, chatId, chatProjectId]);

  useEffect(() => {
    setStatements(null);
    setUnreadableFolders([]);
    setError(null);
    setBusyId(null);
    void reload();
    return () => {
      reloadGenerationRef.current += 1;
    };
  }, [reload, scopeKey]);

  /**
   * Ask for read access back on a folder this chat still has attached.
   *
   * The consent ceremony is the host's own dialog, exactly as it is for the
   * write and command widenings the folders panel offers — this only asks,
   * and follows the broker's answer. Nothing here mints anything on its own,
   * and no other surface restores this grant as a side effect.
   */
  async function restoreReadAccess(folder: ConnectedFolder) {
    const startingScope = scopeKey;
    if (!chatId) return;
    if (
      !useNativePickerLatch
        .getState()
        .claim(PICKER_HOLDERS.grantFolderCapability)
    ) {
      toast.error(PICKER_BUSY_MESSAGE);
      return;
    }
    setBusyId(restoreKey(folder));
    try {
      const granted = await grantFolderCapability(
        { id: chatId },
        folder.rootId,
        "read_files",
      );
      if (granted) useRefreshSignals.getState().signal("folderAccess");
      if (scopeKeyRef.current !== startingScope) return;
      if (granted !== null) await reload();
    } catch (caught) {
      toast.error(
        hostErrorMessage(caught, "The folder could not be granted access."),
      );
    } finally {
      useNativePickerLatch
        .getState()
        .release(PICKER_HOLDERS.grantFolderCapability);
      if (scopeKeyRef.current === startingScope) setBusyId(null);
    }
  }

  async function revoke(statement: ConsentStatementSnapshot) {
    const startingScope = scopeKey;
    const confirmed = await confirm({
      title: "Revoke this approval?",
      description:
        statement.verb.kind === "capability" &&
        statement.verb.capability === "read_files"
          ? `The agent loses “${verbLabel(statement.verb).toLowerCase()}” for “${resourceLabel(
              statement,
            )}” in ${levelLabel(
              statement,
              known,
            ).toLowerCase()} — and command access to it, which depends on reading.`
          : `The agent will ask again before ${verbLabel(
              statement.verb,
            ).toLowerCase()} covered by “${resourceLabel(
              statement,
            )}” in ${levelLabel(statement, known).toLowerCase()}.`,
      confirmLabel: "Revoke",
      destructive: true,
    });
    if (!confirmed || scopeKeyRef.current !== startingScope) return;
    setBusyId(handleKey(statement));
    try {
      if (statement.handle.kind === "tool_grant") {
        await client.revokeStandingGrant(statement.handle.call_id);
      } else {
        await revokeCapabilityConsent(statement);
      }
      if (scopeKeyRef.current !== startingScope) return;
      // Reload rather than filter locally: revoking a folder's read access
      // also withdraws its dependent command access, and the list should show
      // exactly what the broker now holds.
      await reload();
    } catch {
      if (scopeKeyRef.current === startingScope) {
        setError("The approval could not be revoked. Try again.");
      }
    } finally {
      if (scopeKeyRef.current === startingScope) setBusyId(null);
    }
  }

  const body = (
    <>
      {error && <SettingsError>{error}</SettingsError>}
      {statements !== null &&
        statements.length === 0 &&
        unreadableFolders.length === 0 &&
        !error && (
          <p className="text-sm text-muted-foreground">
            {chat
              ? "Nothing saved for this work yet. When you answer an approval with “always allow” or connect a folder, it appears here."
              : "Nothing saved yet. When you answer an approval with “always allow” or connect a folder, it appears here."}
          </p>
        )}
      {statements !== null &&
        groupByLevel(statements, known).map((group) => (
          <SettingsSection key={group.key} title={group.label}>
            <div className="flex flex-col gap-4">
              {group.statements.map((statement) => (
                <StatementRow
                  key={handleKey(statement)}
                  statement={statement}
                  busy={busyId === handleKey(statement)}
                  onRevoke={() => void revoke(statement)}
                />
              ))}
            </div>
          </SettingsSection>
        ))}
      {unreadableFolders.length > 0 && (
        <SettingsSection
          title="Revoked folder access"
          description="These folders are still connected to this work, but the agent cannot read them. Grant read access to make one usable again — the host asks you to confirm."
        >
          <div className="flex flex-col gap-4">
            {unreadableFolders.map((folder) => (
              <AccessRow
                key={restoreKey(folder)}
                title={folder.displayName}
                subtitle={CAPABILITY_LABELS.read_files}
                action={
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busyId === restoreKey(folder)}
                    onClick={() => void restoreReadAccess(folder)}
                  >
                    Grant
                  </Button>
                }
              />
            ))}
          </div>
        </SettingsSection>
      )}
      {dialog}
    </>
  );

  if (chat) {
    return (
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        <div>
          <h2 className="text-sm font-medium">Permissions</h2>
          <p className="text-muted-foreground mt-1 text-sm">
            What this work can do without asking. Project-wide approvals that
            reach it are included. Revoke anything to be asked again.
          </p>
        </div>
        {statements === null ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : (
          body
        )}
      </div>
    );
  }

  return (
    <SettingsPanel
      title="Permissions"
      description="What the agent can do without asking. Revoke anything to be asked again."
      busy={statements === null}
    >
      {body}
    </SettingsPanel>
  );
}
