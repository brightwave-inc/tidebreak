import type { MemoryStatus } from "./api";

/**
 * The Badge tone for one memory lifecycle state.
 *
 * Shared by the Memory settings panel and the transcript's proposal card so
 * the two surfaces cannot drift: an active record reads as success and a
 * rejected one as critical wherever it appears.
 */
export function memoryStatusVariant(
  status: MemoryStatus,
): "success" | "warning" | "secondary" | "critical" {
  if (status === "active") return "success";
  if (status === "proposed") return "warning";
  if (status === "rejected") return "critical";
  return "secondary";
}
