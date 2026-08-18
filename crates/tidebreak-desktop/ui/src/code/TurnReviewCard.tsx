import type { Diffstat } from "../api/types";
import { Badge } from "@/components/ui/badge";

export function DiffstatBadge({ stat }: { stat: Diffstat }) {
  return (
    <Badge variant="outline" size="sm">
      {stat.files} file{stat.files === 1 ? "" : "s"} +{stat.insertions} −
      {stat.deletions}
      {stat.truncated ? " · truncated" : ""}
    </Badge>
  );
}
