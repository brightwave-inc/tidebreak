// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  CodeGitHubRepositoryRef,
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
} from "@/api/types";
import { RepositoryTriggerRules } from "./RepositoryTriggerRules";

afterEach(cleanup);

const repository: CodeGitHubRepositoryRef = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "tidebreak",
  name_with_owner: "brightwave-inc/tidebreak",
  url: "https://github.com/brightwave-inc/tidebreak",
  tidebreak_repo_id: "repo-1",
};

function trigger(
  condition: CodeTriggerCondition,
  action: CodeTriggerAction,
  enabled = true,
): CodeTriggerSnapshot {
  return {
    id: `trigger-${condition}`,
    repo_id: "repo-1",
    condition,
    action,
    enabled,
    created_at: "2026-08-22T12:00:00Z",
    updated_at: "2026-08-22T12:00:00Z",
  };
}

describe("RepositoryTriggerRules", () => {
  it("loads, disables, and deletes repository rules", async () => {
    const user = userEvent.setup();
    const existing = trigger("checks_failed", "deliver");
    const disabled = { ...existing, enabled: false };
    const client = {
      listCodeTriggers: vi.fn(async () => [existing]),
      createCodeTrigger: vi.fn(),
      setCodeTriggerEnabled: vi.fn(async () => disabled),
      deleteCodeTrigger: vi.fn(async () => undefined),
    };
    render(<RepositoryTriggerRules client={client} repository={repository} />);

    expect(
      await screen.findByRole("switch", { name: "Checks fail" }),
    ).toBeChecked();
    await user.click(screen.getByRole("switch", { name: "Checks fail" }));
    await waitFor(() =>
      expect(client.setCodeTriggerEnabled).toHaveBeenCalledWith(
        "repo-1",
        existing.id,
        false,
      ),
    );
    expect(
      screen.getByRole("switch", { name: "Checks fail" }),
    ).not.toBeChecked();

    await user.click(
      screen.getByRole("button", { name: "Delete Checks fail trigger" }),
    );
    await waitFor(() =>
      expect(client.deleteCodeTrigger).toHaveBeenCalledWith(
        "repo-1",
        existing.id,
      ),
    );
  });

  it("creates a notify-only rule in the first write", async () => {
    const user = userEvent.setup();
    const created = trigger("ready_to_merge", "notify");
    const client = {
      listCodeTriggers: vi.fn(async () => []),
      createCodeTrigger: vi.fn(async () => created),
      setCodeTriggerEnabled: vi.fn(),
      deleteCodeTrigger: vi.fn(),
    };
    render(<RepositoryTriggerRules client={client} repository={repository} />);

    await screen.findByRole("switch", { name: "Ready to merge" });
    await user.click(
      screen.getByRole("combobox", { name: "Ready to merge action" }),
    );
    await user.click(screen.getByRole("option", { name: "Just notify me" }));
    await user.click(screen.getByRole("switch", { name: "Ready to merge" }));

    await waitFor(() =>
      expect(client.createCodeTrigger).toHaveBeenCalledWith(
        "repo-1",
        "ready_to_merge",
        "notify",
      ),
    );
  });

  it("shows a retry when loading fails", async () => {
    const user = userEvent.setup();
    const listCodeTriggers = vi
      .fn()
      .mockRejectedValueOnce(new Error("GitHub unavailable"))
      .mockResolvedValueOnce([]);
    const client = {
      listCodeTriggers,
      createCodeTrigger: vi.fn(),
      setCodeTriggerEnabled: vi.fn(),
      deleteCodeTrigger: vi.fn(),
    };
    render(<RepositoryTriggerRules client={client} repository={repository} />);

    expect(await screen.findByText("GitHub unavailable")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(listCodeTriggers).toHaveBeenCalledTimes(2));
    expect(
      await screen.findByRole("switch", { name: "Checks fail" }),
    ).toBeInTheDocument();
  });
});
