// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NewProjectDialog } from "./NewProjectDialog";

afterEach(cleanup);

describe("NewProjectDialog", () => {
  it("creates on Cmd+Enter with the trimmed name", async () => {
    const user = userEvent.setup();
    const onCreate = vi.fn(async () => true);
    const onOpenChange = vi.fn();
    render(
      <NewProjectDialog
        open
        onOpenChange={onOpenChange}
        onCreate={onCreate}
        creating={false}
      />,
    );

    await user.type(
      screen.getByRole("textbox", { name: "Project name" }),
      "  Research  ",
    );
    await user.keyboard("{Meta>}{Enter}{/Meta}");

    await waitFor(() =>
      expect(onCreate).toHaveBeenCalledExactlyOnceWith("Research"),
    );
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
  });

  it("does not create an empty name", async () => {
    const user = userEvent.setup();
    const onCreate = vi.fn(async () => true);
    render(
      <NewProjectDialog
        open
        onOpenChange={vi.fn()}
        onCreate={onCreate}
        creating={false}
      />,
    );

    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await user.click(screen.getByRole("button", { name: "Create" }));

    expect(onCreate).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Create" })).toBeDisabled();
  });

  it("stays open when create fails so the name is not lost", async () => {
    const user = userEvent.setup();
    const onCreate = vi.fn(async () => false);
    const onOpenChange = vi.fn();
    render(
      <NewProjectDialog
        open
        onOpenChange={onOpenChange}
        onCreate={onCreate}
        creating={false}
      />,
    );

    await user.type(
      screen.getByRole("textbox", { name: "Project name" }),
      "Research",
    );
    await user.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() =>
      expect(onCreate).toHaveBeenCalledExactlyOnceWith("Research"),
    );
    expect(onOpenChange).not.toHaveBeenCalled();
  });
});
