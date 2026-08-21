import { expect, it } from "vitest";

import {
  attachableFolders,
  mentionRows,
  recentChatFiles,
  MAX_MENTION_ROWS,
  type MentionCandidate,
} from "./ComposerMentions";

const CANDIDATES: MentionCandidate[] = [
  { kind: "file", id: "doc-1", label: "Budget.xlsx", mediaType: "text/csv" },
  {
    kind: "file",
    id: "doc-2",
    label: "Q3 budget notes.md",
    mediaType: "text/markdown",
  },
  { kind: "folder", id: "root-1", label: "Reports" },
];

function file(documentId: string, name: string) {
  return { documentId, name, mediaType: "application/pdf" };
}

it("ranks a prefix above a mid-name hit", () => {
  const rows = mentionRows(
    CANDIDATES,
    ["browse-files", "connect-folder"],
    "budget",
  );

  // The pickers are named too, so a query that misses them drops them: an `@`
  // list should answer what was typed, not always end in the same two rows.
  expect(rows.map(describe)).toEqual(["file:doc-1", "file:doc-2"]);
});

it("drops candidates a query does not name, and the pickers with them", () => {
  // Ordinary prose after an `@` must not leave a popover standing over the
  // draft: with nothing matched the composer has no rows and closes the list.
  expect(mentionRows(CANDIDATES, ["browse-files"], "nobody")).toEqual([]);
});

it("offers the pickers when nothing has been typed yet", () => {
  const rows = mentionRows(CANDIDATES, ["browse-files"], "");

  expect(rows).toHaveLength(4);
  expect(describe(rows[3]!)).toBe("action:browse-files");
});

it("names each transcript file once, newest first, minus what is already attached", () => {
  const messages = [
    { files: [file("doc-1", "First.pdf")] },
    { files: [file("doc-2", "Second.pdf"), file("doc-3", "Third.pdf")] },
    // The same document referenced again later is still one row.
    { files: [file("doc-1", "First.pdf")] },
    {},
  ];

  const recent = recentChatFiles(messages, [
    {
      documentId: "doc-3",
      displayName: "Third.pdf",
      mediaType: "application/pdf",
      byteLen: 12,
    },
  ]);

  expect(recent.map((entry) => entry.documentId)).toEqual(["doc-1", "doc-2"]);
});

it("bounds the transcript scan at the list's size", () => {
  const messages = Array.from({ length: MAX_MENTION_ROWS + 5 }, (_, index) => ({
    files: [file(`doc-${index}`, `File ${index}.pdf`)],
  }));

  expect(recentChatFiles(messages, [])).toHaveLength(MAX_MENTION_ROWS);
});

it("offers workspace paths as insertable mention rows", () => {
  const rows = mentionRows(
    [{ kind: "path", path: "src/lib.rs", label: "src/lib.rs" }],
    [],
    "lib",
  );
  expect(rows).toEqual([
    {
      kind: "candidate",
      candidate: { kind: "path", path: "src/lib.rs", label: "src/lib.rs" },
    },
  ]);
});

it("offers only approved folders this conversation has not attached", () => {
  const approved = [
    { rootId: "root-1", displayName: "Reports", status: "connected" as const },
    {
      rootId: "root-2",
      displayName: "Contracts",
      status: "connected" as const,
    },
  ];

  expect(attachableFolders(approved, [{ rootId: "root-1" }])).toEqual([
    { kind: "folder", id: "root-2", label: "Contracts" },
  ]);
});

function describe(row: ReturnType<typeof mentionRows>[number]): string {
  if (row.kind === "action") return `action:${row.action}`;
  if (row.candidate.kind === "path") return `path:${row.candidate.path}`;
  return `${row.candidate.kind}:${row.candidate.id}`;
}
