import { readdirSync } from "node:fs";
import { join } from "node:path";

import { expect, type Page } from "@playwright/test";

import type { Machine } from "./machine";

/**
 * Open a workspace on the registered fixture repository with a first message,
 * and wait for the scripted engine's turn to finish.
 */
export async function startWorkspace(
  page: Page,
  firstMessage: string,
): Promise<void> {
  await page.getByRole("radio", { name: "Code" }).click();
  await page.getByRole("button", { name: "New workspace on fixture" }).click();
  const dialog = page.getByRole("dialog", { name: "New workspace" });
  await dialog
    .getByRole("textbox", { name: "First message" })
    .fill(firstMessage);
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(
    page.getByRole("main").getByRole("group", { name: "Turn finished" }),
  ).toBeVisible();
  // The fixture's local origin lets the workspace bring `main` up to date.
  await expect(page.getByText(/Could not update main/)).toHaveCount(0);
}

/**
 * The machine's workspace checkouts, found on disk rather than asked of the
 * server, so a flow can check what a click really did to the files.
 */
export function workspaceCheckouts(machine: Machine): string[] {
  const root = join(machine.dataDir, "code", "worktrees");
  return readdirSync(root, { withFileTypes: true })
    .filter((owner) => owner.isDirectory())
    .flatMap((owner) =>
      readdirSync(join(root, owner.name), { withFileTypes: true })
        .filter((repository) => repository.isDirectory())
        .flatMap((repository) =>
          readdirSync(join(root, owner.name, repository.name), {
            withFileTypes: true,
          })
            .filter((workspace) => workspace.isDirectory())
            .map((workspace) =>
              join(root, owner.name, repository.name, workspace.name),
            ),
        ),
    );
}
