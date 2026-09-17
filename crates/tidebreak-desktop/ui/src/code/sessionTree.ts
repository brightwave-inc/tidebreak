import type { CodeSessionDigest, CodeSessionSnapshot } from "../api/types";
import type { StatusTone } from "./statusTone";

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

export function sessionTreeStatusTone(child: SessionTreeChild): StatusTone {
  if (child.status === "failed") return "critical";
  if (child.attention) return "warning";
  switch (child.status) {
    case "running":
      return "running";
    case "queued":
      return "pending";
    case "completed":
      return "ready";
    case "fenced":
    case "interrupted":
      return "warning";
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
  const candidates = new Map<string, CodeSessionDigest[]>();
  const roots: CodeSessionDigest[] = [];
  const childrenOf = new Map<string, CodeSessionDigest[]>();
  const visited = new Set<string>();
  for (const digest of byId.values()) {
    const parent = digest.parent_session;
    if (!parent || !byId.has(parent) || parent === digest.session) continue;
    const children = candidates.get(parent) ?? [];
    children.push(digest);
    candidates.set(parent, children);
  }
  const visit = (digest: CodeSessionDigest, level: number) => {
    visited.add(digest.session);
    if (level >= depth) return;
    const children: CodeSessionDigest[] = [];
    for (const child of candidates.get(digest.session) ?? []) {
      if (children.length >= perParent) break;
      if (visited.has(child.session)) continue;
      children.push(child);
      visit(child, level + 1);
    }
    if (children.length) childrenOf.set(digest.session, children);
  };
  const addRoot = (digest: CodeSessionDigest) => {
    if (visited.has(digest.session)) return;
    roots.push(digest);
    visit(digest, 1);
  };
  for (const digest of byId.values()) {
    if (
      !digest.parent_session ||
      !byId.has(digest.parent_session) ||
      digest.parent_session === digest.session
    ) {
      addRoot(digest);
    }
  }
  // Keep overflow and cyclic ancestry visible as additional roots.
  for (const digest of byId.values()) addRoot(digest);
  return { roots, childrenOf };
}
