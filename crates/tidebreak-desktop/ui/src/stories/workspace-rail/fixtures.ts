import type {
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
} from "@/api/types";
import {
  codeRepositories,
  codeSession,
  codeWorkspace,
  doneDigest,
  needsYouDigest,
  runningDigest,
} from "../fixtures";

export type RailEntry = {
  workspace: CodeWorkspaceSnapshot;
  session: CodeSessionSnapshot;
  digest?: CodeSessionDigest;
};

export const railRepositories = codeRepositories.slice(0, 2);

function entry(
  id: string,
  title: string,
  repo: number,
  source: "local" | "channel" | "dm",
  state: "running" | "needs-you" | "done" | "idle",
  index: number,
): RailEntry {
  const workspace: CodeWorkspaceSnapshot = {
    ...codeWorkspace,
    id,
    title,
    repo_id: railRepositories[repo]!.id,
    branch_name: `thet/${id}`,
    created_at: new Date(Date.UTC(2026, 8, 9, 8, index * 10)).toISOString(),
  };
  const template =
    state === "running"
      ? runningDigest
      : state === "needs-you"
        ? needsYouDigest
        : state === "done"
          ? doneDigest
          : undefined;
  const digest = template
    ? {
        ...template,
        workspace: id,
        session: `session-${id}`,
        title,
        activity_detail:
          state === "running" ? "Running focused tests" : undefined,
        recap: state === "done" ? "Changes are ready to review" : undefined,
      }
    : undefined;
  const session: CodeSessionSnapshot = {
    ...codeSession,
    id: `session-${id}`,
    workspace_id: id,
    lifecycle: digest?.lifecycle ?? "created",
    attention: digest?.attention ?? codeSession.attention,
    created_at: workspace.created_at,
    external_origin:
      source === "local"
        ? undefined
        : {
            channel_kind: "slack",
            external_key:
              source === "dm"
                ? `T0123456:D0123456:${index}`
                : `T0123456:C0123456:1788950000.00000${index}`,
          },
  };
  return { workspace, session, digest };
}

export const railEntries: RailEntry[] = [
  entry(
    "workspace-grouping",
    "Restore workspace group headings",
    0,
    "local",
    "running",
    1,
  ),
  entry(
    "browser-recovery",
    "Review browser recovery",
    0,
    "local",
    "needs-you",
    2,
  ),
  entry(
    "gateway-retry",
    "Fix credential refresh retries",
    1,
    "local",
    "running",
    3,
  ),
  entry("gateway-usage", "Clarify usage reporting", 1, "local", "done", 4),
  entry(
    "slack-no-repo",
    "Start without a repository",
    0,
    "channel",
    "running",
    5,
  ),
  entry("slack-share", "Check channel access", 0, "channel", "needs-you", 6),
  entry("slack-triage", "Triage the desktop issues", 0, "dm", "idle", 7),
  entry(
    "slack-cards",
    "Finish Slack decision cards",
    1,
    "channel",
    "running",
    8,
  ),
  entry("slack-setup", "Review the private app setup", 1, "dm", "done", 9),
];
