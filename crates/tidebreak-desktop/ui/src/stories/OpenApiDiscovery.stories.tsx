import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import {
  DiscoveryResults,
  NoPublicDocumentGuidance,
} from "@/settings/OpenApiDiscovery";

const meta = {
  title: "Settings/OpenAPI discovery",
} satisfies Meta;

export default meta;

type Story = StoryObj;

export const Searching: Story = {
  render: () => (
    <DiscoveryResults
      discovery={null}
      discovering
      onChoose={fn()}
    />
  ),
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByText(
        /Searching well-known OpenAPI locations/,
      ),
    ).toBeVisible();
  },
};

export const FoundCandidates: Story = {
  render: () => (
    <DiscoveryResults
      discovering={false}
      onChoose={fn()}
      discovery={{
        candidates: [
          {
            url: "https://api.example.com/openapi.json",
            operation_count: 12,
            unsupported_reason: null,
          },
          {
            url: "https://api.example.com/openapi.yaml",
            operation_count: null,
            unsupported_reason:
              "YAML OpenAPI documents are not supported; convert to JSON",
          },
        ],
        tried: [
          "https://api.example.com/openapi.json",
          "https://api.example.com/openapi.yaml",
        ],
      }}
    />
  ),
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByRole("button", {
        name: /Use this document/,
      }),
    ).toBeVisible();
  },
};

export const NothingFound: Story = {
  render: () => (
    <DiscoveryResults
      discovering={false}
      onChoose={fn()}
      discovery={{
        candidates: [],
        tried: [
          "https://api.example.com/openapi.json",
          "https://api.example.com/swagger.json",
          "https://api.example.com/docs/openapi.json",
        ],
      }}
    />
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText(/No OpenAPI document turned up/),
    ).toBeVisible();
    await userEvent.click(canvas.getByText("Locations tried"));
    await expect(
      canvas.getByText("https://api.example.com/openapi.json"),
    ).toBeVisible();
  },
};

export const NoPublicDocument: Story = {
  render: () => <NoPublicDocumentGuidance onPasteExample={fn()} />,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      canvas.getByText("No public OpenAPI document?"),
    );
    await expect(
      await canvas.findByRole("button", { name: /Paste this example/ }),
    ).toBeVisible();
  },
};
