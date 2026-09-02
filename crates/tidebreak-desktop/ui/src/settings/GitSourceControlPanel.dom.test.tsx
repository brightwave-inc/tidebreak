// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { RuntimeSettings } from "@/api/types";
import { GitSourceControlPanel } from "./GitSourceControlPanel";

const initial: RuntimeSettings = {
  model: null,
  has_api_key: false,
  chat_defaults: {
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: null,
  },
  max_active_background_agents: 6,
  sandbox_agent_checkin_steps: 18,
  sandbox_agent_error_checkin: 4,
  compaction: {
    threshold_fraction: 0.78,
    target_fraction: 0.52,
    min_threshold_tokens: 16_000,
    protect_recent_messages: 8,
  },
  model_visibility_overrides: {},
  prompt_cache_retention: "five_minutes",
  computer_use_enabled: true,
  code_turn_recaps_enabled: true,
  rewrite_closing_messages: false,
  harness_update_channel: "pinned",
  git_source_control: {
    auto_rename_branches: true,
    branch_prefix_mode: "account",
    account_prefix: "alex/",
    effective_branch_prefix: "alex/",
  },
  memory: { enabled: true, capture_enabled: false, capture_ready: false },
};

afterEach(cleanup);

describe("GitSourceControlPanel", () => {
  it("saves branch renaming and custom prefix choices", async () => {
    const putSettings = vi.fn(async (body) => ({
      ...initial,
      git_source_control: {
        ...initial.git_source_control,
        ...body.git_source_control,
        custom_branch_prefix: body.git_source_control?.custom_branch_prefix
          ? `${body.git_source_control.custom_branch_prefix}/`
          : undefined,
        effective_branch_prefix: body.git_source_control?.custom_branch_prefix
          ? `${body.git_source_control.custom_branch_prefix}/`
          : initial.git_source_control.effective_branch_prefix,
      },
    }));
    const user = userEvent.setup();
    render(
      <GitSourceControlPanel
        client={{ getSettings: async () => initial, putSettings } as never}
      />,
    );

    await user.click(
      await screen.findByRole("switch", {
        name: "Rename generated branches automatically",
      }),
    );
    expect(putSettings).toHaveBeenCalledWith({
      git_source_control: { auto_rename_branches: false },
    });

    await user.click(screen.getByRole("combobox", { name: "Branch prefix" }));
    await user.click(await screen.findByRole("option", { name: "Custom" }));
    const input = await screen.findByLabelText("Custom prefix");
    fireEvent.change(input, { target: { value: "team/alex" } });
    fireEvent.blur(input);

    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        git_source_control: { custom_branch_prefix: "team/alex" },
      }),
    );
    expect(screen.getByText("team/alex/fix-flaky-auth-retry")).toBeVisible();
  });

  it("restores a toggle when the server rejects the change", async () => {
    const user = userEvent.setup();
    render(
      <GitSourceControlPanel
        client={
          {
            getSettings: async () => initial,
            putSettings: async () => {
              throw new Error("Git settings could not be saved.");
            },
          } as never
        }
      />,
    );

    const toggle = await screen.findByRole("switch", {
      name: "Rename generated branches automatically",
    });
    await user.click(toggle);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Git settings could not be saved.",
    );
    expect(toggle).toBeChecked();
  });
});
