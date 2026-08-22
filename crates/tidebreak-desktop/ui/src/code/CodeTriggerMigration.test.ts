import { describe, expect, it } from "vitest";

import type { CodeGitHubRepositoryRef } from "@/api/types";
import type { CodeDeliveryNotificationRule } from "@/code/CodeDeliveryStore";
import { triggersForNotificationRules } from "@/code/CodeTriggerMigration";

function repository(
  name: string,
  tidebreakRepoId?: string,
): CodeGitHubRepositoryRef {
  return {
    host: "github.com",
    owner: "brightwave-inc",
    name,
    name_with_owner: `brightwave-inc/${name}`,
    url: `https://github.com/brightwave-inc/${name}`,
    ...(tidebreakRepoId ? { tidebreak_repo_id: tidebreakRepoId } : {}),
  };
}

function rule(
  id: CodeDeliveryNotificationRule["id"],
  patch: Partial<Omit<CodeDeliveryNotificationRule, "id">> = {},
): CodeDeliveryNotificationRule {
  return {
    id,
    enabled: true,
    repositoryKeys: [],
    tidebreakLinkedOnly: false,
    ...patch,
  };
}

describe("triggersForNotificationRules", () => {
  it("migrates every rule as notify, never as deliver", () => {
    const armed = triggersForNotificationRules(
      [rule("run_failure")],
      [repository("tidebreak", "repo-1")],
    );
    expect(armed).toEqual([
      { repoId: "repo-1", condition: "checks_failed", action: "notify" },
    ]);
  });

  it("keeps both facts an attention rule covered", () => {
    const armed = triggersForNotificationRules(
      [rule("pull_request_attention")],
      [repository("tidebreak", "repo-1")],
    );
    expect(armed.map((entry) => entry.condition).sort()).toEqual([
      "changes_requested",
      "conflicts",
    ]);
  });

  it("drops a disabled rule", () => {
    expect(
      triggersForNotificationRules(
        [rule("run_failure", { enabled: false })],
        [repository("tidebreak", "repo-1")],
      ),
    ).toEqual([]);
  });

  it("treats an empty scope as every repository", () => {
    const armed = triggersForNotificationRules(
      [rule("run_failure")],
      [repository("tidebreak", "repo-1"), repository("orca", "repo-2")],
    );
    expect(armed.map((entry) => entry.repoId).sort()).toEqual([
      "repo-1",
      "repo-2",
    ]);
  });

  it("matches a scope key whatever case it was stored in", () => {
    // Persisted keys are lowercased by `codeDeliveryRepositoryKey`; the
    // repository ref keeps the host's own casing.
    const armed = triggersForNotificationRules(
      [
        rule("run_failure", {
          repositoryKeys: ["github.com/brightwave-inc/orca"],
        }),
      ],
      [
        {
          host: "GitHub.com",
          owner: "BrightWave-Inc",
          name: "Orca",
          name_with_owner: "BrightWave-Inc/Orca",
          url: "https://github.com/BrightWave-Inc/Orca",
          tidebreak_repo_id: "repo-2",
        },
      ],
    );
    expect(armed).toEqual([
      { repoId: "repo-2", condition: "checks_failed", action: "notify" },
    ]);
  });

  it("honours an explicit scope", () => {
    const armed = triggersForNotificationRules(
      [
        rule("run_failure", {
          repositoryKeys: ["github.com/brightwave-inc/orca"],
        }),
      ],
      [repository("tidebreak", "repo-1"), repository("orca", "repo-2")],
    );
    expect(armed).toEqual([
      { repoId: "repo-2", condition: "checks_failed", action: "notify" },
    ]);
  });

  it("skips a repository triggers cannot bind to", () => {
    expect(
      triggersForNotificationRules(
        [rule("run_failure")],
        [repository("unregistered")],
      ),
    ).toEqual([]);
  });

  it("arms one row per repository and condition", () => {
    // Both rules reach `conflicts` for the same repository. The server is
    // unique on (repository, condition), so arming twice would be a no-op at
    // best and a double notification at worst.
    const armed = triggersForNotificationRules(
      [rule("pull_request_attention"), rule("pull_request_attention")],
      [repository("tidebreak", "repo-1")],
    );
    expect(armed).toHaveLength(2);
    expect(new Set(armed.map((entry) => entry.condition)).size).toBe(2);
  });
});
