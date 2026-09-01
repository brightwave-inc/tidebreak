import type {
  CodeActionSnapshot,
  CodeCheckLogsSnapshot,
  CodeCommitSnapshot,
  CodeDeliveryPullRequestTarget,
  CodePrCommentsSnapshot,
  CodePrMergeMethod,
  CodePushSnapshot,
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
  CodeWatchSnapshot,
  CodeWorkspaceDiff,
  CodeWorkspacePrSnapshot,
  CodeWorkspacePullRequests,
  PullRequestDigest,
} from "../types";
import { type Constructor, HttpCore, HttpError, requireParsed } from "./http";
import {
  parseCodeAction,
  parseCodeCheckLogsSnapshot,
  parseCodeCommit,
  parseCodePrComments,
  parseCodePush,
  parseCodeTrigger,
  parseCodeTriggers,
  parseCodeWatch,
  parseCodeWorkspaceDiff,
  parseCodeWorkspacePr,
  parseCodeWorkspacePullRequests,
} from "../../code/parsers";

export type CodeWorkspaceMergeRequest = {
  target: CodeDeliveryPullRequestTarget;
  expected_head_sha: string;
  method: CodePrMergeMethod;
  auto?: boolean;
};

type CodeWorkspaceMergeResult = {
  target: CodeDeliveryPullRequestTarget;
  accepted_head_sha: string;
  status: CodeWorkspacePrSnapshot;
};

function mergeTargetFromDigest(
  pr: PullRequestDigest,
): Pick<CodeWorkspaceMergeRequest, "target" | "expected_head_sha"> | null {
  if (!pr.url || !pr.head_sha) return null;
  let url: URL;
  try {
    url = new URL(pr.url);
  } catch {
    return null;
  }
  const parts = url.pathname.split("/").filter(Boolean);
  if (
    parts.length !== 4 ||
    parts[2] !== "pull" ||
    Number(parts[3]) !== pr.number ||
    !url.hostname
  ) {
    return null;
  }
  return {
    target: {
      repository: {
        host: url.hostname,
        owner: parts[0],
        name: parts[1],
      },
      number: pr.number,
    },
    expected_head_sha: pr.head_sha,
  };
}

function parseCodeWorkspaceMergeResult(
  value: unknown,
  expected: CodeWorkspaceMergeRequest,
): CodeWorkspaceMergeResult {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("code pull request merge response contains invalid data");
  }
  const record = value as Record<string, unknown>;
  const target = record.target as
    | {
        repository?: { host?: unknown; owner?: unknown; name?: unknown };
        number?: unknown;
      }
    | undefined;
  const repository = target?.repository;
  const host = repository?.host;
  const owner = repository?.owner;
  const name = repository?.name;
  const acceptedHead = record.accepted_head_sha;
  const status = parseCodeWorkspacePr(record.status);
  if (
    !repository ||
    typeof host !== "string" ||
    typeof owner !== "string" ||
    typeof name !== "string" ||
    typeof target.number !== "number" ||
    typeof acceptedHead !== "string" ||
    !status ||
    !sameRepository({ host, owner, name }, expected.target.repository) ||
    target.number !== expected.target.number ||
    acceptedHead !== expected.expected_head_sha
  ) {
    throw new Error("code pull request merge response contains invalid data");
  }
  return {
    target: {
      repository: {
        host,
        owner,
        name,
      },
      number: target.number,
    },
    accepted_head_sha: acceptedHead,
    status,
  };
}

function sameRepository(
  left: { host: string; owner: string; name: string },
  right: { host: string; owner: string; name: string },
): boolean {
  return (
    left.host.toLowerCase() === right.host.toLowerCase() &&
    left.owner.toLowerCase() === right.owner.toLowerCase() &&
    left.name.toLowerCase() === right.name.toLowerCase()
  );
}

