/** Host checkout is absent; files live in a sandbox or its checkpoint. */
export function isRemoteWorktreePath(path: string | null | undefined): boolean {
  return path === "" || Boolean(path?.startsWith("remote:"));
}
