import type { FolderCapability } from "./host";

export function folderAccessLabel(
  capabilities: readonly FolderCapability[],
): string {
  const read = capabilities.includes("read");
  const write = capabilities.includes("write");
  return read
    ? write
      ? "Read and write"
      : "Read only"
    : write
      ? "Write only"
      : "No access";
}
