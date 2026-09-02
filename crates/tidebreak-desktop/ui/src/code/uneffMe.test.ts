import { describe, expect, it, vi } from "vitest";

import type {
  CodeRepoSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorEntry,
} from "../api/types";
import { splitPastedText } from "../PastedText";
import {
  DEBUG_JSON_PROMPT_BUDGET,
  debugJsonForPrompt,
  isTidebreakProductRepo,
  prepareUneffMe,
  tidebreakProductRepo,
  uneffMePrompt,
  uneffMeWorkspaceTitle,
  uneffPreparationSteps,
  uneffSessionSettings,
  type UneffProgress,
} from "./uneffMe";

const APP: CodeRepoSnapshot = {
  id: "repo-app",
  root_path: "/tmp/app",
  display_name: "app",
  default_base_ref: "main",
  branch_prefix: "tidebreak",
  quick_actions: [],
  created_at: "2026-08-15T00:00:00.000Z",
};

const TIDEBREAK: CodeRepoSnapshot = {
  ...APP,
  id: "repo-tb",
  root_path: "/Users/sam/src/tidebreak",
  display_name: "tidebreak",
};

const CREATED: CodeWorkspaceSnapshot = {
  id: "ws-fix",
  repo_id: TIDEBREAK.id,
  title: "Uneff: Fix login",
  worktree_path: "/tmp/tidebreak/.worktrees/uneff",
  branch_name: "tidebreak/uneff-fix-login",
  base_ref: "main",
  status: "active",
  created_at: "2026-08-26T00:00:00.000Z",
};

const CLAUDE: HarnessDoctorEntry = {
  kind: "claude_code",
  found: true,
  installable: true,
  authenticated: true,
  tier: "reference",
  caps: {
    resume: "supported",
    streaming_deltas: "supported",
    mid_turn_steering: "unsupported",
    plan_mode: "supported",
    structured_approvals: "supported",
    auto_mode: "supported",
    allow_mode: "supported",
    reasoning_levels: "unknown",
    native_file_change_events: "unsupported",
    native_interrupt: "supported",
    image_input: "unknown",
    slash_commands: "unknown",
    durable_parks: "unsupported",
    user_questions: "unsupported",
    standing_grants: "unsupported",
    mid_turn_resume: "unsupported",
    transcript: "unsupported",
  },
  commands: [],
  auth_mode: "local_sign_in",
  remediation: "",
  stderr: "",
  unrecognized_event_count: 0,
  relaunch_composes_permission_mode: true,
  update_available: false,
};

const CODEX: HarnessDoctorEntry = { ...CLAUDE, kind: "codex" };

const PROMPT_INPUT = {
  sourceTitle: "Fix login",
  sourceBranch: "tidebreak/fix-login",
  sourceRepo: "app",
  sessionId: "sess-1",
  debug: { session: { id: "sess-1" }, turns: [], events: [] },
};

describe("tidebreak product repo", () => {
  it("matches the product checkout, not a worktree folder", () => {
    expect(isTidebreakProductRepo(TIDEBREAK)).toBe(true);
    expect(
      isTidebreakProductRepo({
        ...APP,
        display_name: "notes",
        root_path: "/Users/sam/Tidebreak/workspaces/tidebreak/ember-orchard",
      }),
    ).toBe(false);
    expect(
      isTidebreakProductRepo({
        ...APP,
        display_name: "brightwave-inc/tidebreak",
        root_path: "/tmp/clone",
      }),
    ).toBe(true);
  });

  it("prefers the display name tidebreak when several match", () => {
    const clone: CodeRepoSnapshot = {
      ...TIDEBREAK,
      id: "repo-clone",
      display_name: "tidebreak-src",
      root_path: "/opt/tidebreak",
    };
    expect(tidebreakProductRepo([APP, clone, TIDEBREAK])?.id).toBe("repo-tb");
  });
});

describe("uneff me prompt", () => {
  it("asks what the user wants before acting, and offers issue or PR", () => {
    const prompt = uneffMePrompt({
      ...PROMPT_INPUT,
      inTidebreakCheckout: true,
    });
    expect(prompt).toContain("Fix login");
    expect(prompt).toContain("tidebreak/fix-login");
    expect(prompt).toContain("Start by asking the user what went wrong");
    expect(prompt).toContain("gh issue create --repo brightwave-inc/tidebreak");
    expect(prompt).toContain("open the pull request against main");
    expect(prompt).toContain("gh repo fork --remote");
    expect(prompt).toContain("Never paste the whole report");
    expect(prompt).not.toContain("omitted");
    // The ask comes before the how-to and the report.
    expect(prompt.indexOf("Start by asking")).toBeLessThan(
      prompt.indexOf("gh issue create"),
    );
    expect(prompt.indexOf("gh issue create")).toBeLessThan(
      prompt.indexOf("The debug report follows"),
    );
  });

  it("folds the debug report the way a long paste is folded", () => {
    const prompt = uneffMePrompt({
      ...PROMPT_INPUT,
      inTidebreakCheckout: true,
    });
    const { prose, pasted } = splitPastedText(prompt);
    expect(pasted).toHaveLength(1);
    expect(pasted[0]).toContain('"id": "sess-1"');
    expect(prose).not.toContain('"id": "sess-1"');
    expect(prose).toContain("Session: sess-1");
  });

  it("never clones for the user when there is no checkout", () => {
    const prompt = uneffMePrompt({
      ...PROMPT_INPUT,
      inTidebreakCheckout: false,
    });
    expect(prompt).toContain("not a Tidebreak checkout");
    expect(prompt).toContain("gh issue create --repo brightwave-inc/tidebreak");
    expect(prompt).toContain("clone it only after they say yes");
    expect(prompt).toContain("Do not clone anything without asking");
    expect(prompt).toContain("adding the Tidebreak repository to Code");
    expect(prompt).not.toContain("fresh workspace");
  });

  it("drops journal events when the dump is too large", () => {
    const events = Array.from({ length: 80 }, (_, index) => ({
      seq: index,
      blob: "x".repeat(2_000),
    }));
    const packed = debugJsonForPrompt({
      session: { id: "sess-1" },
      turns: [{ id: "turn-1" }],
      events,
    });
    expect(packed.omitted).toBe("events");
    expect(packed.text).toContain('"sess-1"');
    expect(packed.text).not.toContain('"blob"');
    expect(packed.text.length).toBeLessThanOrEqual(DEBUG_JSON_PROMPT_BUDGET);

    const prompt = uneffMePrompt({
      ...PROMPT_INPUT,
      debug: { session: { id: "sess-1" }, turns: [{ id: "turn-1" }], events },
      inTidebreakCheckout: true,
    });
    expect(prompt).toContain("Journal events were omitted");
  });

  it("truncates a title the way a fresh PR-agent workspace does", () => {
    expect(uneffMeWorkspaceTitle("Fix login")).toBe("Uneff: Fix login");
    expect(uneffMeWorkspaceTitle("x".repeat(80)).length).toBe(60);
    expect(uneffMeWorkspaceTitle("x".repeat(80)).endsWith("…")).toBe(true);
  });
});

