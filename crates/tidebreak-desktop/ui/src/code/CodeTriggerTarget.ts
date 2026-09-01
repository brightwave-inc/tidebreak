import type {
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
  HarnessDoctorReport,
  RepoId,
} from "@/api/types";
import { HARNESS_LABELS } from "./labels";
import type { CodeTriggerTarget } from "./CodeTriggerRules";
import type { DigestsByWorkspace } from "./CodeUpdatesStore";

type TargetWorkspace = Pick<
  CodeWorkspaceSnapshot,
  "id" | "repo_id" | "status" | "pr"
>;

/**
 * Name the session a repository trigger would reach right now.
 *
 * The server creates one fire per eligible workspace. This projection names
 * the newest target across those workspaces, using the same timestamp that the
 * delivery planner uses within each workspace.
 */
export function codeTriggerTargetForRepository({
  repoId,
  workspaces,
  conversationsByWorkspace,
  doctor,
}: {
  repoId: RepoId | null | undefined;
  workspaces: readonly TargetWorkspace[];
  conversationsByWorkspace: DigestsByWorkspace;
  doctor: HarnessDoctorReport | null;
}): CodeTriggerTarget | null {
  if (!repoId) return null;

  const eligibleWorkspaceIds = new Set(
    workspaces
      .filter(
        (workspace) =>
          workspace.repo_id === repoId &&
          workspace.status === "active" &&
          workspace.pr !== undefined,
      )
      .map((workspace) => workspace.id),
  );

  let target: CodeSessionDigest | undefined;
  let targetAt = Number.NEGATIVE_INFINITY;
  for (const workspaceId of eligibleWorkspaceIds) {
    const conversations = conversationsByWorkspace[workspaceId];
    if (!conversations) continue;
    for (const digest of Object.values(conversations)) {
      if (
        digest.kind !== "interactive" ||
        digest.lifecycle === "ended" ||
        digest.lifecycle === "fenced" ||
        !digest.harness_kind ||
        !digest.trigger_target_at
      ) {
        continue;
      }
      const activeAt = Date.parse(digest.trigger_target_at);
      if (!Number.isFinite(activeAt) || activeAt <= targetAt) continue;
      target = digest;
      targetAt = activeAt;
    }
  }

  if (!target?.harness_kind) return null;
  const harness = doctor?.harnesses.find(
    (entry) => entry.kind === target.harness_kind,
  );
  return {
    sessionTitle: target.title,
    harnessLabel: HARNESS_LABELS[target.harness_kind],
    delivery:
      harness?.caps.mid_turn_steering === "supported" ? "steer" : "next_turn",
  };
}
