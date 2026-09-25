import { useNavigate } from "@tanstack/react-router";
import { Button } from "@/components/ui/button";
import type { CodeSessionSnapshot } from "../../api/types";
import { cn } from "@/lib/utils";
import {
  sessionTreeChildHref,
  sessionTreeChildLabel,
  sessionTreeLocationLabel,
  sessionTreeStatusLabel,
  sessionTreeStatusTone,
  sessionTreeWaitLabel,
  type SessionTreeChild,
} from "../sessionTree";
import { STATUS_TEXT } from "../statusTone";

export function CodeSessionTree({
  nodes,
  wait,
}: {
  nodes: NonNullable<CodeSessionSnapshot["children"]>;
  wait: CodeSessionSnapshot["wait"];
}) {
  const navigate = useNavigate();
  if (nodes.length === 0 && !wait) return null;
  const waitLabel = sessionTreeWaitLabel(wait);
  return (
    <section
      aria-label="Child conversations"
      className="border-border-subtle mx-auto mt-3 w-[calc(100%-2rem)] max-w-3xl rounded-lg border px-3 py-2"
      data-testid="session-tree"
    >
      {/* In a narrow pane the wait count drops below the heading whole,
          rather than breaking "Waiting on 6 of 6" across two lines. */}
      <div className="flex flex-wrap items-baseline justify-between gap-x-2">
        <h2 className="text-xs font-medium text-muted-foreground">
          Child conversations
        </h2>
        {waitLabel && (
          <p className={cn("text-xs whitespace-nowrap", STATUS_TEXT.pending)}>
            {waitLabel}
          </p>
        )}
      </div>
      {nodes.length === 0 ? (
        <p className="text-muted-foreground mt-1 text-sm">
          No child conversations yet.
        </p>
      ) : (
        <ul className="mt-1 flex flex-col gap-1">
          {nodes.map((child) => (
            <SessionTreeRow
              key={child.id}
              child={child}
              onOpen={() => {
                const href = sessionTreeChildHref(child);
                void navigate(href);
              }}
            />
          ))}
        </ul>
      )}
    </section>
  );
}

function SessionTreeRow({
  child,
  onOpen,
}: {
  child: SessionTreeChild;
  onOpen: () => void;
}) {
  const location = sessionTreeLocationLabel(child.execution_location);
  const status = sessionTreeStatusLabel(child);
  return (
    <li className="flex min-w-0 items-center gap-2 py-1">
      <div className="min-w-0 flex-1">
        <p
          className="truncate text-sm text-foreground"
          title={sessionTreeChildLabel(child)}
        >
          {sessionTreeChildLabel(child)}
        </p>
        <p className="text-muted-foreground truncate text-xs">
          <span className={STATUS_TEXT[sessionTreeStatusTone(child)]}>
            {status}
          </span>
          {location && ` · ${location}`}
        </p>
      </div>
      <Button
        type="button"
        size="sm"
        variant="ghost"
        onClick={onOpen}
        aria-label={`Open ${sessionTreeChildLabel(child)}`}
      >
        Open
      </Button>
    </li>
  );
}
