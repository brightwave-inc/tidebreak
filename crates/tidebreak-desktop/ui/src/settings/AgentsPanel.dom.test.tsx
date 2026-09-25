// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, RuntimeSettings } from "../api";
import { AgentsPanel } from "./AgentsPanel";
import { useUiStore } from "../UiStore";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
  useUiStore.setState({ activeTurnSendMode: "queue" });
});

const settings: RuntimeSettings = {
  model: null,
  has_api_key: false,
  chat_defaults: {
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: null,
  },
  max_active_background_agents: 5,
  sandbox_agent_checkin_steps: 100,
  sandbox_agent_error_checkin: 5,
  compaction: {
    threshold_fraction: 0.75,
    target_fraction: 0.25,
    min_threshold_tokens: 50000,
    protect_recent_messages: 5,
  },
  model_visibility_overrides: {},
  prompt_cache_retention: "five_minutes",
  computer_use_enabled: true,
  code_turn_recaps_enabled: true,
  rewrite_closing_messages: false,
  harness_update_channel: "pinned",
  git_source_control: {
    auto_rename_branches: true,
    keep_local_main_up_to_date: true,
    branch_prefix_mode: "account",
    effective_branch_prefix: "tidebreak/",
  },
  memory: { enabled: true, capture_enabled: false, capture_ready: false },
};

describe("AgentsPanel", () => {
  it("loads and saves the agent limits and check-in cadences", async () => {
    const putSettings = vi.fn().mockResolvedValue({
      ...settings,
      max_active_background_agents: 8,
      sandbox_agent_checkin_steps: 250,
      sandbox_agent_error_checkin: 3,
    });
    const client = {
      getSettings: vi.fn().mockResolvedValue(settings),
      putSettings,
    } as unknown as ApiClient;

    render(<AgentsPanel client={client} />);
    await screen.findByText("Background agents per conversation");
    expect(screen.getByText(/always steers immediately/)).toBeVisible();
    expect(screen.getByRole("radio", { name: "Queue" })).toBeChecked();
    fireEvent.click(screen.getByRole("radio", { name: "Steer" }));
    expect(useUiStore.getState().activeTurnSendMode).toBe("steer");
    expect(window.localStorage.getItem("tidebreak.composer.sendMode")).toBe(
      "steer",
    );
    const [limit, steps, errors] = screen.getAllByRole("spinbutton");
    expect(limit).toHaveValue(5);
    expect(steps).toHaveValue(100);
    expect(errors).toHaveValue(5);
    fireEvent.change(limit, { target: { value: "8" } });
    fireEvent.change(steps, { target: { value: "250" } });
    fireEvent.change(errors, { target: { value: "3" } });
    fireEvent.blur(errors);
    expect(screen.queryByRole("button", { name: "Save settings" })).toBeNull();
    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        max_active_background_agents: 8,
        sandbox_agent_checkin_steps: 250,
        sandbox_agent_error_checkin: 3,
      }),
    );
  });

  it("turns off future fallback recaps", async () => {
    const settings: RuntimeSettings = {
      model: null,
      has_api_key: false,
      chat_defaults: {
        model: null,
        reasoning_effort: null,
        permission_mode: null,
        network_policy: null,
      },
      max_active_background_agents: 5,
      sandbox_agent_checkin_steps: 100,
      sandbox_agent_error_checkin: 5,
      compaction: {
        threshold_fraction: 0.75,
        target_fraction: 0.25,
        min_threshold_tokens: 50000,
        protect_recent_messages: 5,
      },
      model_visibility_overrides: {},
      prompt_cache_retention: "five_minutes",
      computer_use_enabled: true,
      code_turn_recaps_enabled: true,
      rewrite_closing_messages: false,
      harness_update_channel: "pinned",
      git_source_control: {
        auto_rename_branches: true,
        keep_local_main_up_to_date: true,
        branch_prefix_mode: "account",
        effective_branch_prefix: "tidebreak/",
      },
      memory: { enabled: true, capture_enabled: false, capture_ready: false },
    };
    const putSettings = vi.fn().mockResolvedValue({
      ...settings,
      code_turn_recaps_enabled: false,
    });
    const client = {
      getSettings: vi.fn().mockResolvedValue(settings),
      putSettings,
    } as unknown as ApiClient;

    render(<AgentsPanel client={client} />);
    const toggle = await screen.findByRole("switch", {
      name: "Write fallback recaps",
    });
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        code_turn_recaps_enabled: false,
      }),
    );
    expect(toggle).not.toBeChecked();
  });
});

describe("AgentsPanel load failures and validation", () => {
  it("clears the load failure once a retry reads the settings", async () => {
    const getSettings = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("Load failed"))
      .mockResolvedValue(settings);
    render(
      <AgentsPanel
        client={{ getSettings, putSettings: vi.fn() } as unknown as ApiClient}
      />,
    );

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveTextContent("Could not load agent settings");
    // Nothing can be edited until the settings load, so nothing typed is lost.
    for (const field of screen.getAllByRole("spinbutton")) {
      expect(field).toBeDisabled();
    }

    fireEvent.click(within(failure).getByRole("button", { name: "Try again" }));

    await waitFor(() =>
      expect(screen.getAllByRole("spinbutton")[0]).toHaveValue(5),
    );
    expect(screen.queryByRole("alert")).toBeNull();
    expect(getSettings).toHaveBeenCalledTimes(2);
  });

  it("shows a value it cannot save under its field, with no retry", async () => {
    const putSettings = vi.fn();
    render(
      <AgentsPanel
        client={
          {
            getSettings: vi.fn().mockResolvedValue(settings),
            putSettings,
          } as unknown as ApiClient
        }
      />,
    );
    await waitFor(() =>
      expect(screen.getAllByRole("spinbutton")[1]).toHaveValue(100),
    );
    const steps = screen.getAllByRole("spinbutton")[1];

    fireEvent.change(steps, { target: { value: "0" } });
    fireEvent.blur(steps);

    const message = await screen.findByRole("alert");
    expect(message).toHaveTextContent(/Check-in steps must be a whole number/);
    expect(message.closest('[data-slot="notice"]')).toBeNull();
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
    expect(steps).toHaveAttribute("aria-invalid", "true");
    expect(steps).toHaveValue(0);
    expect(putSettings).not.toHaveBeenCalled();
  });
});
