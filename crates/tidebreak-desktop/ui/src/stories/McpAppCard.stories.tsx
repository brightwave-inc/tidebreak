import type { Meta, StoryObj } from "@storybook/react-vite";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api";
import { McpAppCard } from "@/McpAppCard";

const frameDocument = encodeURIComponent(`<!doctype html>
<html>
  <head>
    <style>
      html { color-scheme: light; }
      html[data-theme="dark"] { color-scheme: dark; }
      html, body { margin: 0; background: Canvas; }
      body { padding: 12px; color: GrayText; font: 12px/1.5 system-ui, sans-serif; }
    </style>
  </head>
  <body>
    Waiting for the tool result…
    <script>
      const applyTheme = (theme) => {
        document.documentElement.dataset.theme = theme;
      };
      window.addEventListener("message", (event) => {
        const message = event.data;
        if (message?.id === 1 && message?.result?.hostContext?.theme) {
          applyTheme(message.result.hostContext.theme);
          parent.postMessage({
            jsonrpc: "2.0",
            method: "ui/notifications/initialized",
          }, "*");
        }
        if (message?.method === "ui/notifications/host-context-changed") {
          applyTheme(message.params?.theme);
        }
      });
      parent.postMessage({
        jsonrpc: "2.0",
        id: 1,
        method: "ui/initialize",
        params: {
          appInfo: { name: "Story app", version: "1.0.0" },
          appCapabilities: {},
          protocolVersion: "2026-01-26",
        },
      }, "*");
    </script>
  </body>
</html>`);

const client = {
  baseUrl: "",
  createMcpViewFrame: async () => ({
    frame_path: `data:text/html;charset=utf-8,${frameDocument}`,
  }),
  getMcpAppPayload: async () => {
    throw new Error("temporary payload failure");
  },
} as unknown as ApiClient;

const appContext = { client } as AppContextValue;

const meta = {
  title: "Apps/MCP App card",
  component: McpAppCard,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story, context) => (
      <AppContextProvider value={appContext}>
        <div
          className="bg-page-background min-h-screen w-full min-w-0 p-6 text-foreground"
          style={{
            width: context.globals.viewport === "compact" ? 420 : "100%",
            maxWidth: "100%",
          }}
        >
          <Story />
        </div>
      </AppContextProvider>
    ),
  ],
  args: {
    server: "issue-tracker",
    resourceUri: "ui://issue-tracker/issues.html",
    chatId: "chat-story",
    callId: "call-story",
  },
} satisfies Meta<typeof McpAppCard>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The frame loads, but its tool result needs an explicit retry. */
export const PayloadFailure: Story = {};
