import { Badge } from "@/components/ui/badge";
import type { CodeWorkspaceTree } from "../api/types";

type Revision = NonNullable<CodeWorkspaceTree["revision"]>;

/** Live sandbox vs retained checkpoint. Host worktrees omit this chip. */
export function WorkspaceRevisionChip({
  revision,
  revisionRef,
}: {
  revision?: Revision;
  revisionRef?: string;
}) {
  if (!revision) return null;
  const label = revision === "live" ? "Live sandbox" : "Retained checkpoint";
  return (
    <Badge
      variant={revision === "live" ? "info" : "secondary"}
      size="sm"
      className="font-normal"
      title={revisionRef}
    >
      {label}
    </Badge>
  );
}
