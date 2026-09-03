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

export const SpecIndex: Story = {
  render: () => (
    <DiscoveryResults
      discovering={false}
      onChoose={fn()}
      discovery={{
        candidates: [
          {
            url: "https://developers.beehiiv.com/openapi.json",
            operation_count: null,
            unsupported_reason:
              "this URL is a specification index pointing to 3 child documents; use one of the child documents instead",
            child_urls: [
              "https://developers.beehiiv.com/openapi/webhooks.json",
              "https://developers.beehiiv.com/openapi/oauth2.json",
              "https://developers.beehiiv.com/openapi/api-reference.json",
            ],
          },
        ],
        tried: [
          "https://developers.beehiiv.com/openapi.json",
          "https://developers.beehiiv.com/openapi.yaml",
          "https://developers.beehiiv.com/swagger.json",
        ],
      }}
    />
  ),
};

export const SpecIndexWithResolvedChildren: Story = {
  render: () => (
    <DiscoveryResults
      discovering={false}
      onChoose={fn()}
      discovery={{
        candidates: [
          {
            url: "https://developers.beehiiv.com/openapi.json",
            operation_count: null,
            unsupported_reason:
              "this URL is a specification index pointing to 3 child documents; use one of the child documents instead",
            child_urls: [
              "https://developers.beehiiv.com/openapi/webhooks.json",
              "https://developers.beehiiv.com/openapi/oauth2.json",
              "https://developers.beehiiv.com/openapi/api-reference.json",
            ],
          },
          {
            url: "https://developers.beehiiv.com/openapi/webhooks.json",
            operation_count: 4,
            unsupported_reason: null,
          },
          {
            url: "https://developers.beehiiv.com/openapi/oauth2.json",
            operation_count: 6,
            unsupported_reason: null,
          },
          {
            url: "https://developers.beehiiv.com/openapi/api-reference.json",
            operation_count: 42,
            unsupported_reason: null,
          },
        ],
        tried: [
          "https://developers.beehiiv.com/openapi.json",
          "https://developers.beehiiv.com/openapi.yaml",
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
