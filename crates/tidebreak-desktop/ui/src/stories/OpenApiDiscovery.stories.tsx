import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
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
    <DiscoveryResults discovery={null} discovering onChoose={fn()} />
  ),
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
};

export const NoPublicDocument: Story = {
  render: () => <NoPublicDocumentGuidance onPasteExample={fn()} />,
};
