// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer, insertPluginDirective } from "./Composer";
import type { PluginInfo } from "./api";

afterEach(cleanup);

const DOCUMENTS: PluginInfo = {
  name: "documents",
  display_name: "Documents",
  description: "Writes Word, Excel, and PowerPoint files.",
  category: "documents",
  origin: "builtin",
  capabilities: [],
  enabled: false,
  skills: [],
};

it("engages a plugin from the tools menu and says so in the draft", async () => {
  const onSelect = vi.fn();
  const onDraftChange = vi.fn();
  const user = userEvent.setup();
  render(
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft="Summarize the report"
      plugins={{ items: [DOCUMENTS], onSelect }}
      onDraftChange={onDraftChange}
      onSend={vi.fn(async () => {})}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />,
  );

  await user.click(screen.getByRole("button", { name: "Tools" }));
  await user.click(screen.getByRole("menuitem", { name: /Documents/ }));

  expect(onSelect).toHaveBeenCalledWith(DOCUMENTS);
  expect(onDraftChange).toHaveBeenCalledWith(
    "Summarize the report Use the Documents plugin: ",
  );
});

it("puts the directive at the caret without running words together", () => {
  expect(insertPluginDirective("", "Use it: ", 0, 0)).toEqual({
    text: "Use it: ",
    caret: 8,
  });
  // Mid-draft, over a selection, and after text that already ends in a space.
  expect(insertPluginDirective("draft here", "Use it: ", 5, 10)).toEqual({
    text: "draft Use it: ",
    caret: 14,
  });
  expect(insertPluginDirective("draft ", "Use it: ", 6, 6)).toEqual({
    text: "draft Use it: ",
    caret: 14,
  });
});
