import { DiffstatBadge } from "tidebreak-desktop-ui";

export function TypicalChange() {
  return (
    <DiffstatBadge
      stat={{ files: 6, insertions: 128, deletions: 41, truncated: false }}
    />
  );
}

export function SingleFile() {
  return (
    <DiffstatBadge
      stat={{ files: 1, insertions: 3, deletions: 0, truncated: false }}
    />
  );
}

export function TruncatedDiff() {
  return (
    <DiffstatBadge
      stat={{ files: 214, insertions: 9805, deletions: 7712, truncated: true }}
    />
  );
}
