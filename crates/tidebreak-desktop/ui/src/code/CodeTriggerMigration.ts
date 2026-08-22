import type {
  CodeGitHubRepositoryRef,
  CodeTriggerAction,
  CodeTriggerCondition,
} from "@/api/types";
import type {
  CodeDeliveryNotificationRule,
  CodeDeliveryNotificationRuleKind,
} from "@/code/CodeDeliveryStore";

/**
 * One trigger to arm on the server, derived from a client-side rule.
 *
 * Record 60 folds the `localStorage` notification rules into the trigger
 * substrate so there is one rule engine rather than two. This is the mapping,
 * kept pure so the migration can be tested without a server.
 */
export type TriggerToArm = {
  repoId: string;
  condition: CodeTriggerCondition;
  action: CodeTriggerAction;
};

/**
 * What each old rule watched for, in trigger vocabulary.
 *
 * `pull_request_attention` covered more than one fact, so it maps to more than
 * one condition. Dropping either would silently narrow what a user already
 * asked to hear about.
 */
const RULE_CONDITIONS: Record<
  CodeDeliveryNotificationRuleKind,
  CodeTriggerCondition[]
> = {
  pull_request_attention: ["changes_requested", "conflicts"],
  pull_request_ready: ["ready_to_merge"],
  run_failure: ["checks_failed"],
};

function repositoryKey(repository: CodeGitHubRepositoryRef): string {
  return `${repository.host}/${repository.owner}/${repository.name}`;
}

/**
 * Turn the persisted notification rules into the triggers that reproduce them.
 *
 * Every result uses `notify`: these rules raised a notification and never sent
 * the agent anything, so migrating them to `deliver` would start interrupting
 * work the user never agreed to interrupt.
 *
 * A disabled rule produces nothing. An empty `repositoryKeys` means the rule
 * applied everywhere, so it maps to every repository Tidebreak knows; a
 * repository with no `tidebreak_repo_id` is not one triggers can bind to and is
 * skipped rather than guessed at.
 */
export function triggersForNotificationRules(
  rules: CodeDeliveryNotificationRule[],
  repositories: CodeGitHubRepositoryRef[],
): TriggerToArm[] {
  const armed = new Map<string, TriggerToArm>();
  for (const rule of rules) {
    if (!rule.enabled) {
      continue;
    }
    const scoped =
      rule.repositoryKeys.length === 0
        ? repositories
        : repositories.filter((repository) =>
            rule.repositoryKeys.includes(repositoryKey(repository)),
          );
    for (const repository of scoped) {
      const repoId = repository.tidebreak_repo_id;
      if (!repoId) {
        continue;
      }
      for (const condition of RULE_CONDITIONS[rule.id]) {
        // One row per (repository, condition) — the server's own unique key.
        // Two rules mapping onto one condition must not arm it twice.
        armed.set(`${repoId}:${condition}`, {
          repoId,
          condition,
          action: "notify",
        });
      }
    }
  }
  return [...armed.values()];
}
