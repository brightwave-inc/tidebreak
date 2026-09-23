import type { McpCuration } from "../api";

/**
 * The two-tier honesty label: "Tested" for a server on the curated list,
 * "Community" for everything else. A label only — both tiers mount, connect,
 * and call identically. The server decides the tier from the *saved*
 * definition, so an unsaved edit keeps the previous row's label until Save.
 */
export function McpTierChip({ curated }: { curated: McpCuration | null }) {
  const tested = curated !== null;
  return (
    <span
      className={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs ${
        tested
          ? "border-success-border/40 text-success-foreground"
          : "text-muted-foreground"
      }`}
      title={
        tested
          ? `${curated.display_name} — exercised end to end on ${curated.tested_on}. ${curated.notes}`
          : "Not on Tidebreak's tested list. It still mounts and runs; we have not driven this server ourselves."
      }
    >
      {tested ? "Tested" : "Community"}
    </span>
  );
}
