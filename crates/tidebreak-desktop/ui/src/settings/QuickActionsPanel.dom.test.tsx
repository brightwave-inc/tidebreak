// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  DEFAULT_WORKFLOW_PROMPTS,
  resetWorkflowPromptStore,
  useWorkflowPromptStore,
} from "@/code/workflowPrompts";
import { QuickActionsPanel } from "./QuickActionsPanel";

afterEach(() => {
  cleanup();
  resetWorkflowPromptStore();
  window.localStorage.clear();
});

describe("QuickActionsPanel", () => {
  it("edits a prompt and restores the shipped wording", () => {
    render(<QuickActionsPanel />);
    const createPr = screen.getByRole("textbox", { name: "Create PR prompt" });
    expect(createPr).toHaveValue(DEFAULT_WORKFLOW_PROMPTS.compose_pr);
    expect(
      screen.queryByRole("button", { name: "Reset Create PR to default" }),
    ).toBeNull();

    fireEvent.change(createPr, {
      target: { value: "Open a pull request against {base}." },
    });
    expect(useWorkflowPromptStore.getState().overrides.compose_pr).toBe(
      "Open a pull request against {base}.",
    );
    expect(window.localStorage.getItem("tidebreak.workflowPrompts")).toContain(
      "Open a pull request against {base}.",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Reset Create PR to default" }),
    );
    expect(createPr).toHaveValue(DEFAULT_WORKFLOW_PROMPTS.compose_pr);
    expect(
      screen.queryByRole("button", { name: "Reset Create PR to default" }),
    ).toBeNull();
    expect(
      useWorkflowPromptStore.getState().overrides.compose_pr,
    ).toBeUndefined();
  });
});
