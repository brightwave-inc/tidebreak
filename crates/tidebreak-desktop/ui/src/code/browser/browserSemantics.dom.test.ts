// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type DomFixture = { window: Window & typeof globalThis };
const { JSDOM } = createRequire(import.meta.url)("jsdom") as {
  JSDOM: new (
    html: string,
    options: { runScripts: string; url: string },
  ) => DomFixture;
};

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

function snapshot(realm = window): SnapshotNode[] {
  const script = sharedScripts(rustScript("SNAPSHOT_SCRIPT"))
    .replace("__MAX_NODES__", "50")
    .replace("__MARKER__", "popup-select-fixture");
  return JSON.parse(realm.eval(script)).nodes;
}

function act(
  node: SnapshotNode,
  inputMethod: "dom" | "native" | "independent_native",
  action: Record<string, unknown>,
  points?: Array<{ x: number; y: number }>,
  realm = window,
  fillStage?: "focus" | "select_all" | "insert" | "verify",
) {
  const payload = {
    fillStage,
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
    action,
    points,
  };
  const script = sharedScripts(rustScript("NATIVE_ACTION_RESOLUTION_SCRIPT"))
    .replace(
      "/*__RESOLVED_ACTION__*/",
      inputMethod === "dom" ? rustScript("BACKGROUND_DOM_ACTION_SCRIPT") : "",
    )
    .replace("__PAYLOAD__", JSON.stringify(payload));
  return JSON.parse(realm.eval(script));
}