describe("uneff me session settings", () => {
  it("follows the source session's engine, then the last create, then any", () => {
    const doctor = { harnesses: [CLAUDE, CODEX] };
    expect(
      uneffSessionSettings({
        doctor,
        sourceHarness: "codex",
        lastCreate: { harness: "claude_code", modelsByHarness: {} },
        ceiling: null,
      })?.harness,
    ).toBe("codex");
    expect(
      uneffSessionSettings({
        doctor,
        sourceHarness: "grok",
        lastCreate: {
          harness: "codex",
          modelsByHarness: { codex: "gpt-5" },
          permissionMode: "auto",
        },
        ceiling: null,
      }),
    ).toEqual({
      harness: "codex",
      permissionMode: "auto",
      model: "gpt-5",
      reasoningEffort: undefined,
      fastMode: undefined,
    });
    expect(
      uneffSessionSettings({
        doctor,
        lastCreate: null,
        ceiling: null,
      }),
    ).toMatchObject({ harness: "claude_code", permissionMode: "allow" });
  });

  it("returns null when no engine can start, and honors a plan ceiling", () => {
    expect(
      uneffSessionSettings({
        doctor: { harnesses: [{ ...CLAUDE, found: false }] },
        lastCreate: null,
        ceiling: null,
      }),
    ).toBeNull();
    expect(
      uneffSessionSettings({ doctor: null, lastCreate: null, ceiling: null }),
    ).toBeNull();
    expect(
      uneffSessionSettings({
        doctor: { harnesses: [CLAUDE] },
        lastCreate: null,
        ceiling: "plan",
      })?.permissionMode,
    ).toBe("plan");
  });
});

describe("uneff me preparation steps", () => {
  it("collects the report, then hands over to the workspace step", () => {
    const labels = (progress: UneffProgress) =>
      uneffPreparationSteps(progress).map(
        (step) => `${step.label}:${step.state}`,
      );
    expect(labels({ step: "debug" })).toEqual([
      "Collecting the debug report:active",
    ]);
    expect(labels({ step: "create" })).toEqual([
      "Collecting the debug report:complete",
    ]);
  });
});

describe("prepareUneffMe", () => {
  it("creates a workspace on the connected Tidebreak checkout", async () => {
    const getDebug = vi.fn(async () => ({ session: { id: "sess-1" } }));
    const createWorkspace = vi.fn(async () => CREATED);
    const progress: UneffProgress[] = [];

    const result = await prepareUneffMe({
      repos: [APP, TIDEBREAK],
      sessionId: "sess-1",
      sourceTitle: "Fix login",
      sourceBranch: "tidebreak/fix-login",
      sourceRepo: "app",
      getDebug,
      createWorkspace,
      onProgress: (step: UneffProgress) => progress.push(step),
    });

    expect(getDebug).toHaveBeenCalledWith("sess-1");
    expect(createWorkspace).toHaveBeenCalledWith({
      repo_id: TIDEBREAK.id,
      title: "Uneff: Fix login",
    });
    expect(result.workspace).toEqual(CREATED);
    expect(result.prompt).toContain("fresh workspace on the Tidebreak source");
    expect(progress).toEqual([{ step: "debug" }, { step: "create" }]);
  });

  it("runs in place, and creates nothing, when no checkout is connected", async () => {
    const createWorkspace = vi.fn();
    const progress: UneffProgress[] = [];

    const result = await prepareUneffMe({
      repos: [APP],
      sessionId: "sess-1",
      sourceTitle: "Fix login",
      sourceBranch: "tidebreak/fix-login",
      sourceRepo: "app",
      getDebug: vi.fn(async () => ({})),
      createWorkspace,
      onProgress: (step: UneffProgress) => progress.push(step),
    });

    expect(createWorkspace).not.toHaveBeenCalled();
    expect(result.workspace).toBeNull();
    expect(result.prompt).toContain("not a Tidebreak checkout");
    expect(progress).toEqual([{ step: "debug" }]);
  });
});
