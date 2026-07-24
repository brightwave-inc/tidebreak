// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ProviderInfo } from "../api";
import { ProvidersPanel } from "./ProvidersPanel";

const compatible: ProviderInfo = {
  kind: "openai_compatible",
  enabled: true,
  has_credential: true,
  base_url: "http://127.0.0.1:1234/v1",
  models: [],
};

afterEach(cleanup);

describe("ProvidersPanel", () => {
  it("registers custom compatible models with explicit runtime limits", async () => {
    const putProvider = vi.fn().mockResolvedValue(compatible);
    const client = { putProvider } as unknown as ApiClient;

    render(
      <ProvidersPanel
        providers={[compatible]}
        client={client}
        onChanged={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    fireEvent.change(screen.getByLabelText("Custom model 1 ID"), {
      target: { value: " vendor/model " },
    });
    fireEvent.change(screen.getByLabelText("Custom model 1 display name"), {
      target: { value: " Vendor Model " },
    });
    fireEvent.change(screen.getByLabelText("Custom model 1 context tokens"), {
      target: { value: "65536" },
    });
    fireEvent.change(screen.getByLabelText("Custom model 1 max output"), {
      target: { value: "8192" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Save configuration" }),
    );

    await waitFor(() =>
      expect(putProvider).toHaveBeenCalledWith("openai_compatible", {
        enabled: true,
        base_url: "http://127.0.0.1:1234/v1",
        models: [
          {
            id: "vendor/model",
            display_name: "Vendor Model",
            context_window: 65_536,
            max_output_tokens: 8_192,
          },
        ],
      }),
    );
  });

  it("does not expose custom model registration for curated providers", () => {
    render(
      <ProvidersPanel
        providers={[
          {
            // `base_url` is omitted, not null, exactly as the server sends it.
            kind: "anthropic",
            enabled: false,
            has_credential: false,
            models: [],
          },
        ]}
        client={{} as ApiClient}
        onChanged={vi.fn()}
      />,
    );

    expect(
      screen.queryByRole("button", { name: "Add model" }),
    ).not.toBeInTheDocument();
  });
});