function select(node: SnapshotNode, inputMethod: "dom" | "native") {
  return act(node, inputMethod, { type: "select", value: "test" });
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

describe("independent DOM event actions", () => {
  let realm: Window & typeof globalThis;
  let fixture: DomFixture;
  beforeEach(() => {
    // Use jsdom's own Window: Vitest's global proxy is not a UIEvent view.
    fixture = new JSDOM(document.body.innerHTML, {
      runScripts: "outside-only",
      url: "http://localhost/",
    });
    realm = fixture.window;
    for (const element of realm.document.querySelectorAll("input, select")) {
      element.getBoundingClientRect = () => ({
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
    }
    realm.document.elementFromPoint = () =>
      realm.document.querySelector("#popup");
  });
  afterEach(() => fixture.window.close());
  it("delivers a page key shortcut without changing the active editor or its selection", () => {
    const editor = realm.document.querySelector<HTMLInputElement>("#editor")!;
    const target = realm.document.querySelector<HTMLSelectElement>("#popup")!;
    editor.focus();
    editor.setSelectionRange(2, 6);
    let shortcutCount = 0;
    const events: Array<{
      type: string;
      key: string;
      trusted: boolean;
      meta: boolean;
    }> = [];
    for (const type of ["keydown", "keyup"]) {
      target.addEventListener(type, (event) => {
        const key = event as KeyboardEvent;
        events.push({
          type,
          key: key.key,
          trusted: key.isTrusted,
          meta: key.metaKey,
        });
        if (type === "keydown" && key.metaKey && key.key === "a")
          shortcutCount += 1;
      });
    }
    const node = snapshot(realm).find(
      (candidate) => candidate.selector === "#popup",
    )!;
    const result = act(
      node,
      "dom",
      { type: "key_chord", key: "a", modifiers: ["meta"] },
      undefined,
      realm,
    );
    expect(result.inputDispatched).toBe(true);
    expect(result.message).toContain("not applied");
    expect(shortcutCount).toBe(1);
    expect(events).toEqual([
      { type: "keydown", key: "a", trusted: false, meta: true },
      { type: "keyup", key: "a", trusted: false, meta: true },
    ]);
    expect(realm.document.activeElement).toBe(editor);
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([2, 6]);
    expect(editor.value).toBe("keep typing");
  });

  it.each([
    ["right_click", "contextmenu", 2],
    ["double_click", "dblclick", 0],
  ])(
    "delivers %s to its page handler without focusing the target",
    (type, expected, button) => {
      const editor = realm.document.querySelector<HTMLInputElement>("#editor")!;
      const target = realm.document.querySelector<HTMLSelectElement>("#popup")!;
      editor.focus();
      let handled = 0;
      target.addEventListener(String(expected), (event) => {
        expect((event as MouseEvent).button).toBe(button);
        expect(event.isTrusted).toBe(false);
        handled += 1;
      });
      const node = snapshot(realm).find(
        (candidate) => candidate.selector === "#popup",
      )!;
      expect(act(node, "dom", { type }, undefined, realm).inputDispatched).toBe(
        true,
      );
      expect(handled).toBe(1);
      expect(realm.document.activeElement).toBe(editor);
    },
  );

  it("delivers one bounded pointer drag to a canvas handler and releases its button", () => {
    const editor = realm.document.querySelector<HTMLInputElement>("#editor")!;
    const target = realm.document.createElement("canvas");
    target.id = "canvas";
    target.getBoundingClientRect =
      realm.document.querySelector("#popup")!.getBoundingClientRect;
    realm.document.body.appendChild(target);
    realm.document.elementFromPoint = () => target;
    editor.focus();
    editor.setSelectionRange(2, 6);
    let buttonHeld = false;
    let dropped = false;
    const moves: Array<[number, number]> = [];
    target.addEventListener("pointerdown", (event) => {
      expect((event as PointerEvent).isPrimary).toBe(false);
      expect(event.isTrusted).toBe(false);
      buttonHeld = true;
    });
    target.addEventListener("pointermove", (event) => {
      const pointer = event as PointerEvent;
      expect(pointer.buttons).toBe(1);
      expect(buttonHeld).toBe(true);
      moves.push([pointer.clientX, pointer.clientY]);
    });
    target.addEventListener("pointerup", (event) => {
      expect((event as PointerEvent).buttons).toBe(0);
      buttonHeld = false;
      dropped = true;
    });
    const node = snapshot(realm).find(
      (candidate) => candidate.selector === "#canvas",
    )!;
    const result = act(
      node,
      "dom",
      { type: "drag" },
      [
        { x: 2, y: 2 },
        { x: 70, y: 20 },
      ],
      realm,
    );
    expect(result.inputDispatched).toBe(true);
    expect(result.message).toContain(
      "Native drag-and-drop and trusted pointer behavior are not applied",
    );
    expect(moves).toHaveLength(8);
    expect(moves.at(-1)).toEqual([80, 30]);
    expect(dropped).toBe(true);
    expect(buttonHeld).toBe(false);
    expect(realm.document.activeElement).toBe(editor);
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([2, 6]);
  });

  it("refuses pointer drag before any event when the engine lacks PointerEvent", () => {
    const target = realm.document.querySelector<HTMLSelectElement>("#popup")!;
    const down = vi.fn();
    target.addEventListener("pointerdown", down);
    const node = snapshot(realm).find(
      (candidate) => candidate.selector === "#popup",
    )!;
    const previous = realm.PointerEvent;
    Object.defineProperty(realm, "PointerEvent", {
      configurable: true,
      value: undefined,
    });
    try {
      const result = act(
        node,
        "dom",
        { type: "drag" },
        [
          { x: 2, y: 2 },
          { x: 70, y: 20 },
        ],
        realm,
      );
      expect(result.status).toBe("unsupported_native");
      expect(result.inputDispatched).toBeUndefined();
      expect(down).not.toHaveBeenCalled();
    } finally {
      Object.defineProperty(realm, "PointerEvent", {
        configurable: true,
        value: previous,
      });
    }
  });
});

describe("independent host fill preparation", () => {
  function fieldTarget() {
    const field = document.querySelector<HTMLInputElement>("#editor")!;
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: () => field,
    });
    return {
      field,
      node: snapshot().find((candidate) => candidate.selector === "#editor")!,
    };
  }

  it("prepares DOM focus and selection without requiring a key native window", () => {
    const { field, node } = fieldTarget();
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const result = act(
      node,
      "independent_native",
      { type: "fill", value: "replacement" },
      undefined,
      window,
      "focus",
    );
    expect(result.status).toBe("ready");
    expect(result.inputDispatched).toBe(true);
    expect(result.targetDomFocused).toBe(true);
    expect(field.value).toBe("keep typing");
    expect([field.selectionStart, field.selectionEnd]).toEqual([
      0,
      field.value.length,
    ]);
    const insert = act(
      node,
      "independent_native",
      { type: "fill", value: "replacement" },
      undefined,
      window,
      "insert",
    );
    expect(insert.status).toBe("ready");
    expect(insert.targetFocused).toBe(true);
  });

  it("requires fresh selection and target identity before native insertion", () => {
    const { field, node } = fieldTarget();
    act(
      node,
      "independent_native",
      { type: "fill", value: "replacement" },
      undefined,
      window,
      "focus",
    );
    field.setSelectionRange(1, 3);
    expect(
      act(
        node,
        "independent_native",
        { type: "fill", value: "replacement" },
        undefined,
        window,
        "insert",
      ).status,
    ).toBe("pending_native_input");
    field.replaceWith(field.cloneNode(true));
    expect(
      act(
        node,
        "independent_native",
        { type: "fill", value: "replacement" },
        undefined,
        window,
        "insert",
      ).status,
    ).toBe("stale_target");
  });

  it("reports a mutation when a focus handler replaces the target", () => {
    const { field, node } = fieldTarget();
    field.addEventListener("focus", () =>
      field.replaceWith(field.cloneNode(true)),
    );
    const result = act(
      node,
      "independent_native",
      { type: "fill", value: "replacement" },
      undefined,
      window,
      "focus",
    );
    expect(result.status).toBe("stale_target");
    expect(result.inputDispatched).toBe(true);
    expect(document.querySelector<HTMLInputElement>("#editor")!.value).toBe(
      "keep typing",
    );
  });
});

describe("independent native input boundary", () => {
  it("refuses native canvas drag while preserving DOM drag", () => {
    document.body.innerHTML =
      '<canvas id="canvas" tabindex="0" aria-label="Drawing"></canvas>';
    const canvas = document.querySelector<HTMLCanvasElement>("#canvas")!;
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: () => canvas,
    });
    const node = snapshot().find(
      (candidate) => candidate.selector === "#canvas",
    )!;
    const action = { type: "drag", to: { x: 20, y: 20 } };
    const result = act(node, "independent_native", action);
    expect(result.status).toBe("unsupported_native");
    expect(result.inputDispatched).not.toBe(true);
    // The real-realm drag test covers the retained DOM pointer sequence.
    expect(node.actions).toContain("drag");
  });
});
