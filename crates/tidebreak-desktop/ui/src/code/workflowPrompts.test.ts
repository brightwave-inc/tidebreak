// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";

import { prWorkflowPrompt } from "./prActions";
import { composePrPrompt } from "./workspaceWorkflow";
import {
  DEFAULT_WORKFLOW_PROMPTS,
  interpolateWorkflowPrompt,
  renderWorkflowPrompt,
  resetWorkflowPromptStore,
  useWorkflowPromptStore,
} from "./workflowPrompts";

afterEach(() => {
  resetWorkflowPromptStore();
  window.localStorage.clear();
});

describe("interpolateWorkflowPrompt", () => {
  it("fills known placeholders and leaves unknown ones", () => {
    expect(
      interpolateWorkflowPrompt("Open against {base} for {pr}.", {
        base: "`main`",
        pr: "#41",
      }),
    ).toBe("Open against `main` for #41.");
    expect(interpolateWorkflowPrompt("Keep {other}", { base: "main" })).toBe(
      "Keep {other}",
    );
  });
});

describe("renderWorkflowPrompt", () => {
  it("uses the shipped Create PR wording and names the base branch", () => {
    expect(composePrPrompt("main")).toContain(
      "open a pull request against `main`",
    );
    expect(composePrPrompt(" ")).toContain("the default branch");
  });

  it("sends a stored Create PR override", () => {
    useWorkflowPromptStore
      .getState()
      .setPrompt("compose_pr", "Ship the branch to {base}. Do not merge.");
    expect(composePrPrompt("main")).toBe(
      "Ship the branch to `main`. Do not merge.",
    );
  });

  it("keeps the logs-attached Fix CI wording until the prompt is customized", () => {
    expect(
      renderWorkflowPrompt("fix_errors", { pr: "#41" }, { logsAttached: true }),
    ).toContain("already downloaded");
    useWorkflowPromptStore
      .getState()
      .setPrompt("fix_errors", "Fix {pr} from the attached logs.");
    expect(
      renderWorkflowPrompt("fix_errors", { pr: "#41" }, { logsAttached: true }),
    ).toBe("Fix #41 from the attached logs.");
  });

  it("falls back to the default when the stored prompt is empty", () => {
    useWorkflowPromptStore.getState().setPrompt("compose_pr", "   ");
    expect(composePrPrompt("main")).toBe(
      interpolateWorkflowPrompt(DEFAULT_WORKFLOW_PROMPTS.compose_pr, {
        base: "`main`",
      }),
    );
  });

  it("persists overrides and forgets a reset prompt", () => {
    const store = useWorkflowPromptStore.getState();
    store.setPrompt("compose_pr", "Custom {base}");
    expect(window.localStorage.getItem("tidebreak.workflowPrompts")).toContain(
      "Custom {base}",
    );
    store.resetPrompt("compose_pr");
    expect(window.localStorage.getItem("tidebreak.workflowPrompts")).toBeNull();
  });
});

describe("prWorkflowPrompt overrides", () => {
  it("uses the stored instruction and still appends live pull-request state", () => {
    useWorkflowPromptStore
      .getState()
      .setPrompt("update_branch", "Rebase {pr} onto {base}.");
    const prompt = prWorkflowPrompt("update_branch", {
      number: 41,
      state: "open",
      base_branch: "main",
      title: "Fix login",
    });
    expect(prompt.startsWith("Rebase #41 onto main.")).toBe(true);
    expect(prompt).toContain("Pull request: #41 - Fix login");
  });
});
