import { useEffect, useMemo, useState } from "react";

import type { ApiClient } from "../api/client";
import type { CodeWorkspaceDiff } from "../api/types";
import { cn } from "@/lib/utils";
import { friendlyErrorMessage } from "@/lib/utils";
import { DiffstatBadge } from "./TurnReviewCard";

/**
 * Server-produced unified diff, grouped per file and tinted with the
 * semantic status tokens. No client-side diff library.
 */
export function DiffPanel({
  client,
  workspaceId,
  turnId,
  file,
}: {
  client: Pick<ApiClient, "getCodeWorkspaceDiff">;
  workspaceId: string;
  turnId?: string;
  file?: string;
}) {
  const [payload, setPayload] = useState<CodeWorkspaceDiff | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const next = await client.getCodeWorkspaceDiff(workspaceId, {
          turn: turnId,
          file,
        });
        if (!cancelled) {
          setPayload(next);
          setError(null);
        }
      } catch (err) {
        if (!cancelled) {
          setError(friendlyErrorMessage(err, "Could not load the diff"));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, workspaceId, turnId, file]);

  const groups = useMemo(
    () => (payload ? groupUnifiedDiff(payload.diff) : []),
    [payload],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <header className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
        <div className="min-w-0">
          <h2 className="text-sm font-medium">Diff</h2>
          <p className="text-muted-foreground truncate text-xs">
            {file ?? (turnId ? `Turn ${turnId}` : "Workspace vs base")}
          </p>
        </div>
        {payload && <DiffstatBadge stat={payload.stat} />}
      </header>
      {error && <p className="text-critical px-3 py-2 text-sm">{error}</p>}
      {payload?.truncated && (
        <p className="text-warning-foreground bg-warning-background border-warning-border border-b px-3 py-2 text-xs">
          This diff was truncated. Open a single file for the rest.
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-auto">
        {groups.map((group) => (
          <section key={group.path} className="border-b last:border-b-0">
            <h3 className="bg-muted/40 truncate px-3 py-1.5 font-mono text-xs">
              {group.path}
            </h3>
            <pre className="py-1 text-[11px] leading-4">
              {group.lines.map((line, index) => (
                <span
                  key={`${group.path}:${index}`}
                  className={cn(
                    "block min-h-4 px-3",
                    line.startsWith("+") &&
                      !line.startsWith("+++") &&
                      "bg-success-background text-success-foreground",
                    line.startsWith("-") &&
                      !line.startsWith("---") &&
                      "bg-critical-background text-critical-foreground",
                    line.startsWith("@@") && "text-muted-foreground",
                  )}
                >
                  {line || " "}
                </span>
              ))}
            </pre>
          </section>
        ))}
        {payload && groups.length === 0 && !error && (
          <p className="text-muted-foreground px-3 py-6 text-sm">No diff.</p>
        )}
      </div>
    </div>
  );
}

export function groupUnifiedDiff(
  diff: string,
): Array<{ path: string; lines: string[] }> {
  if (!diff) return [];
  const groups: Array<{ path: string; lines: string[] }> = [];
  let current: { path: string; lines: string[] } | null = null;
  for (const line of diff.split("\n")) {
    const header = line.match(/^diff --git a\/(.+?) b\/(.+)$/);
    if (header) {
      current = { path: header[2] ?? header[1] ?? "file", lines: [line] };
      groups.push(current);
      continue;
    }
    if (!current) {
      current = { path: "diff", lines: [] };
      groups.push(current);
    }
    current.lines.push(line);
  }
  return groups.filter((group) => group.lines.length > 0);
}
