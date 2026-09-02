import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

/**
 * Header chip that counts pending memory proposals for this session and
 * links to the memory manager.
 */
export function MemoryProposalChip({
  count,
  className,
}: {
  count: number | null | undefined;
  className?: string;
}) {
  if (!count) return null;
  const label = count === 1 ? "1 memory proposal" : `${count} memory proposals`;
  return (
    // The app routes on hash history, so a plain path would leave the
    // router; the hash form is what the settings links already use.
    <a
      href="#/settings/memory"
      className={cn("shrink-0", className)}
      aria-label={label}
    >
      <Badge variant="info" size="sm">
        {label}
      </Badge>
    </a>
  );
}
