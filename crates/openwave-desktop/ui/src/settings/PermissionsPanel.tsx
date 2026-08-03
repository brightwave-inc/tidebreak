import { useCallback, useEffect, useState } from "react";

import type {
  ApiClient,
  ConsentStatementSnapshot,
  GrantScope,
  RendererToolName,
} from "../api";
import { useConfirm } from "../components/ConfirmDialog";
import { listCapabilityConsents } from "../host";
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
      // Workspace writes are never standing-grantable, so no stored grant can
      // carry this scope today; named anyway so the vocabulary stays total.
      if (scope.tool === "write_file") {
        return scope.path;
      }
      return `“${scope.query}”`;
    }
    case "any_args_for":
      return `${scope.command} …`;
    case "command_prefix":
      return `${scope.tokens.join(" ")} …`;
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
      return resource.display_name ?? `Folder ${shortOpaqueId(resource.root_id)}`;
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
 * What a group of statements applies to, named the way the reader chose it. A
 * project statement says so out loud: it reaches conversations that have not
 * been started yet, which is the whole point and also the thing worth being
 * able to see.
 */
export function levelLabel(statement: ConsentStatementSnapshot): string {
  const title = statement.level_title?.trim();
  if (statement.level.level === "project") {
    return title
      ? `Everything in ${title}`
      : `Everything in project ${shortOpaqueId(statement.level.project_id)}`;
  }
  return title || `Chat ${shortOpaqueId(statement.level.chat_id)}`;
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

/** Statements in listing order, bucketed by what they reach, order preserved. */
export function groupByLevel(
  statements: readonly ConsentStatementSnapshot[],
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
        label: levelLabel(statement),
        statements: [statement],
      });
  }
  return [...groups.values()];
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
  const metadata = grantedAtLabel(statement);
  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">
          {resourceLabel(statement)}
        </p>
        <p className="text-muted-foreground mt-0.5 truncate text-sm">
          {verbLabel(statement.verb)}
        </p>
        {metadata && (
          <p className="text-muted-foreground mt-1 truncate text-xs">
            {metadata}
          </p>
        )}
      </div>
      {statement.handle.kind === "tool_grant" ? (
        <Button variant="ghost" size="sm" disabled={busy} onClick={onRevoke}>
          Revoke
        </Button>
      ) : (
        // Capability statements are not individually revocable yet; the
        // Folders surface disconnects the whole folder. Read model first.
        <span className="text-muted-foreground text-xs">
          Managed with its folder
        </span>
      )}
    </div>
  );
}

export function PermissionsPanel({ client }: { client: ApiClient }) {
  const [statements, setStatements] = useState<
    ConsentStatementSnapshot[] | null
  >(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();

  const reload = useCallback(async () => {
    try {
      const [tool, capability] = await Promise.all([
        client.listConsentStatements(),
        listCapabilityConsents(),
      ]);
      setStatements([...tool, ...capability]);
      setError(null);
    } catch {
      setError("Saved approvals could not be loaded.");
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function revoke(statement: ConsentStatementSnapshot) {
    if (statement.handle.kind !== "tool_grant") return;
    const callId = statement.handle.call_id;
    const confirmed = await confirm({
      title: "Revoke this approval?",
      description: `The agent will ask again before ${verbLabel(
        statement.verb,
      ).toLowerCase()} covered by “${resourceLabel(
        statement,
      )}” in ${levelLabel(statement).toLowerCase()}.`,
      confirmLabel: "Revoke",
      destructive: true,
    });
    if (!confirmed) return;
    setBusyId(handleKey(statement));
    try {
      await client.revokeStandingGrant(callId);
      setStatements(
        (current) =>
          current?.filter(
            (existing) =>
              existing.handle.kind !== "tool_grant" ||
              existing.handle.call_id !== callId,
          ) ?? null,
      );
      setError(null);
    } catch {
      setError("The approval could not be revoked. Try again.");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <SettingsPanel
      title="Permissions"
      description="What the agent can do without asking. Revoke anything to be asked again."
      busy={statements === null}
    >
      {error && <SettingsError>{error}</SettingsError>}
      {statements !== null && statements.length === 0 && !error && (
        <p className="text-sm text-muted-foreground">
          Nothing saved yet. When you answer an approval with “always allow”
          or connect a folder, it appears here.
        </p>
      )}
      {statements !== null &&
        groupByLevel(statements).map((group) => (
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
      {dialog}
    </SettingsPanel>
  );
}
