// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient, RuntimeSettings } from "../api";
import { AgentsPanel } from "./AgentsPanel";
import { useUiStore } from "../UiStore";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
  useUiStore.setState({ activeTurnSendMode: "queue" });
});

describe("AgentsPanel", () => {
  it("loads and saves the agent limits and check-in cadences", async () => {
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
      computer_use_enabled: true,
    };
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
    await screen.findByText("Active background agents per work");
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
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));
    await waitFor(() =>
      expect(putSettings).toHaveBeenCalledWith({
        max_active_background_agents: 8,
        sandbox_agent_checkin_steps: 250,
        sandbox_agent_error_checkin: 3,
      }),
    );
  });
});
