import { useState, type ReactNode } from "react";
import {
  Check,
  CircleDashed,
  ExternalLink,
  FileCode,
  Files,
  GitPullRequest,
  X,
} from "lucide-react";

import type { ApiClient } from "../api/client";
import type {
  CodeWorkspaceSnapshot,
  PullRequestCheck,
  PullRequestDigest,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import { openExternal } from "@/host";
import { DiffPanel } from "./DiffPanel";
import { FilesPanel } from "./FilesPanel";
import { PrCard } from "./PrCard";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

type InspectorTab = "files" | "diff" | "pr";

/**
 * Right workspace rail: Files, Diff, and PR as icon tabs.
 *
 * Files is the changed-file list. Diff is the worktree patch, and that
 * is where commit and push live. PR stays empty until a pull request exists.
 */
export function CodeInspector({
  client,
  workspaceId,
  workspace,
  contentRevision,
}: {
  client: ApiClient;
  workspaceId: string;
  workspace: CodeWorkspaceSnapshot | null;
  contentRevision: number;
}) {
  const digest = useCodeUpdatesStore((state) => state.byWorkspace[workspaceId]);
  const pr = digest?.pr_state ?? workspace?.pr;
  const [tab, setTab] = useState<InspectorTab>("files");
  const [file, setFile] = useState<string | undefined>();
  const active = workspace?.status !== "archived";

  function openFile(next: string) {
    setFile(next);
    setTab("diff");
  }

  return (
    <aside
      className="flex w-80 shrink-0 flex-col overflow-hidden border-l"
      aria-label="Workspace surfaces"
      data-testid="code-inspector"
    >
      <header className="flex h-12 shrink-0 items-center gap-1 border-b px-2">
        <TabButton
          label="Files"
          current={tab === "files"}
          onClick={() => setTab("files")}
        >
          <Files className="size-3.5" />
        </TabButton>
        <TabButton
          label="Diff"
          current={tab === "diff"}
          onClick={() => setTab("diff")}
        >
          <FileCode className="size-3.5" />
        </TabButton>
        <TabButton
          label="Pull request"
          current={tab === "pr"}
          onClick={() => setTab("pr")}
        >
          <GitPullRequest
            className={cn("size-3.5", prIconClass(pr?.state))}
          />
        </TabButton>
      </header>
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        {tab === "files" && (
          <FilesPanel
            client={client}
            workspaceId={workspaceId}
            contentRevision={contentRevision}
            selected={file}
            onOpenFile={openFile}
          />
        )}
        {tab === "diff" && (
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            <DiffPanel
              client={client}
              workspaceId={workspaceId}
              file={file}
              contentRevision={contentRevision}
            />
            {active && (
              <div className="border-t px-3 py-3">
                <PrCard
                  client={client}
                  workspaceId={workspaceId}
                  contentRevision={contentRevision}
                  framed={false}
                />
              </div>
            )}
          </div>
        )}
        {tab === "pr" && (
          <PrTab pr={pr} branch={workspace?.branch_name} />
        )}
      </div>
    </aside>
  );
}

function TabButton({
  label,
  current,
  onClick,
  children,
}: {
  label: string;
  current: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      aria-current={current}
      onClick={onClick}
      className={cn(
        "text-muted-foreground hover:text-foreground grid size-8 place-items-center rounded-lg",
        current && "bg-muted text-foreground",
      )}
    >
      {children}
    </button>
  );
}

function PrTab({
  pr,
  branch,
}: {
  pr?: PullRequestDigest;
  branch?: string;
}) {
  if (!pr) {
    return (
      <div className="text-muted-foreground flex flex-col gap-2 px-4 py-8 text-sm">
        <p className="text-foreground font-medium">No pull request yet</p>
        <p className="text-xs leading-relaxed">
          This tab stays quiet until one exists. Commit and push live with the
          diff, not here. When a PR is opened, status, checks, and review
          comments land in this column.
        </p>
      </div>
    );
  }

  const counts = checkCounts(pr);
  return (
    <div className="flex flex-col gap-4 overflow-y-auto px-3 py-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <GitPullRequest
              className={cn("size-4 shrink-0", prIconClass(pr.state))}
            />
            {pr.url ? (
              <a
                href={pr.url}
                className="text-foreground truncate text-sm font-semibold underline-offset-2 hover:underline"
                onClick={(event) => {
                  event.preventDefault();
                  void openExternal(pr.url!).catch(() => undefined);
                }}
              >
                {pr.title ?? `#${pr.number}`}
              </a>
            ) : (
              <p className="truncate text-sm font-semibold">
                {pr.title ?? `#${pr.number}`}
              </p>
            )}
          </div>
          <p className="text-muted-foreground mt-1 truncate text-xs">
            #{pr.number}
            {branch ? ` · ${branch}` : ""}
          </p>
        </div>
        <Badge variant={prStateVariant(pr.state)} size="sm">
          {pr.state}
        </Badge>
      </div>
      <CheckList checks={pr.checks ?? []} counts={counts} />
    </div>
  );
}

function CheckList({
  checks,
  counts,
}: {
  checks: PullRequestCheck[];
  counts: { passing: number; pending: number; failing: number };
}) {
  const [open, setOpen] = useState(checks.length > 0);
  return (
    <div className="flex flex-col gap-1">
      <button
        type="button"
        className="hover:bg-muted/50 flex w-full items-center gap-2 rounded-md px-1 py-1 text-left text-xs"
        onClick={() => setOpen((current) => !current)}
        aria-expanded={open}
      >
        <CheckCount
          icon={<Check className="size-3" />}
          count={counts.passing}
          label="passing"
          className="text-success"
        />
        <CheckCount
          icon={<CircleDashed className="size-3" />}
          count={counts.pending}
          label="pending"
          className="text-muted-foreground"
        />
        <CheckCount
          icon={<X className="size-3" />}
          count={counts.failing}
          label="failing"
          className="text-critical"
        />
      </button>
      {open &&
        checks.map((check) => (
          <CheckRow key={`${check.name}:${check.detail ?? ""}`} check={check} />
        ))}
    </div>
  );
}

function CheckCount({
  icon,
  count,
  label,
  className,
}: {
  icon: ReactNode;
  count: number;
  label: string;
  className: string;
}) {
  return (
    <span className={cn("flex items-center gap-1", className)}>
      {icon}
      {count} {label}
    </span>
  );
}

function CheckRow({ check }: { check: PullRequestCheck }) {
  const tone =
    check.bucket === "pass"
      ? "text-success"
      : check.bucket === "fail"
        ? "text-critical"
        : "text-muted-foreground";
  const Icon =
    check.bucket === "pass" ? Check : check.bucket === "fail" ? X : CircleDashed;
  const body = (
    <>
      <Icon className={cn("size-3.5 shrink-0", tone)} />
      <span className="min-w-0 flex-1 truncate">{check.name}</span>
      {check.detail && (
        <span className="text-muted-foreground shrink-0 capitalize">
          {check.detail}
        </span>
      )}
    </>
  );
  if (!check.url) {
    return (
      <div className="flex items-center gap-2 px-1 py-1 text-xs">{body}</div>
    );
  }
  return (
    <a
      href={check.url}
      className="hover:bg-muted/50 flex items-center gap-2 rounded-md px-1 py-1 text-xs"
      onClick={(event) => {
        event.preventDefault();
        void openExternal(check.url!).catch(() => undefined);
      }}
    >
      {body}
      <ExternalLink className="text-muted-foreground size-3 shrink-0" />
    </a>
  );
}

function checkCounts(pr: PullRequestDigest): {
  passing: number;
  pending: number;
  failing: number;
} {
  const checks = pr.checks ?? [];
  if (checks.length > 0) {
    return {
      passing: checks.filter((check) => check.bucket === "pass").length,
      pending: checks.filter((check) => check.bucket === "pending").length,
      failing: checks.filter((check) => check.bucket === "fail").length,
    };
  }
  const summary = pr.checks_summary ?? "";
  const passing = Number(/(\d+) passing/.exec(summary)?.[1] ?? 0);
  const pending = Number(/(\d+) pending/.exec(summary)?.[1] ?? 0);
  const failing = Number(/(\d+) failing/.exec(summary)?.[1] ?? 0);
  return { passing, pending, failing };
}

function prIconClass(state?: string): string {
  const token = state?.toLowerCase();
  if (token === "open") return "text-success";
  if (token === "merged") return "text-info";
  if (token === "closed") return "text-critical";
  return "text-muted-foreground";
}

function prStateVariant(state: string): "success" | "critical" | "info" | "outline" {
  const token = state.toLowerCase();
  if (token === "open") return "success";
  if (token === "merged") return "info";
  if (token === "closed") return "critical";
  return "outline";
}
