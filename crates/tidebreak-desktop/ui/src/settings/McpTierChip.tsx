import type { McpCuration } from "../api";

/**
 * The two-tier honesty label: "Tested server" for a server on the curated
 * list, "Community server" for everything else. A label only — both tiers
 * mount, connect,
 * and call identically. The server decides the tier from the *saved*
 * definition, so an unsaved edit keeps the previous row's label until Save.
 */
export function McpTierChip({ curated }: { curated: McpCuration | null }) {
  const tested = curated !== null;
  return (
    <span
      className="self-start text-xs text-muted-foreground"
      title={
        tested
          ? `${curated.display_name} — exercised end to end on ${curated.tested_on}. ${curated.notes}`
          : "Not on Tidebreak's tested list. It still mounts and runs; we have not driven this server ourselves."
      }
    >
      {tested ? "Tested server" : "Community server"}
    </span>
  );
}
