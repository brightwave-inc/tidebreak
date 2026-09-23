import { useState } from "react";
import { create } from "zustand";

import type { ApiClient } from "@/api/client";
import type {
  CodeProjectConfigFile,
  CodeWorkspaceSnapshot,
  HarnessKind,
} from "@/api/types";
import { friendlyErrorMessage } from "@/lib/utils";
import { needsTrustDecision, workspaceHasLocalTrust } from "./repositoryTrust";
import {
  RepositoryTrustSheet,
  type RepositoryTrustChoice,
} from "./RepositoryTrustSheet";

/** How a trust question ended. */
export type RepositoryTrustOutcome = "trusted" | "untrusted" | "dismissed";

type TrustClient = Pick<
  ApiClient,
  "getCodeWorkspaceTrust" | "setCodeRepoTrust"
>;

type TrustRequest = {
  id: number;
  client: Pick<ApiClient, "setCodeRepoTrust">;
  repoId: string;
  repoLabel: string | null;
  files: CodeProjectConfigFile[];
  settled: Promise<RepositoryTrustOutcome>;
  resolve: (outcome: RepositoryTrustOutcome) => void;
};

type RepositoryTrustState = {
  /** Open questions, oldest first. The sheet shows the head. */
  queue: TrustRequest[];
  ask: (
    request: Omit<TrustRequest, "id" | "settled" | "resolve">,
  ) => Promise<RepositoryTrustOutcome>;
  settle: (id: number, outcome: RepositoryTrustOutcome) => void;
};

let nextRequestId = 0;

/**
 * The open trust questions. A store rather than component state, because the
 * paths that start a session are plain functions and the sheet is mounted
 * once in the shell.
 */
export const useRepositoryTrustStore = create<RepositoryTrustState>(
  (set, get) => ({
    queue: [],
    ask: (request) => {
      // Two sessions starting in one repository share one question.
      const pending = get().queue.find(
        (entry) => entry.repoId === request.repoId,
      );
      if (pending) return pending.settled;
      let resolve: (outcome: RepositoryTrustOutcome) => void = () => {};
      const settled = new Promise<RepositoryTrustOutcome>((done) => {
        resolve = done;
      });
      set((state) => ({
        queue: [
          ...state.queue,
          { ...request, id: ++nextRequestId, settled, resolve },
        ],
      }));
      return settled;
    },
    settle: (id, outcome) => {
      const request = get().queue.find((entry) => entry.id === id);
      if (!request) return;
      set((state) => ({
        queue: state.queue.filter((entry) => entry.id !== id),
      }));
      request.resolve(outcome);
    },
  }),
);

/**
 * Ask about the repository's own engine config before a session starts on
 * `harness`, when there is anything to ask.
 *
 * Nothing is asked when the repository already has a decision, when its
 * checkout carries no config that engine would load, or when the workspace's
 * trust is not this reader's to decide. The server launches an undecided
 * repository without its config, so every path that skips the question, a
 * failed read included, is the safe one. The promise settles once the
 * session may start.
 */
export async function confirmRepositoryTrust(input: {
  client: TrustClient;
  workspace: Pick<
    CodeWorkspaceSnapshot,
    "id" | "worktree_path" | "is_owner" | "repo_display_name"
  >;
  harness: HarnessKind;
}): Promise<RepositoryTrustOutcome | null> {
  if (!workspaceHasLocalTrust(input.workspace)) return null;
  let snapshot;
  try {
    snapshot = await input.client.getCodeWorkspaceTrust(input.workspace.id);
  } catch {
    return null;
  }
  if (!needsTrustDecision(snapshot, input.harness)) return null;
  return useRepositoryTrustStore.getState().ask({
    client: input.client,
    repoId: snapshot.repo_id,
    repoLabel: input.workspace.repo_display_name ?? null,
    files: snapshot.files,
  });
}

/**
 * The trust sheet for the question at the head of the queue. Mounted once in
 * the shell, so a session started from any route can ask.
 */
export function RepositoryTrustSheetHost() {
  const request = useRepositoryTrustStore((state) => state.queue[0] ?? null);
  const settle = useRepositoryTrustStore((state) => state.settle);
  // Keyed by request, so a question never inherits the last one's state.
  const [status, setStatus] = useState<{
    id: number;
    saving: RepositoryTrustChoice | null;
    error: string | null;
  } | null>(null);

  if (!request) return null;
  const current = status?.id === request.id ? status : null;

  const choose = async (choice: RepositoryTrustChoice) => {
    setStatus({ id: request.id, saving: choice, error: null });
    try {
      await request.client.setCodeRepoTrust(request.repoId, choice === "trust");
      settle(request.id, choice === "trust" ? "trusted" : "untrusted");
    } catch (caught) {
      if (choice === "continue") {
        // The session starts without the settings either way. Without a
        // recorded answer the next session asks again, which is the honest
        // cost of the failed write.
        settle(request.id, "dismissed");
        return;
      }
      setStatus({
        id: request.id,
        saving: null,
        error: friendlyErrorMessage(caught, "Could not trust the repository."),
      });
    }
  };

  return (
    <RepositoryTrustSheet
      key={request.id}
      open
      repoLabel={request.repoLabel}
      files={request.files}
      saving={current?.saving ?? null}
      error={current?.error ?? null}
      onChoose={(choice) => void choose(choice)}
      onDismiss={() => settle(request.id, "dismissed")}
    />
  );
}
