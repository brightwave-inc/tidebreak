// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeTriggerSnapshot } from "@/api/types";
import { CodeTriggerRules } from "./CodeTriggerRules";

afterEach(cleanup);

function trigger(): CodeTriggerSnapshot {
  return {
    id: "trigger-checks",
    repo_id: "repo-1",
    condition: "checks_failed",
    action: "deliver",
    enabled: true,
    created_at: "2026-08-22T12:00:00Z",
    updated_at: "2026-08-22T12:00:00Z",
  };
}

describe("CodeTriggerRules", () => {
  it("chooses notify before arming a new rule", async () => {
    const user = userEvent.setup();
    const onArm = vi.fn();
    render(
      <CodeTriggerRules
        triggers={[]}
        target={null}
        onArm={onArm}
        onSetEnabled={vi.fn()}
        onChangeAction={vi.fn()}
        onDelete={vi.fn()}
      />,
    );

    await user.click(
      screen.getByRole("combobox", { name: "Checks fail action" }),
    );
    await user.click(screen.getByRole("option", { name: "Just notify me" }));
    expect(onArm).not.toHaveBeenCalled();

    await user.click(screen.getByRole("switch", { name: "Checks fail" }));
    expect(onArm).toHaveBeenCalledWith("checks_failed", "notify");
  });

  it("updates and deletes an armed rule", async () => {
    const user = userEvent.setup();
    const armed = trigger();
    const onChangeAction = vi.fn();
    const onDelete = vi.fn();
    render(
      <CodeTriggerRules
        triggers={[armed]}
        target={null}
        onArm={vi.fn()}
        onSetEnabled={vi.fn()}
        onChangeAction={onChangeAction}
        onDelete={onDelete}
      />,
    );

    await user.click(
      screen.getByRole("combobox", { name: "Checks fail action" }),
    );
    await user.click(screen.getByRole("option", { name: "Just notify me" }));
    expect(onChangeAction).toHaveBeenCalledWith(armed, "notify");

    await user.click(
      screen.getByRole("button", { name: "Delete Checks fail trigger" }),
    );
    expect(onDelete).toHaveBeenCalledWith(armed);
  });
});
