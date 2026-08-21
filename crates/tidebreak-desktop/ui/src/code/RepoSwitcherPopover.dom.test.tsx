// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { renderWithRouter } from "@/test/router";
import type { CodeRepoSnapshot } from "../api/types";
import { useCodeUiStore } from "./CodeUiStore";
import { RepoSwitcherPopover } from "./RepoSwitcherPopover";

function repo(id: string, name: string): CodeRepoSnapshot {
  return {
    id,
    root_path: `/tmp/${id}`,
    display_name: name,
    default_base_ref: "main",
    branch_prefix: "tidebreak",
    quick_actions: [],
    created_at: "2026-08-15T00:00:00.000Z",
  };
}

afterEach(() => {
  cleanup();
  useCodeUiStore.setState({ addRepoOpen: false });
});

describe("RepoSwitcherPopover", () => {
  it("filters, navigates from the keyboard, and offers Add repo", async () => {
    const user = userEvent.setup();
    const { router } = await renderWithRouter(
      <RepoSwitcherPopover
        repos={[repo("r-app", "app"), repo("r-lib", "lib")]}
      />,
      { initialUrl: "/code" },
    );

    await user.click(screen.getByRole("button", { name: "Repos" }));
    const input = await screen.findByPlaceholderText("Find a repo");

    await user.type(input, "li");
    expect(
      screen.queryByRole("option", { name: /app/ }),
    ).not.toBeInTheDocument();
    await user.keyboard("{Enter}");
    expect(router.state.location.pathname).toBe("/code/r/r-lib");
  });

  it("opens the add-repo flow from the pinned row", async () => {
    const user = userEvent.setup();
    await renderWithRouter(
      <RepoSwitcherPopover repos={[repo("r-app", "app")]} />,
      { initialUrl: "/code" },
    );

    await user.click(screen.getByRole("button", { name: "Repos" }));
    await user.click(await screen.findByRole("option", { name: "Add repo…" }));
    expect(useCodeUiStore.getState().addRepoOpen).toBe(true);
  });

  it("names the current repo on the trigger", async () => {
    await renderWithRouter(
      <RepoSwitcherPopover repos={[repo("r-app", "app")]} />,
      { initialUrl: "/code/r/r-app" },
    );
    expect(
      screen.getByRole("button", { name: "Switch repo (current: app)" }),
    ).toBeInTheDocument();
  });
});
