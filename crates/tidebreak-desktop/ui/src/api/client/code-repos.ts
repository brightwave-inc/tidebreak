import type {
  CodeAnalyticsRange,
  CodeAnalyticsSnapshot,
  CodeCloneDefaults,
  CodeCloneJobSnapshot,
  CodeGithubRepositories,
  CodeHarnessInstallSnapshot,
  CodeRepoSnapshot,
  CodeRepoSources,
  CodeSubscriptionUsage,
  CodeWorktreeRoot,
  HarnessDoctorReport,
  HarnessKind,
  QuickAction,
} from "../types";
import {
  parseCodeAnalytics,
  parseCodeCloneDefaults,
  parseCodeCloneJob,
  parseCodeGithubRepositories,
  parseCodeHarnessInstall,
  parseCodeRepo,
  parseCodeRepoSources,
  parseCodeSubscriptionUsage,
  parseCodeWorktreeRoot,
  type ParsedHarnessModelList,
  parseHarnessDoctorReport,
  parseHarnessModelList,
} from "../../code/parsers";
import { type Constructor, HttpCore, parseList, requireParsed } from "./http";

/** Code repositories, clones, worktree roots, harness doctor, usage, and analytics. */
export function withCodeReposApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async listCodeRepos(): Promise<CodeRepoSnapshot[]> {
      const body = await this.json<unknown>("/code/repos", {
        headers: this.headers(),
      });
      return parseList(body, parseCodeRepo, "code repos");
    }

    async createCodeRepo(body: {
      path: string;
      display_name?: string;
      default_base_ref?: string;
      branch_prefix?: string;
      setup_script?: string;
      archive_script?: string;
      quick_actions?: QuickAction[];
    }): Promise<CodeRepoSnapshot> {
      return requireParsed(
        parseCodeRepo(
          await this.json("/code/repos", {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify(body),
          }),
        ),
        "code repo",
      );
    }

    async getCodeRepo(repoId: string): Promise<CodeRepoSnapshot> {
      return requireParsed(
        parseCodeRepo(
          await this.json(`/code/repos/${encodeURIComponent(repoId)}`, {
            headers: this.headers(),
          }),
        ),
        "code repo",
      );
    }

    async patchCodeRepo(
      repoId: string,
      body: {
        display_name?: string;
        default_base_ref?: string;
        branch_prefix?: string;
        setup_script?: string | null;
        archive_script?: string | null;
        /** The whole list, replaced. Omit to leave the stored list alone. */
        quick_actions?: QuickAction[];
      },
    ): Promise<CodeRepoSnapshot> {
      return requireParsed(
        parseCodeRepo(
          await this.json(`/code/repos/${encodeURIComponent(repoId)}`, {
            method: "PATCH",
            headers: this.headers(true),
            body: JSON.stringify(body),
          }),
        ),
        "code repo",
      );
    }

    deleteCodeRepo(repoId: string): Promise<void> {
      return this.json(`/code/repos/${encodeURIComponent(repoId)}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    /**
     * What this machine can add a repository from.
     *
     * Member-plane, unlike `getCodeCloneDefaults`, which is administrator-only:
     * on a shared machine the person adding a repository is usually not an
     * administrator, and a dialog that cannot read its own options is no dialog.
     */
    async listCodeGithubRepositories(
      signal?: AbortSignal,
    ): Promise<CodeGithubRepositories> {
      return requireParsed(
        parseCodeGithubRepositories(
          await this.json("/code/repos/github", {
            headers: this.headers(),
            signal,
          }),
        ),
        "github repositories",
      );
    }

    async getCodeRepoSources(signal?: AbortSignal): Promise<CodeRepoSources> {
      return requireParsed(
        parseCodeRepoSources(
          await this.json("/code/repos/sources", {
            headers: this.headers(),
            signal,
          }),
        ),
        "repo sources",
      );
    }

    async getCodeCloneDefaults(
      signal?: AbortSignal,
    ): Promise<CodeCloneDefaults> {
      return requireParsed(
        parseCodeCloneDefaults(
          await this.json("/code/repos/clone-defaults", {
            headers: this.headers(),
            signal,
          }),
        ),
        "clone defaults",
      );
    }

    /**
     * Start a clone. `parent_dir` is omitted when the machine places clones
     * itself — see `CodeRepoSources.chooses_destination`.
     */
    async startCodeClone(body: {
      url?: string;
      github?: string;
      parent_dir?: string;
      name?: string;
    }): Promise<CodeCloneJobSnapshot> {
      return requireParsed(
        parseCodeCloneJob(
          await this.json("/code/repos/clone", {
            method: "POST",
            headers: this.headers(true),
            body: JSON.stringify(body),
          }),
        ),
        "clone job",
      );
    }

    async getCodeWorktreeRoot(): Promise<CodeWorktreeRoot> {
      return requireParsed(
        parseCodeWorktreeRoot(
          await this.json("/code/worktree-root", { headers: this.headers() }),
        ),
        "worktree root",
      );
    }

    /**
     * Move the root new worktrees are created under. A null root restores the
     * default. Worktrees already on disk keep the path they were created at.
     */
    async setCodeWorktreeRoot(root: string | null): Promise<CodeWorktreeRoot> {
      return requireParsed(
        parseCodeWorktreeRoot(
          await this.json("/code/worktree-root", {
            method: "PUT",
            headers: this.headers(true),
            body: JSON.stringify({ root }),
          }),
        ),
        "worktree root",
      );
    }

    async getCodeCloneJob(jobId: string): Promise<CodeCloneJobSnapshot> {
      return requireParsed(
        parseCodeCloneJob(
          await this.json(`/code/repos/clone/${encodeURIComponent(jobId)}`, {
            headers: this.headers(),
          }),
        ),
        "clone job",
      );
    }

    async getHarnessDoctor(): Promise<HarnessDoctorReport> {
      return requireParsed(
        parseHarnessDoctorReport(
          await this.json("/code/harnesses", { headers: this.headers() }),
        ),
        "harness doctor",
      );
    }

    async getCodeSubscriptionUsage(): Promise<CodeSubscriptionUsage> {
      return requireParsed(
        parseCodeSubscriptionUsage(
          await this.json("/code/usage", { headers: this.headers() }),
        ),
        "subscription usage",
      );
    }

    async getCodeAnalytics(
      range: CodeAnalyticsRange,
      repoId?: string,
    ): Promise<CodeAnalyticsSnapshot> {
      const query = new URLSearchParams({ range });
      if (repoId) query.set("repo_id", repoId);
      return requireParsed(
        parseCodeAnalytics(
          await this.json(`/code/analytics?${query.toString()}`, {
            headers: this.headers(),
          }),
        ),
        "code analytics",
      );
    }

    async listCodeHarnessModels(
      kind: HarnessKind,
    ): Promise<ParsedHarnessModelList> {
      return requireParsed(
        parseHarnessModelList(
          await this.json(
            `/code/harnesses/${encodeURIComponent(kind)}/models`,
            {
              headers: this.headers(),
            },
          ),
        ),
        "harness models",
      );
    }

    /**
     * Warm the pinned install of one engine ahead of a session create.
     *
     * Answers as soon as the server knows where the install stands; the phases
     * that follow arrive on `WS /code/updates`. Safe to call repeatedly — an
     * installed pin answers `ready` and one already running is not restarted.
     */
    /**
     * Start — or report — the pinned download of one engine.
     *
     * `deliberate` marks a reader who pressed Install rather than a picker
     * warming its selection. Only that case retries a managed-Node install that
     * already failed.
     */
    async startHarnessInstall(
      kind: HarnessKind,
      deliberate = false,
    ): Promise<CodeHarnessInstallSnapshot> {
      const path = `/code/harnesses/${encodeURIComponent(kind)}/install`;
      return requireParsed(
        parseCodeHarnessInstall(
          await this.json(deliberate ? `${path}?deliberate=true` : path, {
            method: "POST",
            headers: this.headers(),
          }),
        ),
        "harness install",
      );
    }

    async refreshHarnessDoctor(): Promise<HarnessDoctorReport> {
      return requireParsed(
        parseHarnessDoctorReport(
          await this.json("/code/harnesses/refresh", {
            method: "POST",
            headers: this.headers(),
          }),
        ),
        "harness doctor",
      );
    }
  };
}
