import type { Meta, StoryObj } from "@storybook/react-vite";
import { setEditorPreference } from "@/code/editorPreference";
import type { ExternalEditorId } from "@/code/editorPreference";
import type { ExternalEditorProbe } from "@/code/codeWorktreeHost";
import { ExternalEditorSection } from "@/settings/ExternalEditorSection";
import { SettingsPanel } from "@/settings/primitives";

/**
 * Settings → Coding harnesses → External editor.
 *
 * The reader picks one editor and every "Open in …" in the product starts
 * naming it. The rows below the picker answer the doctor's question about
 * editors — is this one on this computer, and where — so a choice that would
 * fail says so before it is made rather than after.
 */

function EditorShowcase({
  probes,
  canDetect = true,
}: {
  probes?: ExternalEditorProbe[];
  canDetect?: boolean;
  /** Read by `beforeEach` to seed the store, not by the component. */
  editor?: ExternalEditorId;
  customProgram?: string;
}) {
  return (
    <SettingsPanel
      title="Coding harnesses"
      description="Coding engines on this computer, and the editor Tidebreak hands files to."
    >
      <ExternalEditorSection
        canDetect={canDetect}
        detect={async () => probes ?? []}
      />
    </SettingsPanel>
  );
}

const INSTALLED: ExternalEditorProbe[] = [
  { id: "vscode", program: "/usr/local/bin/code" },
  { id: "cursor", program: "/opt/homebrew/bin/cursor" },
  { id: "zed", program: "/Applications/Zed.app/Contents/MacOS/cli" },
  { id: "jetbrains", program: null },
];

const meta = {
  title: "Settings/External editor",
  component: EditorShowcase,
  parameters: { layout: "fullscreen" },
  beforeEach: ({ args }) => {
    setEditorPreference({
      editor: args.editor ?? "vscode",
      customProgram: args.customProgram ?? "",
    });
    return () => setEditorPreference({ editor: "vscode", customProgram: "" });
  },
} satisfies Meta<typeof EditorShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Three editors found, one missing, and the chosen one confirmed ready. */
export const Detected: Story = {
  args: { probes: INSTALLED },
};

/** The chosen editor is not installed, so the field says what closes the gap. */
export const ChosenEditorMissing: Story = {
  args: { probes: INSTALLED, editor: "jetbrains" },
};

/** A custom command takes a program path and no flags. */
export const CustomCommand: Story = {
  args: {
    probes: INSTALLED,
    editor: "custom",
    customProgram: "/opt/homebrew/bin/nvim",
  },
};

/** Nothing installed: every row says so rather than the list disappearing. */
export const NothingInstalled: Story = {
  args: {
    probes: [
      { id: "vscode", program: null },
      { id: "cursor", program: null },
      { id: "zed", program: null },
      { id: "jetbrains", program: null },
    ],
  },
};

/** Attached to another machine: no probe to run, and the field says why. */
export const AttachedRemotely: Story = {
  args: { canDetect: false },
};
