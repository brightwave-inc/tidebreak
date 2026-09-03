// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeGitHubRepositoryRef } from "@/api/types";
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

describe("RepositoryTriggerRules", () => {
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
