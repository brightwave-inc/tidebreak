// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import { ReviewPrototype } from "./ReviewPages";

afterEach(cleanup);

it("keeps an unsent comment through refresh and a trip to the inbox", async () => {
  const user = userEvent.setup();
  render(<ReviewPrototype scenario="discussion" />);
  await user.type(
    screen.getByRole("textbox", { name: "Comment" }),
    "Keep this draft.",
  );
  await user.click(
    screen.getByRole("button", { name: "Refresh pull request" }),
  );
  await user.click(
    within(
      screen.getByRole("navigation", { name: "Code navigation" }),
    ).getByRole("button", { name: "Pull requests" }),
  );
  await user.click(screen.getByRole("button", { name: /Open PR 2251:/ }));
  await user.click(screen.getByRole("tab", { name: "Discussion" }));
  expect(screen.getByRole("textbox", { name: "Comment" })).toHaveValue(
    "Keep this draft.",
  );
});

it("commits the staged file while preserving other unstaged files", async () => {
  const user = userEvent.setup();
  render(<ReviewPrototype scenario="source" />);
  await user.click(screen.getByRole("button", { name: "Stage file" }));
  expect(screen.getByRole("tab", { name: "Staged 1" })).toHaveAttribute(
    "data-state",
    "active",
  );
  await user.click(screen.getByRole("button", { name: "Commit staged (1)" }));
  expect(screen.getByRole("tab", { name: "Unstaged 2" })).toHaveAttribute(
    "data-state",
    "active",
  );
  expect(screen.getByRole("button", { name: "Push 3 commits" })).toBeEnabled();
  expect(
    screen.queryByRole("button", { name: /PullRequestDetail.tsx src\/code/ }),
  ).toBeNull();
});

it("keeps rows available while a refresh error is retried", async () => {
  const user = userEvent.setup();
  render(<ReviewPrototype scenario="inbox-failed" />);
  expect(screen.getByRole("alert")).toHaveTextContent(
    "Your last results are still available.",
  );
  expect(screen.getByRole("button", { name: /Open PR 2251:/ })).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Retry refresh" }));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByRole("button", { name: /Open PR 2251:/ })).toBeVisible();
});
