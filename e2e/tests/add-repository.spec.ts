import { existsSync, realpathSync } from "node:fs";
import { join } from "node:path";

import { listRepositories } from "../harness/api";
import { expect, openApp, test } from "../harness/fixtures";
import {
  createFixtureRepository,
  serveRepository,
} from "../harness/repository";

test("a repository added by URL is cloned onto the machine and ready for a workspace", async ({
  page,
  machine,
}) => {
  const origin = await serveRepository(
    machine.root,
    await createFixtureRepository(machine.root),
  );
  try {
    await openApp(page, machine);
    await page.getByRole("radio", { name: "Code" }).click();
    await expect(
      page.getByRole("heading", { name: "Start with a repository" }),
    ).toBeVisible();

    await page
      .getByRole("main")
      .getByRole("button", { name: "Add repo" })
      .click();
    const palette = page.getByRole("dialog", { name: "Add a repo" });
    // A tab attached to a machine cannot name a path on it, so only URLs are offered.
    await expect(
      palette.getByRole("option", { name: /Git URL/ }),
    ).toBeVisible();
    await expect(
      palette.getByRole("option", { name: /Local folder/ }),
    ).toHaveCount(0);
    await palette.getByRole("option", { name: /Git URL/ }).click();

    const form = page.getByRole("dialog", { name: "Git URL" });
    await form.getByRole("textbox", { name: "URL" }).fill(origin.url);
    await form.getByRole("button", { name: "Clone" }).click();

    // The clone job finishes and hands straight over to a new workspace on it.
    const newWorkspace = page.getByRole("dialog", { name: "New workspace" });
    await expect(newWorkspace.getByRole("button", { name: "Repo" })).toHaveText(
      "fixture",
    );
    await expect(
      newWorkspace.getByRole("button", { name: "Base ref" }),
    ).toHaveText("From main");
    await page.keyboard.press("Escape");
    await expect(
      page.getByRole("button", { name: "New workspace on fixture" }),
    ).toBeVisible();
    // The machine places clones itself, under its own data directory.
    const [repository] = await listRepositories(machine);
    expect(repository.display_name).toBe("fixture");
    expect(realpathSync(repository.root_path)).toContain(
      realpathSync(machine.dataDir),
    );
    expect(existsSync(join(repository.root_path, "README.md"))).toBe(true);
  } finally {
    await origin.close();
  }
});
