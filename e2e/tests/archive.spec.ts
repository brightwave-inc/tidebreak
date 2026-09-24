import { registerRepository } from "../harness/api";
import { startWorkspace } from "../harness/code";
import { expect, openApp, test } from "../harness/fixtures";
import { createFixtureRepository } from "../harness/repository";
import { codeTurn } from "../harness/scripts";

// A turn that changes nothing, so archiving has no leftover work to confirm.
test.use({
  scripts: { harness: codeTurn({ reply: "The repository has one README." }) },
  // Opening an archived workspace reads its file tree and pull request, and
  // both answer 500 today. Delete this entry when #3580 is fixed.
  knownServerErrors: {
    "#3580": [
      { method: "GET", path: /^\/code\/workspaces\/[^/]+\/tree$/ },
      { method: "GET", path: /^\/code\/workspaces\/[^/]+\/pr$/ },
    ],
  },
});

test("an archived workspace keeps its conversation and returns to the rail on restore", async ({
  page,
  machine,
}) => {
  await registerRepository(
    machine,
    await createFixtureRepository(machine.root),
  );
  await openApp(page, machine);
  await startWorkspace(page, "What is in this repository?");

  const main = page.getByRole("main");
  const title =
    (await main.getByRole("heading", { level: 1 }).textContent())?.trim() ?? "";
  expect(title).not.toBe("");
  const rail = page.getByRole("region", { name: "fixture" });
  const row = rail.getByRole("button", {
    name: new RegExp(`^${title.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")} ·`),
  });
  await expect(row).toBeVisible();

  await main
    .getByRole("button", { name: "Workspace actions", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Archive", exact: true }).click();
  await expect(page.getByText("Workspace archived")).toBeVisible();
  await expect(rail).toHaveCount(0);

  const destinations = page.getByRole("navigation", {
    name: "Code destinations",
  });
  await destinations.getByRole("button", { name: "Archive" }).click();
  const archived = page.getByRole("list", { name: "Archived workspaces" });
  await expect(archived.getByRole("listitem")).toHaveCount(1);
  await expect(archived.getByRole("listitem")).toContainText(title);

  // An archived workspace still opens onto the conversation it had.
  await archived.getByRole("button", { name: `Open ${title}` }).click();
  await expect(main.getByRole("article", { name: "You" })).toContainText(
    "What is in this repository?",
  );
  await expect(main.getByRole("article", { name: "Assistant" })).toContainText(
    "The repository has one README.",
  );

  await destinations.getByRole("button", { name: "Archive" }).click();
  await archived
    .getByRole("listitem")
    .getByRole("button", { name: "Restore" })
    .click();
  await expect(page.getByText("Workspace restored")).toBeVisible();
  await expect(page.getByText("No archived workspaces")).toBeVisible();
  await expect(row).toBeVisible();
});