/** Commit, push, pull requests, triggers, watches, merges, and diffs. */
export function withCodeGitApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    private readonly workspaceMergeTargets = new Map<
      string,
      Pick<CodeWorkspaceMergeRequest, "target" | "expected_head_sha">
    >();

    async commitCodeWorkspace(
      workspaceId: string,
      message?: string,
    ): Promise<CodeCommitSnapshot> {
      return requireParsed(
        parseCodeCommit(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/git/commit`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(message ? { message } : {}),
            },
          ),
        ),
        "code commit",
      );
    }

    async pushCodeWorkspace(workspaceId: string): Promise<CodePushSnapshot> {
      return requireParsed(
        parseCodePush(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/git/push`,
            { method: "POST", headers: this.headers() },
          ),
        ),
        "code push",
      );
    }

    async createCodePullRequest(
      workspaceId: string,
      body: { title?: string; body?: string } = {},
    ): Promise<CodeWorkspacePrSnapshot> {
      const snapshot = requireParsed(
        parseCodeWorkspacePr(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/git/pr`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
          ),
        ),
        "code pull request",
      );
      this.rememberWorkspaceMergeTarget(workspaceId, snapshot);
      return snapshot;
    }

    async getCodeWorkspacePr(
      workspaceId: string,
    ): Promise<CodeWorkspacePrSnapshot> {
      const snapshot = requireParsed(
        parseCodeWorkspacePr(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/pr`,
            {
              headers: this.headers(),
            },
          ),
        ),
        "code pull request",
      );
      this.rememberWorkspaceMergeTarget(workspaceId, snapshot);
      return snapshot;
    }

    /** Every pull request attributed to the workspace (decision 77). */
    async getCodeWorkspacePullRequests(
      workspaceId: string,
    ): Promise<CodeWorkspacePullRequests> {
      return requireParsed(
        parseCodeWorkspacePullRequests(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/pull-requests`,
            {
              headers: this.headers(),
            },
          ),
        ),
        "code workspace pull requests",
      );
    }

    /** Force a fresh host read, bypassing the server's short PR cache. */
    async refreshCodeWorkspacePr(
      workspaceId: string,
    ): Promise<CodeWorkspacePrSnapshot> {
      const snapshot = requireParsed(
        parseCodeWorkspacePr(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/pr/refresh`,
            { method: "POST", headers: this.headers() },
          ),
        ),
        "code pull request",
      );
      this.rememberWorkspaceMergeTarget(workspaceId, snapshot);
      return snapshot;
    }

    /**
     * Download the failing checks' job logs into private storage.
     *
     * The fix-errors action calls this before it prompts, so the agent opens a
     * file instead of working out which job failed and asking GitHub itself.
     * Each call replaces the previous set: only the head being fixed matters.
     */
    async writeCodeCheckLogs(
      workspaceId: string,
    ): Promise<CodeCheckLogsSnapshot> {
      return requireParsed(
        parseCodeCheckLogsSnapshot(
          // A GitHub read, so it takes the delivery timeout rather than hanging
          // the button on a `gh` that never answers.
          await this.deliveryJson(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/pr/check-logs`,
            { method: "POST", headers: this.headers() },
          ),
        ),
        "code check logs",
      );
    }

    /** Triggers armed on a repository, enabled or not. */
    async listCodeTriggers(repoId: string): Promise<CodeTriggerSnapshot[]> {
      return requireParsed(
        parseCodeTriggers(
          await this.json(
            `/code/repos/${encodeURIComponent(repoId)}/triggers`,
            {
              headers: this.headers(),
            },
          ),
        ),
        "code triggers",
      );
    }

    /**
     * Arm a trigger. Re-arming a condition updates the rule already there
     * rather than adding a second one, so this is safe to call twice.
     */
    async createCodeTrigger(
      repoId: string,
      condition: CodeTriggerCondition,
      action: CodeTriggerAction,
    ): Promise<CodeTriggerSnapshot> {
      return requireParsed(
        parseCodeTrigger(
          await this.json(
            `/code/repos/${encodeURIComponent(repoId)}/triggers`,
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ condition, action }),
            },
          ),
        ),
        "code trigger",
      );
    }

    /** Switch a trigger on or off. The rule survives either way. */
    async setCodeTriggerEnabled(
      repoId: string,
      triggerId: string,
      enabled: boolean,
    ): Promise<CodeTriggerSnapshot> {
      return requireParsed(
        parseCodeTrigger(
          await this.json(
            `/code/repos/${encodeURIComponent(repoId)}/triggers/${encodeURIComponent(triggerId)}`,
            {
              method: "PATCH",
              headers: this.headers(true),
              body: JSON.stringify({ enabled }),
            },
          ),
        ),
        "code trigger",
      );
    }

    /** Remove a trigger. Its recorded fires stay in the transcript. */
    deleteCodeTrigger(repoId: string, triggerId: string): Promise<void> {
      return this.json(
        `/code/repos/${encodeURIComponent(repoId)}/triggers/${encodeURIComponent(triggerId)}`,
        { method: "DELETE", headers: this.headers() },
      );
    }

    /** Start a durable watch on the workspace's open pull request. */
    async startCodeWatch(workspaceId: string): Promise<CodeWatchSnapshot> {
      return requireParsed(
        parseCodeWatch(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/watch`,
            { method: "POST", headers: this.headers() },
          ),
        ),
        "code watch",
      );
    }

    /** Stop the workspace's active watch and end its session. */
    async stopCodeWatch(workspaceId: string): Promise<CodeWatchSnapshot> {
      return requireParsed(
        parseCodeWatch(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/watch`,
            { method: "DELETE", headers: this.headers() },
          ),
        ),
        "code watch",
      );
    }

    async getCodePrComments(
      workspaceId: string,
    ): Promise<CodePrCommentsSnapshot> {
      return requireParsed(
        parseCodePrComments(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/pr/comments`,
            { headers: this.headers() },
          ),
        ),
        "code pull request comments",
      );
    }

    /**
     * User-initiated draft-to-ready. Decision 42 keeps pull-request state
     * changes off the agent path, so this is a dedicated endpoint rather than a
     * prompt. Returns the post-change snapshot.
     */
    async markCodePrReady(
      workspaceId: string,
    ): Promise<CodeWorkspacePrSnapshot> {
      const snapshot = requireParsed(
        parseCodeWorkspacePr(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/pr/ready`,
            { method: "POST", headers: this.headers(true) },
          ),
        ),
        "code pull request",
      );
      this.rememberWorkspaceMergeTarget(workspaceId, snapshot);
      return snapshot;
    }

    /**
     * User-initiated merge. auto=true arms host auto-merge instead of merging
     * immediately. Returns the post-merge snapshot.
     */
    async mergeCodePr(
      workspaceId: string,
      body:
        | CodeWorkspaceMergeRequest
        | { method: CodePrMergeMethod; auto?: boolean },
    ): Promise<CodeWorkspacePrSnapshot> {
      const exact =
        "target" in body && "expected_head_sha" in body
          ? {
              target: body.target,
              expected_head_sha: body.expected_head_sha,
            }
          : this.workspaceMergeTargets.get(workspaceId);
      if (!exact) {
        throw new HttpError(
          409,
          "409: Refresh the pull request before merging it.",
          "pr_identity_missing",
        );
      }
      const request: CodeWorkspaceMergeRequest = {
        ...exact,
        method: body.method,
        auto: body.auto ?? false,
      };
      const result = parseCodeWorkspaceMergeResult(
        await this.json(
          `/code/workspaces/${encodeURIComponent(workspaceId)}/pr/merge`,
          {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify(request),
          },
        ),
        request,
      );
      this.rememberWorkspaceMergeTarget(workspaceId, result.status);
      return result.status;
    }

    private rememberWorkspaceMergeTarget(
      workspaceId: string,
      snapshot: CodeWorkspacePrSnapshot,
    ): void {
      const exact = snapshot.pr ? mergeTargetFromDigest(snapshot.pr) : null;
      if (exact) this.workspaceMergeTargets.set(workspaceId, exact);
      else this.workspaceMergeTargets.delete(workspaceId);
    }

    async runCodeWorkspaceAction(
      workspaceId: string,
      name: string,
    ): Promise<CodeActionSnapshot> {
      return requireParsed(
        parseCodeAction(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/actions/${encodeURIComponent(name)}`,
            { method: "POST", headers: this.headers() },
          ),
        ),
        "code quick action",
      );
    }

    async getCodeWorkspaceDiff(
      workspaceId: string,
      opts: { turn?: string; file?: string } = {},
    ): Promise<CodeWorkspaceDiff> {
      const params = new URLSearchParams();
      if (opts.turn) params.set("turn", opts.turn);
      if (opts.file) params.set("file", opts.file);
      const query = params.size > 0 ? `?${params.toString()}` : "";
      return requireParsed(
        parseCodeWorkspaceDiff(
          await this.json(
            `/code/workspaces/${encodeURIComponent(workspaceId)}/diff${query}`,
            { headers: this.headers() },
          ),
        ),
        "code workspace diff",
      );
    }
  };
}
