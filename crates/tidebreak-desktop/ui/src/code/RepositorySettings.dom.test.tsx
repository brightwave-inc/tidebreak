// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeRepoSnapshot } from "@/api/types";
import { RepositorySettings } from "./RepositorySettings";

afterEach(cleanup);

function repo(overrides: Partial<CodeRepoSnapshot> = {}): CodeRepoSnapshot {
  return {
    id: "repo-1",
    root_path: "/tmp/tidebreak",
    display_name: "tidebreak",
    default_base_ref: "main",
    branch_prefix: "tidebreak/",
    setup_script: "pnpm install",
    quick_actions: [
      { name: "Test", command: "cargo test", auto_run_on_create: false },
    ],
    created_at: "2026-08-22T12:00:00Z",
    ...overrides,
  } as CodeRepoSnapshot;
}

describe("RepositorySettings", () => {
  it("saves an edited setup script when the field is committed", async () => {
    const user = userEvent.setup();
    const stored = repo();
    const client = {
      getCodeRepo: vi.fn(async () => stored),
      patchCodeRepo: vi.fn(async () => ({
        ...stored,
        setup_script: "pnpm install --frozen-lockfile",
      })),
    };
    render(
      <RepositorySettings
        client={client}
        repoId="repo-1"
        repoLabel="brightwave-inc/tidebreak"
      />,
    );

    const setup = await screen.findByLabelText("Setup script");
    expect(setup).toHaveValue("pnpm install");
    await user.clear(setup);
    await user.type(setup, "pnpm install --frozen-lockfile");
    await user.tab();

    await waitFor(() =>
      expect(client.patchCodeRepo).toHaveBeenCalledWith(
        "repo-1",
        expect.objectContaining({
          setup_script: "pnpm install --frozen-lockfile",
        }),
      ),
    );
  });

  it("writes quick actions, which had no write path at all before", async () => {
    const user = userEvent.setup();
    const stored = repo({ quick_actions: [] });
    const client = {
      getCodeRepo: vi.fn(async () => stored),
      patchCodeRepo: vi.fn(async (_id: string, body: unknown) => ({
        ...stored,
        ...(body as object),
      })),
    };
    render(
      <RepositorySettings
        client={client}
        repoId="repo-1"
        repoLabel="brightwave-inc/tidebreak"
      />,
    );

    await user.click(await screen.findByRole("button", { name: "Add" }));
    await user.type(screen.getByLabelText("Quick action 1 name"), "Lint");
    await user.type(
      screen.getByLabelText("Quick action 1 command"),
      "cargo clippy",
    );
    await user.click(
      screen.getByRole("switch", { name: "Run quick action 1 on create" }),
    );

    await waitFor(() =>
      expect(client.patchCodeRepo).toHaveBeenCalledWith(
        "repo-1",
        expect.objectContaining({
          quick_actions: [
            { name: "Lint", command: "cargo clippy", auto_run_on_create: true },
          ],
        }),
      ),
    );
  });

  it("does not write a field that only lost focus", async () => {
    const user = userEvent.setup();
    const client = {
      getCodeRepo: vi.fn(async () => repo()),
      patchCodeRepo: vi.fn(),
    };
    render(
      <RepositorySettings
        client={client}
        repoId="repo-1"
        repoLabel="brightwave-inc/tidebreak"
      />,
    );

    await user.click(await screen.findByLabelText("Base ref"));
    await user.tab();

    expect(client.patchCodeRepo).not.toHaveBeenCalled();
  });

  it("says what to do when the repository is not registered", async () => {
    const client = { getCodeRepo: vi.fn(), patchCodeRepo: vi.fn() };
    render(
      <RepositorySettings
        client={client}
        repoId={null}
        repoLabel="brightwave-inc/tidebreak"
      />,
    );

    expect(
      await screen.findByText(
        "Register this repository in Tidebreak before editing hooks.",
      ),
    ).toBeInTheDocument();
    expect(client.getCodeRepo).not.toHaveBeenCalled();
  });
});
