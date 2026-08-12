import type { ConsentStatementSnapshot } from "./api";

/** The part of a conversation a folder statement is matched against. */
export type FolderChatScope = { id: string; project_id: string | null };

/**
 * The broker's capability vocabulary as consent statements carry it. Folder
 * access is read off the same statements the Permissions surface shows —
 * there is deliberately no folder-capability model of the app's own left to
 * drift from what the broker holds.
 */
export type FolderReach = Extract<
  ConsentStatementSnapshot["verb"],
  { kind: "capability" }
>["capability"];

/** Whether a statement's level covers this chat: the chat itself, or the
 * project it is filed under. */
export function statementReachesChat(
  statement: ConsentStatementSnapshot,
  chat: FolderChatScope,
): boolean {
  return statement.level.level === "chat"
    ? statement.level.chat_id === chat.id
    : statement.level.project_id === chat.project_id;
}

/** The capability statements naming one connected folder, for this chat. */
export function folderStatements(
  statements: readonly ConsentStatementSnapshot[],
  rootId: string,
  chat: FolderChatScope,
): ConsentStatementSnapshot[] {
  return statements.filter(
    (statement) =>
      statement.verb.kind === "capability" &&
      (statement.resource.kind === "host_root" ||
        statement.resource.kind === "host_path_subtree") &&
      statement.resource.root_id === rootId &&
      statementReachesChat(statement, chat),
  );
}

/** The distinct reach a set of folder statements adds up to. */
export function folderReach(
  statements: readonly ConsentStatementSnapshot[],
): FolderReach[] {
  const reach: FolderReach[] = [];
  for (const statement of statements) {
    if (
      statement.verb.kind === "capability" &&
      !reach.includes(statement.verb.capability)
    ) {
      reach.push(statement.verb.capability);
    }
  }
  return reach;
}

export function folderAccessLabel(reach: readonly FolderReach[]): string {
  const parts: string[] = [];
  if (reach.includes("read_files")) parts.push("Read");
  if (reach.includes("write_files")) parts.push("write");
  if (reach.includes("execute_commands")) parts.push("commands");
  switch (parts.length) {
    case 0:
      return "No access";
    case 1: {
      const only = parts[0];
      return `${only[0].toUpperCase()}${only.slice(1)} only`;
    }
    case 2:
      return `${parts[0]} and ${parts[1]}`;
    default:
      return `${parts[0]}, ${parts[1]}, and ${parts[2]}`;
  }
}
