import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor } from "storybook/test";
import { CodeEditorGroups } from "@/code/CodeEditorGroups";
import { BrowserToolbar } from "@/code/browser/BrowserToolbar";
import {
  createBrowserSession,
  finishBrowserNavigation,
} from "@/code/browser/browserSession";
import { ComputerUseActionStatus } from "@/ComputerUseActionStatus";

const session = finishBrowserNavigation(
  createBrowserSession({ id: "agent-preview", workspaceId: "workspace-story" }),
  "http://localhost:5173/settings",
);
const openEvent = "storybook:open-agent-preview";

function Preview() {
  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <div className="flex h-control items-center border-b border-border px-3 text-xs font-medium">
        Browser (agent)
      </div>
      <BrowserToolbar
        session={session}
        address={session.address}
        addressError={null}
        canGoBack={false}
        canGoForward={false}
        onAddressChange={fn()}
        onNavigate={fn()}
        onBack={fn()}
        onForward={fn()}
        onReload={fn()}
        onStop={fn()}
        controller={{
          kind: "agent",
          label: "Code agent",
          action: "Testing the settings form",
          halted: false,
          takeoverRequired: false,
        }}
        onStopAgent={fn()}
        onSelectHistory={fn()}
        onOpenExternal={fn()}
        onOverlayOpenChange={fn()}
        onTakeOver={fn()}
      />
      <ComputerUseActionStatus
        now={1000}
        action={{
          actionId: "preview-action",
          sessionId: "code-session",
          source: "browser",
          action: "click",
          phase: "running",
          executionMode: "background",
          coordinateFrame: "viewport",
          startedAtMillis: 1000,
          visibleUntilMillis: 2200,
        }}
      />
      <div className="min-h-0 flex-1 overflow-auto p-6">
        <h2 className="text-lg font-semibold">Project settings</h2>
        <p className="mt-2 text-sm text-muted-foreground">
          Your preview stays beside the source editor.
        </p>
        <label
          className="mt-6 block text-xs font-medium"
          htmlFor="preview-name"
        >
          Project name
        </label>
        <input
          id="preview-name"
          defaultValue="Tidebreak preview"
          className="mt-2 h-control w-full rounded-md border bg-background px-3 text-sm"
        />
        <button
          type="button"
          className="mt-4 h-control rounded-md bg-foreground px-3 text-sm font-medium text-background"
        >
          Save changes
        </button>
      </div>
    </div>
  );
}

function EditorGroupsStory({ preview = false }: { preview?: boolean }) {
  const [opened, setOpened] = useState(preview);
  useEffect(() => {
    const open = () => setOpened(true);
    window.addEventListener(openEvent, open);
    return () => window.removeEventListener(openEvent, open);
  }, []);
  return (
    <div className="h-dvh bg-background">
      <CodeEditorGroups
        primary={
          <div className="flex h-full min-h-0 flex-col">
            <div className="flex h-control items-center border-b border-border px-3 font-mono text-xs">
              src/Settings.tsx
            </div>
            <textarea
              aria-label="Source editor"
              spellCheck={false}
              defaultValue={
                "export function Settings() {\n  const [name, setName] = useState('Tidebreak');\n\n  return (\n    <form onSubmit={saveSettings}>\n      <input value={name} onChange={setName} />\n      <button>Save changes</button>\n    </form>\n  );\n}"
              }
              className="min-h-0 w-full flex-1 resize-none bg-background p-4 font-mono text-sm leading-relaxed outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring"
            />
          </div>
        }
        secondary={opened ? <Preview /> : undefined}
      />
    </div>
  );
}

const meta = {
  title: "Code/Editor groups",
  component: EditorGroupsStory,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof EditorGroupsStory>;
export default meta;
type Story = StoryObj<typeof meta>;
export const EditorOnly: Story = {};
export const AgentPreview: Story = { args: { preview: true } };
export const PreviewOpensWhileTyping: Story = {
  play: async ({ canvas }) => {
    const editor = canvas.getByRole("textbox", {
      name: "Source editor",
    }) as HTMLTextAreaElement;
    await userEvent.click(editor);
    editor.setSelectionRange(6, 14);
    window.dispatchEvent(new Event(openEvent));
    await waitFor(() =>
      expect(canvas.getByText("Browser (agent)")).toBeVisible(),
    );
    await expect(canvas.getByRole("textbox", { name: "Source editor" })).toBe(
      editor,
    );
    await expect(editor).toHaveFocus();
    await expect(editor.selectionStart).toBe(6);
    await expect(editor.selectionEnd).toBe(14);
  },
};
