// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ToolsMenu } from "./ToolsMenu";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("opens the add menu and runs the upload it offers", async () => {
  const onAttach = vi.fn(async () => {});
  render(
    <ToolsMenu
      onAttach={onAttach}
      citationFormat={null}
      defaultCitationFormat="inline"
      onCitationFormatChange={vi.fn()}
    />,
  );

  await userEvent.click(screen.getByRole("button", { name: "Add" }));
  expect(onAttach).not.toHaveBeenCalled();

  await userEvent.click(await screen.findByRole("menuitem", { name: /Upload files/ }));
  await waitFor(() => expect(onAttach).toHaveBeenCalledTimes(1));
});

it("shows citations but hides upload when onAttach is not provided", async () => {
  render(
    <ToolsMenu
      citationFormat={null}
      defaultCitationFormat="inline"
      onCitationFormatChange={vi.fn()}
    />,
  );

  await userEvent.click(screen.getByRole("button", { name: "Add" }));
  expect(screen.queryByRole("menuitem", { name: /Upload files/ })).toBeNull();
  expect(screen.getByRole("menuitem", { name: /Citations/ })).toBeInTheDocument();
});
