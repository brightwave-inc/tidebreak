import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ApiClient } from "@/api/client";
import { FileViewer } from "@/code/FileViewer";

type FileScenario = "image" | "image-failure" | "unsupported";

const imageSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800" viewBox="0 0 1200 800">
  <rect width="1200" height="800" fill="#e8ecef"/>
  <rect x="72" y="72" width="1056" height="656" rx="24" fill="#fbfcfd" stroke="#c7d0d8"/>
  <text x="124" y="156" font-family="system-ui, sans-serif" font-size="30" font-weight="600" fill="#23313c">Workspace architecture</text>
  <text x="124" y="196" font-family="system-ui, sans-serif" font-size="17" fill="#687985">Review capture · September 9, 2026</text>
  <rect x="124" y="258" width="260" height="166" rx="14" fill="#dce8e7" stroke="#aac2c0"/>
  <rect x="470" y="258" width="260" height="166" rx="14" fill="#e5e8f0" stroke="#b8bfd0"/>
  <rect x="816" y="258" width="260" height="166" rx="14" fill="#e9e5ef" stroke="#c6bcd0"/>
  <path d="M384 341h86M730 341h86" stroke="#7c8c97" stroke-width="4" stroke-linecap="round"/>
  <text x="160" y="334" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#29413f">Conversation</text>
  <text x="506" y="334" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#30394d">Workspace</text>
  <text x="852" y="334" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#42364e">Review</text>
  <text x="160" y="374" font-family="system-ui, sans-serif" font-size="16" fill="#61706f">Intent and progress</text>
  <text x="506" y="374" font-family="system-ui, sans-serif" font-size="16" fill="#697184">Files and terminal</text>
  <text x="852" y="374" font-family="system-ui, sans-serif" font-size="16" fill="#766b7e">Checks and comments</text>
  <rect x="124" y="500" width="952" height="142" rx="14" fill="#f2f4f5" stroke="#d4dadd"/>
  <text x="160" y="555" font-family="ui-monospace, monospace" font-size="16" fill="#52616b">assets/workspace-architecture.svg</text>
  <text x="160" y="598" font-family="system-ui, sans-serif" font-size="18" fill="#33424c">Image files open in the same center pane as source files.</text>
</svg>`;

function clientFor(scenario: FileScenario) {
  const image = scenario !== "unsupported";
  return {
    getCodeWorkspaceBlob: async () => ({
      path: image ? "assets/workspace-architecture.svg" : "build/archive.zip",
      content: "",
      truncated: false,
      binary: true,
    }),
    getCodeWorkspaceFile: async () => {
      if (scenario === "image-failure") {
        throw new Error("The workspace file could not be read.");
      }
      return {
        bytes: new TextEncoder().encode(imageSvg),
        contentType: "image/svg+xml",
      };
    },
  } as Pick<ApiClient, "getCodeWorkspaceBlob" | "getCodeWorkspaceFile">;
}

function FileViewerStory({ scenario }: { scenario: FileScenario }) {
  const path =
    scenario === "unsupported"
      ? "build/archive.zip"
      : "assets/workspace-architecture.svg";
  return (
    <div className="h-[680px] overflow-hidden rounded-lg border bg-page-background">
      <FileViewer
        client={clientFor(scenario)}
        workspaceId="workspace-storybook"
        path={path}
      />
    </div>
  );
}

const meta = {
  title: "Code/File viewer",
  component: FileViewerStory,
  args: { scenario: "image" },
  argTypes: {
    scenario: {
      control: "select",
      options: ["image", "image-failure", "unsupported"],
    },
  },
} satisfies Meta<typeof FileViewerStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ImageFile: Story = {};

export const ImageLoadFailure: Story = {
  args: { scenario: "image-failure" },
};

export const UnsupportedBinaryFile: Story = {
  args: { scenario: "unsupported" },
};

export const CompactImageFile: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
