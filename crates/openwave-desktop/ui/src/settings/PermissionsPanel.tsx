import { useCallback, useEffect, useState } from "react";

import type {
  ApiClient,
  GrantScope,
  RendererToolName,
  StandingGrantSnapshot,
} from "../api";
import { useConfirm } from "../components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

// The "what the agent can do without asking" surface: every standing grant,
// grouped by the chat it applies to, each revocable back to being asked.
// A grant the reader cannot find is a one-way door; this page is where it is
// found.

const TOOL_LABELS: Partial<Record<RendererToolName, string>> = {
  exec: "Commands",
  search: "Document search",
  web_search: "Web search",
  web_extract: "Web pages",
};

export function toolGrantLabel(action: RendererToolName): string {
  return TOOL_LABELS[action] ?? action;
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

export function shortOpaqueId(id: string): string {
  return id.length <= 10 ? id : `${id.slice(0, 6)}…${id.slice(-4)}`;
}

/** The identity of whatever a grant reaches, for grouping. */
export function levelKey(grant: StandingGrantSnapshot): string {
  return grant.level.level === "chat"
    ? `chat:${grant.level.chat_id}`
    : `project:${grant.level.project_id}`;
}

/**
 * What a group of grants applies to, named the way the reader chose it. A
 * project grant says so out loud: it reaches conversations that have not been
 * started yet, which is the whole point and also the thing worth being able
 * to see.
 */
export function levelLabel(grant: StandingGrantSnapshot): string {
  const title = grant.level_title?.trim();
  if (grant.level.level === "project") {
    return title
      ? `Everything in ${title}`
      : `Everything in project ${shortOpaqueId(grant.level.project_id)}`;
  }
  return title || `Chat ${shortOpaqueId(grant.level.chat_id)}`;
}

function grantedAtLabel(grantedAt: string): string {
  const when = new Date(grantedAt);
  if (Number.isNaN(when.getTime())) return "";
  return `Added ${when.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  })}`;
}

/** Grants in listing order, bucketed by what they reach, order preserved. */
export function groupByLevel(
  grants: readonly StandingGrantSnapshot[],
): { key: string; label: string; grants: StandingGrantSnapshot[] }[] {
  const groups = new Map<
    string,
    { key: string; label: string; grants: StandingGrantSnapshot[] }
  >();
  for (const grant of grants) {
    const key = levelKey(grant);
    const group = groups.get(key);
    if (group) group.grants.push(grant);
    else groups.set(key, { key, label: levelLabel(grant), grants: [grant] });
  }
  return [...groups.values()];
}

function GrantRow({
  grant,
  busy,
  onRevoke,
}: {
  grant: StandingGrantSnapshot;
  busy: boolean;
  onRevoke: () => void;
}) {
  const metadata = grantedAtLabel(grant.granted_at);
  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">
          {grantScopeLabel(grant.scope, grant.action)}
        </p>
        <p className="text-muted-foreground mt-0.5 truncate text-sm">
          {toolGrantLabel(grant.action)}
        </p>
        {metadata && (
          <p className="text-muted-foreground mt-1 truncate text-xs">
            {metadata}
          </p>
        )}
      </div>
      <Button variant="ghost" size="sm" disabled={busy} onClick={onRevoke}>
        Revoke
      </Button>
    </div>
  );
}

export function PermissionsPanel({ client }: { client: ApiClient }) {
  const [grants, setGrants] = useState<StandingGrantSnapshot[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();

  const reload = useCallback(async () => {
    try {
      setGrants(await client.listStandingGrants());
      setError(null);
    } catch {
      setError("Saved approvals could not be loaded.");
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function revoke(grant: StandingGrantSnapshot) {
    const confirmed = await confirm({
      title: "Revoke this approval?",
      description: `The agent will ask again before ${toolGrantLabel(
        grant.action,
      ).toLowerCase()} covered by “${grantScopeLabel(
        grant.scope,
        grant.action,
      )}” in ${levelLabel(grant).toLowerCase()}.`,
      confirmLabel: "Revoke",
      destructive: true,
    });
    if (!confirmed) return;
    setBusyId(grant.source_call_id);
    try {
      await client.revokeStandingGrant(grant.source_call_id);
      setGrants(
        (current) =>
          current?.filter(
            (existing) => existing.source_call_id !== grant.source_call_id,
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
      busy={grants === null}
    >
      {error && <SettingsError>{error}</SettingsError>}
      {grants !== null && grants.length === 0 && !error && (
        <p className="text-sm text-muted-foreground">
          Nothing saved yet. When you answer an approval with “always allow”,
          it appears here.
        </p>
      )}
      {grants !== null &&
        groupByLevel(grants).map((group) => (
          <SettingsSection key={group.key} title={group.label}>
            <div className="flex flex-col gap-4">
              {group.grants.map((grant) => (
                <GrantRow
                  key={grant.source_call_id}
                  grant={grant}
                  busy={busyId === grant.source_call_id}
                  onRevoke={() => void revoke(grant)}
                />
              ))}
            </div>
          </SettingsSection>
        ))}
      {dialog}
    </SettingsPanel>
  );
}
