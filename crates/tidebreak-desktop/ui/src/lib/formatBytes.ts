/**
 * A file size for a reader, in binary units.
 *
 * Shared by the sources and outputs catalogs so a 1 MB file reads the same in
 * both — they used to disagree, one counting a kilobyte as 1,000 bytes and the
 * other as 1,024.
 */
export function formatBytes(value: number | null): string {
  if (value === null) return "—";
  if (value < 1_024) return `${value} B`;
  if (value < 1_048_576) return `${round(value / 1_024)} KB`;
  if (value < 1_073_741_824) return `${round(value / 1_048_576)} MB`;
  return `${round(value / 1_073_741_824)} GB`;
}

function round(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(
    value,
  );
}
