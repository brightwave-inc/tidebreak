import { describe, expect, it } from "vitest";

import type { CodeWorkspaceSnapshot } from "@/api/types";
import { codeDigest, codeWorkspace, harnessDoctor } from "@/stories/fixtures";
import { codeTriggerTargetForRepository } from "./CodeTriggerTarget";
import type { DigestsByWorkspace } from "./CodeUpdatesStore";

function workspace(
  overrides: Partial<CodeWorkspaceSnapshot> = {},
): CodeWorkspaceSnapshot {
  return {
    ...codeWorkspace,
    id: "ws-trigger",
    repo_id: "repo-trigger",
    pr: { number: 42, state: "open" },
    ...overrides,
  };
}

describe("codeTriggerTargetForRepository", () => {
  it("uses the newest eligible interactive session, not snapshot order", () => {
    const conversationsByWorkspace: DigestsByWorkspace = {
      "ws-trigger": {
        "sess-created-later": codeDigest({
          workspace: "ws-trigger",
          session: "sess-created-later",
          harness_kind: "claude_code",
          title: "Older activity",
          trigger_target_at: "2026-08-28T10:00:00Z",
        }),
        "sess-active-later": codeDigest({
          workspace: "ws-trigger",
          session: "sess-active-later",
          harness_kind: "codex",
          title: "Fix trigger delivery",
          trigger_target_at: "2026-08-29T11:00:00Z",
        }),
        "sess-watch": codeDigest({
          workspace: "ws-trigger",
          session: "sess-watch",
          kind: "watch",
          harness_kind: "codex",
          title: "Watch",
          trigger_target_at: "2026-08-29T12:00:00Z",
        }),
        "sess-fenced": codeDigest({
          workspace: "ws-trigger",
          session: "sess-fenced",
          lifecycle: "fenced",
          harness_kind: "codex",
          title: "Fenced",
          trigger_target_at: "2026-08-29T13:00:00Z",
        }),
      },
    };

    expect(
      codeTriggerTargetForRepository({
        repoId: "repo-trigger",
        workspaces: [workspace()],
        conversationsByWorkspace,
        doctor: harnessDoctor,
      }),
    ).toEqual({
      sessionTitle: "Fix trigger delivery",
      harnessLabel: "Codex CLI",
      delivery: "steer",
    });
  });

  it("waits for quiet unless the selected harness declares steering", () => {
    const conversationsByWorkspace: DigestsByWorkspace = {
      "ws-trigger": {
        "sess-claude": codeDigest({
          workspace: "ws-trigger",
          session: "sess-claude",
          harness_kind: "claude_code",
          title: "Review notification rules",
          trigger_target_at: "2026-08-29T11:00:00Z",
        }),
      },
    };

    expect(
      codeTriggerTargetForRepository({
        repoId: "repo-trigger",
        workspaces: [workspace()],
        conversationsByWorkspace,
        doctor: harnessDoctor,
      }),
    ).toEqual({
      sessionTitle: "Review notification rules",
      harnessLabel: "Claude Code",
      delivery: "next_turn",
    });
    expect(
      codeTriggerTargetForRepository({
        repoId: "repo-trigger",
        workspaces: [workspace({ pr: undefined })],
        conversationsByWorkspace,
        doctor: harnessDoctor,
      }),
    ).toBeNull();
  });
});
