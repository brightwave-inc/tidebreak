// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const source = readFileSync(
  path.resolve(process.cwd(), "../src/browser_semantics.rs"),
  "utf8",
);

function rustScript(name: string): string {
  const match = source.match(
    new RegExp(String.raw`const ${name}: &str = r(#+)"([\s\S]*?)"\1;`),
  );
  if (!match) throw new Error(`Missing browser script ${name}`);
  return match[2];
}

function sharedScripts(script: string): string {
  return script
    .replace(
      "__TARGET_IDENTITY_STORE__",
      rustScript("TARGET_IDENTITY_STORE_SCRIPT"),
    )
    .replace(
      "__SENSITIVE_FIELD_POLICY__",
      rustScript("SENSITIVE_FIELD_POLICY"),
    );
}

type SnapshotNode = {
  ref: string;
  tag: string;
  role: string;
  name: string;
  inputType: string | null;
  href: string | null;
  sensitive: boolean;
  selector: string;
  framePath: string[];
  actions: string[];
  actionExecutionModes: Record<string, string[]>;
};

function snapshot(): SnapshotNode[] {
  const script = sharedScripts(rustScript("SNAPSHOT_SCRIPT"))
    .replace("__MAX_NODES__", "50")
    .replace("__MARKER__", "popup-select-fixture");
  return JSON.parse(window.eval(script)).nodes;
}

function select(node: SnapshotNode, inputMethod: "dom" | "native") {
  const payload = {
    inputMethod,
    framePath: node.framePath,
    selector: node.selector,
    marker: "popup-select-fixture",
    markerValue: node.ref,
    fingerprint: {
      tag: node.tag,
      role: node.role,
      name: node.name,
      inputType: node.inputType,
      href: node.href,
      sensitive: node.sensitive,
    },
    action: { type: "select", value: "test" },
  };
  const script = sharedScripts(rustScript("NATIVE_ACTION_RESOLUTION_SCRIPT"))
    .replace(
      "/*__RESOLVED_ACTION__*/",
      inputMethod === "dom" ? rustScript("BACKGROUND_DOM_ACTION_SCRIPT") : "",
    )
    .replace("__PAYLOAD__", JSON.stringify(payload));
  return JSON.parse(window.eval(script));
}

beforeEach(() => {
  document.body.innerHTML = `
    <input id="editor" aria-label="Editor" value="keep typing" />
    <select id="popup" aria-label="Environment">
      <option value="local">Local</option><option value="test">Test</option>
    </select>
    <select id="listbox" aria-label="Listbox" size="2">
      <option value="local">Local</option><option value="test">Test</option>
    </select>
    <input id="password" type="password" value="private" />
  `;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 10,
    y: 10,
    left: 10,
    top: 10,
    right: 110,
    bottom: 40,
    width: 100,
    height: 30,
    toJSON: () => ({}),
  });
  Object.defineProperty(document, "elementFromPoint", {
    configurable: true,
    value: () => document.querySelector("#popup"),
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  Reflect.deleteProperty(document, "elementFromPoint");
  document.body.innerHTML = "";
});

describe("popup select browser actions", () => {
  it("advertises background select and changes its value without moving focus", () => {
    const editor = document.querySelector<HTMLInputElement>("#editor")!;
    const popup = document.querySelector<HTMLSelectElement>("#popup")!;
    editor.focus();
    editor.setSelectionRange(2, 6);
    const events: Array<{ type: string; trusted: boolean }> = [];
    for (const type of ["input", "change"]) {
      popup.addEventListener(type, (event) => {
        events.push({ type: event.type, trusted: event.isTrusted });
      });
    }
    const node = snapshot().find(
      (candidate) => candidate.selector === "#popup",
    )!;
    expect(node.actions).toEqual(["select", "human_takeover"]);
    expect(node.actionExecutionModes).toEqual({ select: ["background"] });
    const result = select(node, "dom");
    expect(result.status).toBe("ready");
    expect(result.inputDispatched).toBe(true);
    expect(popup.value).toBe("test");
    expect(events).toEqual([
      { type: "input", trusted: false },
      { type: "change", trusted: false },
    ]);
    expect(document.activeElement).toBe(editor);
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([2, 6]);
  });

  it("keeps native popup selection refused without changing the page", () => {
    const popup = document.querySelector<HTMLSelectElement>("#popup")!;
    const onChange = vi.fn();
    popup.addEventListener("change", onChange);
    const node = snapshot().find(
      (candidate) => candidate.selector === "#popup",
    )!;
    const result = select(node, "native");
    expect(result.status).toBe("unsupported_native");
    expect(result.message).toContain("Take over the browser");
    expect(result.inputDispatched).not.toBe(true);
    expect(popup.value).toBe("local");
    expect(onChange).not.toHaveBeenCalled();
  });

  it("preserves listbox actions and sensitive-field takeover", () => {
    const nodes = snapshot();
    const listbox = nodes.find(
      (candidate) => candidate.selector === "#listbox",
    )!;
    expect(listbox.actions).toContain("select");
    expect(listbox.actionExecutionModes).toEqual({});
    const password = nodes.find(
      (candidate) => candidate.selector === "#password",
    )!;
    expect(password.actions).toEqual(["human_takeover"]);
    expect(password.actionExecutionModes).toEqual({});
  });
});
