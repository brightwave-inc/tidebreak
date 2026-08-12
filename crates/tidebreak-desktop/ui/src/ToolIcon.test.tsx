import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ToolIcon } from "./ToolIcon";
import { toolCallPresentation, type ToolCallStatus } from "./ToolCallCard";
import { RENDERER_TOOL_NAMES } from "./generated/wire";

/**
 * Every renderer tool name that should have presentation of its own, taken from
 * the generated vocabulary rather than a list kept here.
 *
 * `Record<RendererToolName, _>` already makes a missing icon or a missing copy
 * entry a compile error. This asserts the runtime half — that each name
 * resolves to real wording and a real glyph rather than the fallback — and
 * walking the generated list is what keeps a newly added tool from being
 * checked by neither side.
 *
 * `other` is excluded because it is the server's fold for anything
 * unrecognized, so the generic presentation is its correct answer. The last
 * test below covers it.
 */
const TOOL_NAMES = RENDERER_TOOL_NAMES.filter((name) => name !== "other");

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
