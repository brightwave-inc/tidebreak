import { Button } from "@/components/ui/button";
import {
  CheckTone,
  DetailSheet,
  DetailSkeleton,
  relativeTime,
} from "../PullRequestDetail";
import type {
  CodeDeliveryRunAction,
  CodeDeliveryRunDetail,
  CodeDeliveryRunSummary,
} from "../../api/types";
import {
  DetailStat,
  InlineLoadError,
  PartialErrorBanner,
  RunStateText,
} from "./status";
import {
  ExternalLink,
  GitBranch,
  LoaderCircle,
  RefreshCw,
  X,
} from "lucide-react";
import { codeDeliveryRepositoryTarget } from "../CodeDeliveryStore";
import { friendlyErrorMessage } from "@/lib/utils";
import { humanize, runBucket } from "./helpers";
import { openInBrowser } from "@/openInBrowser";
import { toast } from "sonner";
import { useApp } from "@/AppContext";
import { useEffect, useRef, useState } from "react";

export function RunDetailSheet({
  summary,
  initialDetail,
  onClose,
  onChanged,
  onOpenWorkspace,
}: {
  summary: CodeDeliveryRunSummary;
  initialDetail?: CodeDeliveryRunDetail;
  onClose: () => void;
  onChanged: () => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const { client } = useApp();
  const [detail, setDetail] = useState<CodeDeliveryRunDetail | null>(
    initialDetail ?? null,
  );
  const [loading, setLoading] = useState(!initialDetail);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"all" | "failed" | null>(null);
  const generation = useRef(0);
  const activeTarget = useRef(summary.id);
  const mounted = useRef(true);
  activeTarget.current = summary.id;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const targetIsActive = (targetId: string) =>
    mounted.current && activeTarget.current === targetId;

  const load = async () => {
    const targetId = summary.id;
    const token = ++generation.current;
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeDeliveryRunDetail({
        repository: codeDeliveryRepositoryTarget(summary.repository),
        kind: summary.kind,
        id: summary.github_id,
      });
      if (token === generation.current && targetIsActive(targetId)) {
        setDetail(next);
      }
    } catch (caught) {
      if (token === generation.current && targetIsActive(targetId)) {
        setError(friendlyErrorMessage(caught, "Could not load this run."));
      }
    } finally {
      if (token === generation.current && targetIsActive(targetId)) {
        setLoading(false);
      }
    }
  };

  useEffect(() => {
    if (initialDetail?.summary.id === summary.id) {
      setDetail(initialDetail);
      setLoading(false);
    } else {
      setDetail(null);
      void load();
    }
    return () => {
      generation.current += 1;
    };
  }, [client, initialDetail, summary.id]);

  const rerun = async (kind: "all" | "failed") => {
    if (busy) return;
    const targetId = summary.id;
    setBusy(kind);
    const action: CodeDeliveryRunAction = {
      type: kind === "all" ? "rerun" : "rerun_failed",
    };
    try {
      const result = await client.runCodeDeliveryRunAction({
        target: {
          repository: codeDeliveryRepositoryTarget(summary.repository),
          kind: summary.kind,
          id: summary.github_id,
        },
        action,
      });
      if (!targetIsActive(targetId)) return;
      if (result.success) {
        toast.success(result.message);
      } else {
        toast.warning(result.message);
      }
      onChanged();
      await load();
    } catch (caught) {
      if (!targetIsActive(targetId)) return;
      toast.error(
        friendlyErrorMessage(
          caught,
          kind === "all"
            ? "Could not rerun this workflow."
            : "Could not rerun failed jobs.",
        ),
      );
    } finally {
      if (targetIsActive(targetId)) setBusy(null);
    }
  };

  return (
    <DetailSheet
      label={`${summary.kind === "deployment" ? "Deployment" : "Action"}: ${summary.name}`}
      onClose={onClose}
    >
      <div className="flex shrink-0 items-start gap-3 border-b border-border-subtle px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>{summary.repository.name_with_owner}</span>
            <span>
              {summary.kind === "deployment" ? "Deployment" : "Action"}
            </span>
          </div>
          <h2 className="mt-1 text-base font-semibold leading-snug">
            {summary.name}
          </h2>
        </div>
        <Button type="button" size="icon-xs" variant="ghost" onClick={onClose}>
          <X />
          <span className="sr-only">Close run details</span>
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-5">
        {loading && !detail ? (
          <DetailSkeleton />
        ) : error ? (
          <InlineLoadError message={error} onRetry={() => void load()} />
        ) : detail ? (
          <div className="flex flex-col gap-5">
            {detail.errors.length > 0 && (
              <PartialErrorBanner errors={detail.errors} compact />
            )}
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => void openInBrowser(detail.summary.url)}
              >
                <ExternalLink />
                Open on GitHub
              </Button>
              {detail.summary.kind === "workflow_run" &&
                detail.summary.status === "completed" && (
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={Boolean(busy)}
                    onClick={() => void rerun("all")}
                  >
                    {busy === "all" ? (
                      <LoaderCircle className="animate-spin" />
                    ) : (
                      <RefreshCw />
                    )}
                    Rerun all
                  </Button>
                )}
              {detail.can_rerun_failed && (
                <Button
                  type="button"
                  size="sm"
                  disabled={Boolean(busy)}
                  onClick={() => void rerun("failed")}
                >
                  {busy === "failed" ? (
                    <LoaderCircle className="animate-spin" />
                  ) : (
                    <RefreshCw />
                  )}
                  Rerun failed
                </Button>
              )}
              {detail.summary.workspace_links.map((workspace) => (
                <Button
                  key={workspace.workspace_id}
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => onOpenWorkspace(workspace.workspace_id)}
                >
                  <GitBranch />
                  Open {workspace.title}
                </Button>
              ))}
            </div>

            <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
              <DetailStat
                label="Status"
                value={humanize(detail.summary.status)}
              />
              <DetailStat
                label="Conclusion"
                value={
                  detail.summary.conclusion
                    ? humanize(detail.summary.conclusion)
                    : "Pending"
                }
              />
              <DetailStat
                label="Workflow"
                value={detail.summary.workflow ?? detail.summary.name}
              />
              <DetailStat
                label="Environment"
                value={detail.summary.environment ?? "None"}
              />
              <DetailStat
                label="Branch"
                value={detail.summary.branch ?? "Unknown"}
                mono
              />
              <DetailStat
                label="Event"
                value={
                  detail.summary.event
                    ? humanize(detail.summary.event)
                    : "Unknown"
                }
              />
            </dl>

            {detail.jobs.length > 0 && (
              <section>
                <h3 className="text-sm font-medium">Jobs</h3>
                <div className="mt-2 flex flex-col rounded-lg border border-border-subtle">
                  {detail.jobs.map((job) => (
                    <button
                      key={job.id}
                      type="button"
                      className="flex items-start gap-2 border-b border-border-subtle px-3 py-2.5 text-left last:border-b-0 hover:bg-muted/30"
                      onClick={() => void openInBrowser(job.url)}
                    >
                      <CheckTone
                        bucket={runBucket(job.conclusion, job.status)}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-xs font-medium">
                          {job.name}
                        </span>
                        {job.failed_steps.length > 0 && (
                          <span className="mt-1 block text-xs text-critical">
                            {job.failed_steps.join(", ")}
                          </span>
                        )}
                      </span>
                      <ExternalLink className="size-3.5 text-muted-foreground" />
                    </button>
                  ))}
                </div>
              </section>
            )}

            {detail.deployment_statuses.length > 0 && (
              <section>
                <h3 className="text-sm font-medium">Deployment history</h3>
                <div className="mt-2 flex flex-col gap-2">
                  {detail.deployment_statuses.map((status) => (
                    <div
                      key={status.id}
                      className="rounded-lg border border-border-subtle px-3 py-2.5"
                    >
                      <div className="flex items-center justify-between gap-2">
                        <RunStateText value={status.state} />
                        <span className="text-xs text-muted-foreground">
                          {relativeTime(status.created_at)}
                        </span>
                      </div>
                      {status.description && (
                        <p className="mt-1 text-xs text-muted-foreground">
                          {status.description}
                        </p>
                      )}
                      <div className="mt-2 flex gap-2">
                        {status.environment_url && (
                          <Button
                            type="button"
                            size="xs"
                            variant="outline"
                            onClick={() =>
                              void openInBrowser(status.environment_url!)
                            }
                          >
                            <ExternalLink />
                            Environment
                          </Button>
                        )}
                        {status.log_url && (
                          <Button
                            type="button"
                            size="xs"
                            variant="outline"
                            onClick={() => void openInBrowser(status.log_url!)}
                          >
                            Logs
                          </Button>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              </section>
            )}
          </div>
        ) : null}
      </div>
    </DetailSheet>
  );
}
