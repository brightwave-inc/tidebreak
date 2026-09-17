import type { CodeSessionDigest, CodeSessionSnapshot } from "../api/types";

export const MAX_SESSION_TREE_DEPTH = 3;
export const MAX_NESTED_SIDEBAR_CHILDREN = 8;

export type SessionTreeChild = NonNullable<
  CodeSessionSnapshot["children"]
>[number];

export function sessionTreeChildLabel(child: SessionTreeChild): string {
  const title = child.title?.trim();
  return title || "Untitled conversation";
}

export function sessionTreeStatusLabel(child: SessionTreeChild): string {
  if (child.attention && child.status !== "failed") return "Needs attention";
  switch (child.status) {
    case "running":
      return "Running";
    case "queued":
      return "Queued";
    case "completed":
      return "Completed";
    case "fenced":
      return "Paused";
    case "failed":
      return "Failed";
    case "interrupted":
      return "Interrupted";
  }
}

export function sessionTreeLocationLabel(
  location: SessionTreeChild["execution_location"],
): string | null {
  if (location === "sandbox") return "Sandbox";
  if (location === "machine") return "This machine";
  return null;
}

export function sessionTreeWaitLabel(
  wait: CodeSessionSnapshot["wait"] | CodeSessionDigest["wait"],
): string | null {
  if (!wait || wait.total < 1) return null;
  return `Waiting on ${wait.waiting} of ${wait.total}`;
}

export function sessionTreeChildHref(child: SessionTreeChild): {
  to: "/code/w/$workspaceId" | "/code/s/$sessionId";
  params: { workspaceId: string } | { sessionId: string };
  search?: { task: string };
} {
  if (child.workspace_id) {
    return {
      to: "/code/w/$workspaceId",
      params: { workspaceId: child.workspace_id },
      search: { task: child.id },
    };
  }
  return {
    to: "/code/s/$sessionId",
    params: { sessionId: child.id },
  };
}

export function nestSessionDigests(
  conversations: readonly CodeSessionDigest[],
  depth = MAX_SESSION_TREE_DEPTH,
  perParent = MAX_NESTED_SIDEBAR_CHILDREN,
): {
  roots: CodeSessionDigest[];
  childrenOf: Map<string, CodeSessionDigest[]>;
} {
  const byId = new Map(
    conversations.map((digest) => [digest.session, digest] as const),
  );
  const childrenOf = new Map<string, CodeSessionDigest[]>();
  const nested = new Set<string>();
  for (const digest of conversations) {
    const parent = digest.parent_session;
    if (!parent || !byId.has(parent) || parent === digest.session) continue;
    const listed = childrenOf.get(parent) ?? [];
    if (listed.length >= perParent) continue;
    listed.push(digest);
    childrenOf.set(parent, listed);
    nested.add(digest.session);
  }
  const roots = conversations.filter((digest) => !nested.has(digest.session));
  pruneDepth(childrenOf, roots, depth);
  return { roots, childrenOf };
}

function pruneDepth(
  childrenOf: Map<string, CodeSessionDigest[]>,
  roots: readonly CodeSessionDigest[],
  depth: number,
): void {
  const keep = new Set<string>();
  const walk = (id: string, remaining: number) => {
    if (remaining <= 0) return;
    keep.add(id);
    for (const child of childrenOf.get(id) ?? []) {
      walk(child.session, remaining - 1);
    }
  };
  for (const root of roots) walk(root.session, depth);
  for (const [parent, children] of [...childrenOf.entries()]) {
    if (!keep.has(parent)) {
      childrenOf.delete(parent);
      continue;
    }
    childrenOf.set(
      parent,
      children.filter((child) => keep.has(child.session)),
    );
  }
}
