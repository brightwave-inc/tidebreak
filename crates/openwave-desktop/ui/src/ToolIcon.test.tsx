import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ToolIcon } from "./ToolIcon";
import { toolCallPresentation, type ToolCallStatus } from "./ToolCallCard";

/**
 * Every renderer tool name, in the order the union declares them. The point of
 * listing them here is that the union is the contract: `Record<RendererToolName, _>`
 * makes a missing icon a compile error, and this asserts the runtime half —
 * that each one actually resolves to distinct, non-fallback presentation.
 */
const TOOL_NAMES = [
  "search",
  "list_sources",
  "read_source",
  "read_tool_result",
  "web_search",
  "read_delegated_file",
  "read_file",
  "list_dir",
  "write_file",
  "create_deliverable",
  "request_folder_access",
  "connect_folder",
  "list_connected_folders",
  "list_folder",
  "read_connected_file",
  "import_connected_file",
  "spawn_sandbox_agent",
  "wait_for_agents",
  "ask_user_questions",
  "exec",
] as const;

describe("tool presentation coverage", () => {
  it("gives every allowlisted tool its own copy rather than the fallback", () => {
    // `list_folder` and `import_connected_file` both reached the allowlist
    // without copy or an icon and silently read as "Use a tool".
    for (const name of TOOL_NAMES) {
      const presentation = toolCallPresentation(name, "running" as ToolCallStatus);
      expect(presentation.label, `${name} has no label`).not.toBe("Use a tool");
      expect(presentation.title, `${name} has no title`).not.toBe("Using a tool");
    }
  });

  it("renders an icon for every allowlisted tool", () => {
    for (const name of TOOL_NAMES) {
      const markup = renderToStaticMarkup(<ToolIcon name={name} />);
      expect(markup, `${name} rendered nothing`).toContain("<svg");
    }
  });

  it("folds an unrecognized name to the generic tool rather than guessing", () => {
    const presentation = toolCallPresentation("mcp__evil__exfiltrate", "running");
    expect(presentation.label).toBe("Use a tool");
    // `other` is the server's own fold, and shares that treatment.
    expect(toolCallPresentation("other", "running").label).toBe("Use a tool");
    expect(renderToStaticMarkup(<ToolIcon name="mcp__evil__exfiltrate" />)).toContain(
      "<svg",
    );
  });
});
