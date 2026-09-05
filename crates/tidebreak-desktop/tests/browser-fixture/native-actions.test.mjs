import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const fixtureRoot = dirname(fileURLToPath(import.meta.url));
const semanticsPath = resolve(fixtureRoot, "../../src/browser_semantics.rs");
const semanticsSource = await readFile(semanticsPath, "utf8");
const policyMatch = semanticsSource.match(
  /const SENSITIVE_FIELD_POLICY: &str = r##"([\s\S]*?)"##;/,
);
const actionMatch = semanticsSource.match(
  /const NATIVE_ACTION_RESOLUTION_SCRIPT: &str = r#"([\s\S]*?)"#;/,
);
const identityStoreMatch = semanticsSource.match(
  /const TARGET_IDENTITY_STORE_SCRIPT: &str = r#"([\s\S]*?)"#;/,
);

assert.ok(policyMatch, "the native action resolver must use the shared field policy");
assert.ok(actionMatch, "the native action resolver must be present");
assert.ok(identityStoreMatch, "the native action resolver must use private target identities");

const targetIdentityStoreKey = Symbol.for("io.brightwave.tidebreak.browser.target-identities");
const targetIdentityStore = new WeakMap();
const fixtureTargetRefs = new WeakMap();
Object.defineProperty(globalThis, targetIdentityStoreKey, {
  value: targetIdentityStore,
  configurable: false,
  enumerable: false,
  writable: false,
});

class FixtureInput {}
class FixtureSelect {}
class FixtureTextArea {}

function view() {
  return {
    HTMLInputElement: FixtureInput,
    HTMLSelectElement: FixtureSelect,
    HTMLTextAreaElement: FixtureTextArea,
    innerHeight: 600,
    innerWidth: 800,
    scrollX: 0,
    scrollY: 0,
    getComputedStyle(element) {
      return {
        display: "block",
        visibility: "visible",
        opacity: "1",
        overflowX: "visible",
        overflowY: "visible",
        ...element.computedStyle,
      };
    },
  };
}

function button({ marker = "@e1", rect = { x: 20, y: 30, width: 120, height: 40 } } = {}) {
  const attributes = new Map([["aria-label", "Continue"]]);
  const element = {
    id: "continue",
    localName: "button",
    isConnected: true,
    isContentEditable: false,
    disabled: false,
    labels: [],
    form: null,
    parentElement: null,
    previousElementSibling: null,
    getAttribute(name) {
      return attributes.get(name) ?? null;
    },
    hasAttribute(name) {
      return attributes.has(name);
    },
    getRootNode() {
      return this.ownerDocument;
    },
    closest() {
      return null;
    },
    querySelectorAll() {
      return [];
    },
    matches() {
      return true;
    },
    contains(candidate) {
      return candidate === this;
    },
    getBoundingClientRect() {
      return {
        ...rect,
        left: rect.x,
        top: rect.y,
        right: rect.x + rect.width,
        bottom: rect.y + rect.height,
      };
    },
  };
  fixtureTargetRefs.set(element, marker);
  return element;
}

// Keyboard progression fixtures use inline listboxes; popup cases pass size 0 or 1.
function selectControl({ marker = "@e1", size = 4, multiple = false } = {}) {
  const attributes = new Map([["aria-label", "Mode"]]);
  const element = Object.assign(new FixtureSelect(), {
    id: "mode",
    localName: "select",
    size,
    multiple,
    isConnected: true,
    isContentEditable: false,
    disabled: false,
    labels: [],
    form: null,
    parentElement: null,
    previousElementSibling: null,
    options: [
      { disabled: false, value: "one" },
      { disabled: false, value: "two" },
    ],
    selectedIndex: 0,
    value: "one",
    getAttribute(name) {
      return attributes.get(name) ?? null;
    },
    hasAttribute(name) {
      return attributes.has(name);
    },
    getRootNode() {
      return this.ownerDocument;
    },
    closest() {
      return null;
    },
    querySelectorAll() {
      return [];
    },
    matches() {
      return true;
    },
    contains(candidate) {
      return candidate === this;
    },
    getBoundingClientRect() {
      return {
        x: 20,
        y: 30,
        width: 160,
        height: 36,
        left: 20,
        top: 30,
        right: 180,
        bottom: 66,
      };
    },
  });
  fixtureTargetRefs.set(element, marker);
  return element;
}

function textControl({ tag = "input", type = "text", marker = "@e1" } = {}) {
  const base = button({ marker });
  const attributes = new Map([["aria-label", "Search"]]);
  if (tag === "input") attributes.set("type", type);
  const element = Object.assign(
    tag === "textarea" ? new FixtureTextArea() : new FixtureInput(),
    base,
    {
      id: "search",
      localName: tag,
      type,
      readOnly: false,
      value: "old text",
      selectionStart: 0,
      selectionEnd: 8,
      getAttribute(name) {
        return attributes.get(name) ?? null;
      },
      hasAttribute(name) {
        return attributes.has(name);
      },
      setAttribute(name, value) {
        attributes.set(name, value);
      },
    },
  );
  fixtureTargetRefs.set(element, marker);
  return element;
}

function fillPayload(element, fillStage = "focus") {
  return {
    ...payload({
      selector: element.localName + ":nth-of-type(1)",
      fingerprint: {
        href: null,
        inputType: element.localName === "input" ? element.type : null,
        name: "Search",
        role: "textbox",
        sensitive: false,
        tag: element.localName,
      },
      action: { type: "fill", value: "new text" },
    }),
    fillStage,
  };
}

function keyboardPayload(element, action, focusStage = "acquire") {
  const request =
    element.localName === "select"
      ? payload({
          selector: "select:nth-of-type(1)",
          fingerprint: {
            href: null,
            inputType: null,
            name: "Mode",
            role: "combobox",
            sensitive: false,
            tag: "select",
          },
          action,
        })
      : { ...fillPayload(element), action };
  return { ...request, focusStage };
}

function nestedFocusFixture(element) {
  const leaf = documentFor(element);
  leaf.activeElement = element;
  const frames = [];
  const documents = [leaf];
  let doc = leaf;
  for (let depth = 0; depth < 2; depth += 1) {
    const frame = button({ rect: { x: 20, y: 20, width: 600, height: 500 } });
    frame.localName = "iframe";
    frame.contentDocument = doc;
    doc = documentFor(button(), { frame, hit: frame });
    doc.activeElement = frame;
    frames.unshift(frame);
    documents.unshift(doc);
  }
  return { doc, frames, documents, framePath: frames.map(() => "iframe:nth-of-type(1)") };
}

function documentFor(
  element,
  { hit = element, title = "Fixture", frame = null, scrollingElement = null } = {},
) {
  const root = scrollingElement ?? {
    clientHeight: 600,
    clientWidth: 800,
    scrollHeight: 600,
    scrollLeft: 0,
    scrollTop: 0,
    scrollWidth: 800,
  };
  const doc = {
    activeElement: null,
    hasFocus() {
      return true;
    },
    defaultView: view(),
    documentElement: root,
    scrollingElement: root,
    title,
    getElementById() {
      return null;
    },
    querySelector(selector) {
      return frame && selector === "iframe:nth-of-type(1)" ? frame : element;
    },
    elementFromPoint(x, y) {
      return typeof hit === "function" ? hit(x, y) : hit;
    },
  };
  element.ownerDocument = doc;
  return doc;
}

function payload({
  framePath = [],
  selector = "button:nth-of-type(1)",
  fingerprint = {
    href: null,
    inputType: null,
    name: "Continue",
    role: "button",
    sensitive: false,
    tag: "button",
  },
  action = { type: "hover" },
} = {}) {
  return {
    framePath,
    selector,
    marker: "__tidebreak_marker__",
    markerValue: "@e1",
    fingerprint,
    action,
  };
}

function nestedScrollFixture({ covered = false, visible = false } = {}) {
  const containerRect = {
    x: 100,
    y: 100,
    width: 300,
    height: 200,
    left: 100,
    top: 100,
    right: 400,
    bottom: 300,
  };
  const contentTop = 700;
  const target = button();
  const containerChild = { localName: "div" };
  const overlay = { localName: "div" };
  const container = {
    localName: "div",
    parentElement: null,
    clientHeight: containerRect.height,
    clientLeft: 0,
    clientTop: 0,
    clientWidth: containerRect.width,
    computedStyle: { overflowX: "hidden", overflowY: "auto" },
    scrollHeight: 1_000,
    scrollLeft: 0,
    scrollTop: visible ? 615 : 0,
    scrollWidth: containerRect.width,
    contains(candidate) {
      return candidate === this || candidate === containerChild || candidate === target;
    },
    getBoundingClientRect() {
      return containerRect;
    },
  };
  target.parentElement = container;
  target.getBoundingClientRect = () => {
    const top = containerRect.top + contentTop - container.scrollTop;
    return {
      x: 140,
      y: top,
      width: 120,
      height: 30,
      left: 140,
      top,
      right: 260,
      bottom: top + 30,
    };
  };
  const hit = (x, y) => {
    const targetRect = target.getBoundingClientRect();
    if (
      x >= targetRect.left &&
      x < targetRect.right &&
      y >= targetRect.top &&
      y < targetRect.bottom
    ) {
      return covered ? overlay : target;
    }
    if (
      x >= containerRect.left &&
      x < containerRect.right &&
      y >= containerRect.top &&
      y < containerRect.bottom
    ) {
      return containerChild;
    }
    return null;
  };
  return { container, doc: documentFor(target, { hit }), target };
}

function framedScrollFixture() {
  const root = {
    clientHeight: 600,
    clientWidth: 800,
    scrollHeight: 2_000,
    scrollLeft: 0,
    scrollTop: 0,
    scrollWidth: 800,
  };
  const target = button({ rect: { x: 20, y: 100, width: 120, height: 40 } });
  const child = documentFor(target);
  child.defaultView.innerHeight = 300;
  child.defaultView.innerWidth = 400;
  const frame = {
    localName: "iframe",
    parentElement: null,
    clientHeight: 300,
    clientLeft: 2,
    clientTop: 2,
    clientWidth: 400,
    contentDocument: child,
    contains(candidate) {
      return candidate === this;
    },
    getBoundingClientRect() {
      const top = 700 - root.scrollTop;
      return {
        x: 100,
        y: top,
        width: 404,
        height: 304,
        left: 100,
        top,
        right: 504,
        bottom: top + 304,
      };
    },
  };
  const background = { localName: "main" };
  const hit = (x, y) => {
    const rect = frame.getBoundingClientRect();
    return x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
      ? frame
      : background;
  };
  const parent = documentFor(button(), {
    frame,
    hit,
    scrollingElement: root,
  });
  return { doc: parent, root };
}

function resolveAction(doc, request, { registerTarget = true } = {}) {
  let targetDoc = doc;
  for (const selector of request.framePath) {
    targetDoc = targetDoc.querySelector(selector).contentDocument;
  }
  const target = targetDoc.querySelector(request.selector);
  if (registerTarget) {
    targetIdentityStore.set(target, {
      snapshotMarker: request.marker,
      targetRef: fixtureTargetRefs.get(target),
    });
  }
  const script = actionMatch[1]
    .replace("__TARGET_IDENTITY_STORE__", identityStoreMatch[1])
    .replace("__SENSITIVE_FIELD_POLICY__", policyMatch[1])
    .replace("__PAYLOAD__", JSON.stringify(request));
  const execute = new Function("document", "window", "location", `return (${script});`);
  return JSON.parse(execute(doc, doc.defaultView, { href: "https://fixture.invalid/" }));
}

test("a fresh visible target resolves to native coordinates", () => {
  const element = button();
  const doc = documentFor(element);
  const result = resolveAction(doc, payload());

  assert.equal(result.status, "ready");
  assert.equal(result.x, 20);
  assert.equal(result.y, 30);
  assert.equal(result.width, 120);
  assert.equal(result.height, 40);
});

test("a replaced snapshot marker returns stale_target", () => {
  const element = button({ marker: "@replacement" });
  const result = resolveAction(documentFor(element), payload());

  assert.equal(result.status, "stale_target");
});

test("a covered target returns target_obscured", () => {
  const element = button();
  const overlay = { localName: "div" };
  const result = resolveAction(documentFor(element, { hit: overlay }), payload());

  assert.equal(result.status, "target_obscured");
});

test("a target inside a covered frame returns target_obscured", () => {
  const element = button({ rect: { x: 10, y: 15, width: 80, height: 30 } });
  const child = documentFor(element);
  const frameRect = { x: 100, y: 120, width: 400, height: 300 };
  const frame = {
    localName: "iframe",
    contentDocument: child,
    contains(candidate) {
      return candidate === this;
    },
    getBoundingClientRect() {
      return {
        ...frameRect,
        left: frameRect.x,
        top: frameRect.y,
        right: frameRect.x + frameRect.width,
        bottom: frameRect.y + frameRect.height,
      };
    },
  };
  const parentElement = button();
  const overlay = { localName: "div" };
  const parent = documentFor(parentElement, { frame, hit: overlay });

  const result = resolveAction(parent, payload({ framePath: ["iframe:nth-of-type(1)"] }));
  assert.equal(result.status, "target_obscured");
  assert.match(result.message, /frame/);
});

test("menu-style select changes refuse before any native input descriptor", () => {
  for (const size of [0, 1]) {
    for (const focusStage of ["acquire", "required"]) {
      const element = selectControl({ size });
      const doc = documentFor(element);
      doc.activeElement = element;
      const request = keyboardPayload(element, { type: "select", value: "two" }, focusStage);
      const result = resolveAction(doc, request);
      assert.equal(result.status, "unsupported_native");
      assert.match(result.message, /Take over/);
      for (const field of ["x", "y", "optionIndex", "selectedIndex", "targetFocused", "targetDomFocused"]) {
        assert.equal(Object.hasOwn(result, field), false, field);
      }
      assert.equal(element.value, "one");
      assert.equal(element.selectedIndex, 0);
      assert.equal(doc.activeElement, element);
    }
  }
});

test("menu-style selects preserve no-op and invalid option results", () => {
  for (const size of [0, 1]) {
    const element = selectControl({ size });
    const doc = documentFor(element);
    const resolveValue = value => resolveAction(
      doc, keyboardPayload(element, { type: "select", value }, "acquire"),
    );
    assert.equal(resolveValue("one").status, "no_op");
    assert.equal(resolveValue("missing").status, "invalid_value");
    element.options[1].disabled = true;
    assert.equal(resolveValue("two").status, "invalid_value");
    element.options[0].disabled = true;
    assert.equal(resolveValue("one").status, "invalid_value");
    assert.equal(element.value, "one");
    assert.equal(element.selectedIndex, 0);
  }
});

test("menu-style select snapshots offer human takeover instead of select", () => {
  const snapshotMatch = semanticsSource.match(
    /const SNAPSHOT_SCRIPT: &str = r#"([\s\S]*?)"#;/,
  );
  assert.ok(snapshotMatch);
  for (const size of [0, 1, 4]) {
    const element = selectControl({ size });
    const doc = documentFor(element);
    doc.querySelectorAll = selector => selector === "iframe" ? [] : [element];
    const script = snapshotMatch[1]
      .replace("__MAX_NODES__", "25")
      .replace("__MARKER__", "__fixture_marker__")
      .replace("__TARGET_IDENTITY_STORE__", identityStoreMatch[1])
      .replace("__SENSITIVE_FIELD_POLICY__", policyMatch[1]);
    const execute = new Function("document", "window", "Node", "location", "return (" + script + ");");
    const snapshot = JSON.parse(execute(doc, doc.defaultView, { ELEMENT_NODE: 1 }, { href: "https://fixture.invalid/" }));
    const node = snapshot.nodes.find(candidate => candidate.name === "Mode");
    assert.ok(node);
    assert.equal(node.actions.includes("select"), size > 1);
    assert.equal(node.actions.includes("human_takeover"), size <= 1);
    assert.equal(node.value, "one");
  }
});

test("inline select stays pending until the requested value is confirmed", () => {
  const element = selectControl();
  const doc = documentFor(element);
  const request = payload({
    selector: "select:nth-of-type(1)",
    fingerprint: {
      href: null,
      inputType: null,
      name: "Mode",
      role: "combobox",
      sensitive: false,
      tag: "select",
    },
    action: { type: "select", value: "two" },
  });

  const initial = resolveAction(doc, request);
  assert.equal(initial.status, "ready");
  assert.equal(initial.selectedIndex, 0);
  assert.equal(initial.optionIndex, 1);

  element.selectedIndex = 1;
  const unconfirmed = resolveAction(doc, request);
  assert.equal(unconfirmed.status, "ready");

  element.value = "two";
  const confirmed = resolveAction(doc, request);
  assert.equal(confirmed.status, "no_op");
  assert.match(confirmed.message, /already selected/);
});

test("scroll_into_view targets the nearest nested overflow container", () => {
  const { container, doc } = nestedScrollFixture();
  const result = resolveAction(doc, payload({ action: { type: "scroll_into_view" } }));

  assert.equal(result.status, "ready");
  assert.equal(result.scrollDeltaX, 0);
  assert.equal(result.scrollDeltaY, 615);
  assert.ok(result.scrollX >= 100 && result.scrollX < 400);
  assert.ok(result.scrollY >= 100 && result.scrollY < 300);
  assert.equal(container.scrollTop, 0);
});

test("scroll_into_view confirms visibility after native scrolling", () => {
  const { container, doc } = nestedScrollFixture();
  const request = payload({ action: { type: "scroll_into_view" } });
  const initial = resolveAction(doc, request);
  container.scrollTop += initial.scrollDeltaY;

  const confirmed = resolveAction(doc, request);
  assert.equal(confirmed.status, "no_op");
  assert.match(confirmed.message, /visible after native scrolling/);
});

test("scroll_into_view returns target_obscured when coverage remains", () => {
  const { doc } = nestedScrollFixture({ covered: true, visible: true });
  const result = resolveAction(doc, payload({ action: { type: "scroll_into_view" } }));

  assert.equal(result.status, "target_obscured");
  assert.match(result.message, /covering/);
});

test("scroll_into_view scrolls an outer document to a bordered same-origin frame", () => {
  const { doc, root } = framedScrollFixture();
  const request = payload({
    framePath: ["iframe:nth-of-type(1)"],
    action: { type: "scroll_into_view" },
  });
  const initial = resolveAction(doc, request);

  assert.equal(initial.status, "ready");
  assert.equal(initial.y, 802);
  assert.equal(initial.scrollDeltaY, 552);
  assert.ok(initial.scrollY >= 0 && initial.scrollY < 600);

  root.scrollTop += initial.scrollDeltaY;
  const confirmed = resolveAction(doc, request);
  assert.equal(confirmed.status, "no_op");
});

test("fill phases resolve supported text controls without changing them", () => {
  for (const options of [
    { type: "text" },
    { type: "search" },
    { type: "url" },
    { type: "tel" },
    { tag: "textarea" },
  ]) {
    const element = textControl(options);
    const doc = documentFor(element);
    doc.activeElement = element;
    for (const stage of ["focus", "select_all", "insert", "verify"]) {
      const result = resolveAction(doc, fillPayload(element, stage));
      assert.equal(
        result.status,
        stage === "verify" ? "pending_native_input" : "ready",
        JSON.stringify({ options, stage }),
      );
      assert.equal(element.value, "old text");
      assert.equal(element.selectionStart, 0);
      assert.equal(element.selectionEnd, 8);
    }
  }
});

test("fill rejects controls without native text selection", () => {
  for (const type of [
    "email",
    "number",
    "date",
    "time",
    "month",
    "week",
    "datetime-local",
    "range",
    "color",
  ]) {
    const element = textControl({ type });
    const doc = documentFor(element);
    doc.activeElement = element;
    for (const stage of ["focus", "select_all", "insert", "verify"]) {
      assert.equal(
        resolveAction(doc, fillPayload(element, stage)).status,
        "unsupported_native",
        type + ":" + stage,
      );
    }
  }
});

test("fill does not dispatch while focus is stolen before selection or insertion", () => {
  for (const stage of ["select_all", "insert"]) {
    const element = textControl();
    const doc = documentFor(element);
    assert.equal(resolveAction(doc, fillPayload(element)).status, "ready");
    doc.activeElement = textControl();
    const result = resolveAction(doc, fillPayload(element, stage), { registerTarget: false });
    assert.equal(result.status, "pending_native_input", stage);
    assert.equal(element.value, "old text");
  }
});

test("fill refuses replacement elements with matching attributes and refs", () => {
  for (const stage of ["select_all", "insert", "verify"]) {
    const original = textControl();
    const doc = documentFor(original);
    const request = fillPayload(original);
    assert.equal(resolveAction(doc, request).status, "ready");
    const replacement = textControl();
    replacement.ownerDocument = doc;
    doc.querySelector = () => replacement;
    doc.activeElement = replacement;
    const result = resolveAction(doc, { ...request, fillStage: stage }, { registerTarget: false });
    assert.equal(result.status, "stale_target", stage);
    assert.match(result.message, /replaced/);
  }
});

test("fill rechecks field sensitivity before every continuation", () => {
  for (const stage of ["select_all", "insert", "verify"]) {
    const element = textControl();
    const doc = documentFor(element);
    doc.activeElement = element;
    const request = fillPayload(element);
    assert.equal(resolveAction(doc, request).status, "ready");
    element.setAttribute("autocomplete", "one-time-code");
    Object.defineProperty(element, "value", {
      get() {
        assert.fail("sensitive field values must not be read");
      },
    });
    const result = resolveAction(doc, { ...request, fillStage: stage }, { registerTarget: false });
    assert.equal(result.status, "human_takeover_required", stage);
  }
});

test("fill does not dispatch after a same-origin frame loses ancestor focus", () => {
  for (const stage of ["select_all", "insert"]) {
    const element = textControl();
    const child = documentFor(element);
    child.activeElement = element;
    const frame = button({ rect: { x: 100, y: 100, width: 400, height: 300 } });
    frame.localName = "iframe";
    frame.contentDocument = child;
    const parent = documentFor(button(), { frame, hit: frame });
    const request = { ...fillPayload(element, stage), framePath: ["iframe:nth-of-type(1)"] };
    parent.activeElement = frame;
    assert.equal(resolveAction(parent, request).status, "ready", stage);
    parent.activeElement = button();
    const result = resolveAction(parent, request, { registerTarget: false });
    assert.equal(result.status, "pending_native_input", stage);
  }
});

test("fill does not dispatch insertion while native selection is incomplete", () => {
  for (const [start, end] of [
    [1, 8],
    [0, 7],
    [8, 8],
    [null, null],
  ]) {
    const element = textControl();
    const doc = documentFor(element);
    doc.activeElement = element;
    assert.equal(resolveAction(doc, fillPayload(element, "select_all")).status, "ready");
    element.selectionStart = start;
    element.selectionEnd = end;
    const result = resolveAction(doc, fillPayload(element, "insert"), { registerTarget: false });
    assert.equal(result.status, "pending_native_input", JSON.stringify({ start, end }));
  }
});

test("fill verifies the requested value before it reports completion", () => {
  const element = textControl();
  const doc = documentFor(element);
  doc.activeElement = element;
  const request = fillPayload(element, "verify");
  assert.equal(resolveAction(doc, request).status, "pending_native_input");
  element.value = "partial";
  assert.equal(
    resolveAction(doc, request, { registerTarget: false }).status,
    "pending_native_input",
  );
  element.value = request.action.value;
  assert.equal(resolveAction(doc, request, { registerTarget: false }).status, "no_op");
});

test("fill rejects a covered or changed field before continuation", () => {
  for (const stage of ["select_all", "insert", "verify"]) {
    for (const change of ["covered", "readonly", "disabled", "hidden", "renamed"]) {
      const element = textControl();
      const doc = documentFor(element);
      doc.activeElement = element;
      const request = fillPayload(element);
      assert.equal(resolveAction(doc, request).status, "ready");
      let status;
      if (change === "covered") {
        doc.elementFromPoint = () => button();
        status = "target_obscured";
      } else if (change === "readonly") {
        element.readOnly = true;
        status = "invalid_value";
      } else if (change === "disabled") {
        element.disabled = true;
        status = "invalid_value";
      } else if (change === "hidden") {
        element.computedStyle = { display: "none" };
        status = "stale_target";
      } else {
        element.setAttribute("aria-label", "Another field");
        status = "stale_target";
      }
      assert.equal(
        resolveAction(doc, { ...request, fillStage: stage }, { registerTarget: false }).status,
        status,
        stage + ":" + change,
      );
    }
  }
});

test("fill waits for delayed native focus and selection without changing the field", () => {
  const element = textControl();
  const doc = documentFor(element);
  const selectionRequest = fillPayload(element, "select_all");
  assert.equal(resolveAction(doc, selectionRequest).status, "pending_native_input");
  doc.activeElement = element;
  assert.equal(resolveAction(doc, selectionRequest, { registerTarget: false }).status, "ready");
  element.selectionStart = 8;
  element.selectionEnd = 8;
  const insertRequest = fillPayload(element, "insert");
  assert.equal(
    resolveAction(doc, insertRequest, { registerTarget: false }).status,
    "pending_native_input",
  );
  element.selectionStart = 0;
  assert.equal(resolveAction(doc, insertRequest, { registerTarget: false }).status, "ready");
  assert.equal(element.value, "old text");
  assert.equal(element.selectionEnd, 8);
});

test("fill waits while the page loses native document focus", () => {
  for (const stage of ["select_all", "insert"]) {
    const element = textControl();
    const doc = documentFor(element);
    doc.activeElement = element;
    const request = fillPayload(element, stage);
    assert.equal(resolveAction(doc, request).status, "ready");
    doc.hasFocus = () => false;
    assert.equal(
      resolveAction(doc, request, { registerTarget: false }).status,
      "pending_native_input",
    );
    assert.equal(element.value, "old text");
  }
});

test("fill waits while an ancestor frame document loses native focus", () => {
  for (const stage of ["select_all", "insert"]) {
    const element = textControl();
    const child = documentFor(element);
    child.activeElement = element;
    const frame = button({ rect: { x: 100, y: 100, width: 400, height: 300 } });
    frame.localName = "iframe";
    frame.contentDocument = child;
    const parent = documentFor(button(), { frame, hit: frame });
    parent.activeElement = frame;
    const request = { ...fillPayload(element, stage), framePath: ["iframe:nth-of-type(1)"] };
    assert.equal(resolveAction(parent, request).status, "ready");
    parent.hasFocus = () => false;
    assert.equal(
      resolveAction(parent, request, { registerTarget: false }).status,
      "pending_native_input",
    );
    assert.equal(element.value, "old text");
  }
});

test("focus restores native focus when the toolbar owns focus but the DOM target remains active", () => {
  const element = textControl();
  const doc = documentFor(element);
  doc.activeElement = element;
  doc.hasFocus = () => false;
  const request = keyboardPayload(element, { type: "focus" });
  const initial = resolveAction(doc, request);

  assert.equal(initial.status, "ready");
  assert.equal(initial.targetDomFocused, true);
  assert.equal(initial.targetFocused, false);
  assert.equal(
    resolveAction(doc, { ...request, focusStage: "verify" }, { registerTarget: false }).status,
    "pending_native_input",
  );

  doc.hasFocus = () => true;
  assert.equal(
    resolveAction(doc, { ...request, focusStage: "verify" }, { registerTarget: false }).status,
    "no_op",
  );
  assert.equal(element.value, "old text");
});

test("focus acquisition distinguishes a different active target without changing DOM focus", () => {
  const element = textControl();
  const doc = documentFor(element);
  const other = button();
  doc.activeElement = other;
  element.focus = () => assert.fail("the resolver must not focus a DOM target");
  element.click = () => assert.fail("the resolver must not click a DOM target");
  const request = keyboardPayload(element, { type: "focus" });
  const initial = resolveAction(doc, request);

  assert.equal(initial.status, "ready");
  assert.equal(initial.targetDomFocused, false);
  assert.equal(initial.targetFocused, false);
  assert.equal(
    resolveAction(doc, { ...request, focusStage: "verify" }, { registerTarget: false }).status,
    "pending_native_input",
  );
  assert.equal(doc.activeElement, other);
});

test("focus checks every activeElement and native focus link through nested frames", () => {
  for (const depth of [0, 1, 2]) {
    const element = textControl();
    const { doc, documents, framePath } = nestedFocusFixture(element);
    const request = { ...keyboardPayload(element, { type: "focus" }, "verify"), framePath };
    assert.equal(resolveAction(doc, request).status, "no_op");
    const targetDoc = documents[depth];
    const previous = targetDoc.activeElement;
    targetDoc.activeElement = button();
    assert.equal(
      resolveAction(doc, request, { registerTarget: false }).status,
      "pending_native_input",
      "active element at depth " + depth,
    );
    const acquiring = resolveAction(
      doc,
      { ...request, focusStage: "acquire" },
      { registerTarget: false },
    );
    assert.equal(acquiring.targetDomFocused, false);
    targetDoc.activeElement = previous;
    targetDoc.hasFocus = () => false;
    assert.equal(
      resolveAction(doc, request, { registerTarget: false }).status,
      "pending_native_input",
      "native focus at depth " + depth,
    );
    const restoring = resolveAction(
      doc,
      { ...request, focusStage: "acquire" },
      { registerTarget: false },
    );
    assert.equal(restoring.targetDomFocused, true);
    assert.equal(restoring.targetFocused, false);
  }
});

test("press and select wait for native focus before their key phase", () => {
  for (const type of ["press", "select"]) {
    const element = type === "select" ? selectControl() : textControl();
    const doc = documentFor(element);
    const action = type === "select" ? { type, value: "two" } : { type, key: "Enter" };
    const request = keyboardPayload(element, action, "required");
    const previousValue = element.value;
    doc.activeElement = element;
    doc.hasFocus = () => false;
    assert.equal(resolveAction(doc, request).status, "pending_native_input", type);
    doc.hasFocus = () => true;
    const ready = resolveAction(doc, request, { registerTarget: false });
    assert.equal(ready.status, "ready", type);
    assert.equal(ready.targetFocused, true, type);
    doc.activeElement = button();
    assert.equal(
      resolveAction(doc, request, { registerTarget: false }).status,
      "pending_native_input",
      type,
    );
    assert.equal(element.value, previousValue, type);
  }
});

test("press and select require the complete nested frame focus chain", () => {
  for (const type of ["press", "select"]) {
    for (const depth of [0, 1, 2]) {
      const element = type === "select" ? selectControl() : textControl();
      const { doc, documents, framePath } = nestedFocusFixture(element);
      const action = type === "select" ? { type, value: "two" } : { type, key: "ArrowDown" };
      const request = { ...keyboardPayload(element, action, "required"), framePath };
      assert.equal(resolveAction(doc, request).status, "ready", type);
      const targetDoc = documents[depth];
      const previous = targetDoc.activeElement;
      targetDoc.activeElement = button();
      assert.equal(
        resolveAction(doc, request, { registerTarget: false }).status,
        "pending_native_input",
        type + ":active:" + depth,
      );
      targetDoc.activeElement = previous;
      targetDoc.hasFocus = () => false;
      assert.equal(
        resolveAction(doc, request, { registerTarget: false }).status,
        "pending_native_input",
        type + ":native:" + depth,
      );
    }
  }
});

test("keyboard continuation rejects replaced targets before waiting for focus", () => {
  for (const type of ["focus", "press", "select"]) {
    const makeControl = type === "select" ? selectControl : textControl;
    const original = makeControl();
    const doc = documentFor(original);
    const action =
      type === "select"
        ? { type, value: "two" }
        : type === "press"
          ? { type, key: "Enter" }
          : { type };
    const request = keyboardPayload(original, action);
    assert.equal(resolveAction(doc, request).status, "ready", type);
    const replacement = makeControl();
    replacement.ownerDocument = doc;
    doc.querySelector = () => replacement;
    doc.activeElement = replacement;
    doc.hasFocus = () => false;
    const result = resolveAction(
      doc,
      { ...request, focusStage: type === "focus" ? "verify" : "required" },
      { registerTarget: false },
    );
    assert.equal(result.status, "stale_target", type);
    assert.match(result.message, /replaced/);
  }
});

test("keyboard continuation rechecks sensitive fields before observing their values", () => {
  for (const type of ["focus", "press"]) {
    const element = textControl();
    const doc = documentFor(element);
    const action = type === "press" ? { type, key: "Enter" } : { type };
    const request = keyboardPayload(element, action);
    assert.equal(resolveAction(doc, request).status, "ready", type);
    doc.activeElement = element;
    doc.hasFocus = () => false;
    element.setAttribute("autocomplete", "one-time-code");
    Object.defineProperty(element, "value", {
      get() {
        assert.fail("sensitive field values must not be read");
      },
    });
    assert.equal(
      resolveAction(
        doc,
        { ...request, focusStage: type === "focus" ? "verify" : "required" },
        { registerTarget: false },
      ).status,
      "human_takeover_required",
      type,
    );
  }
});

test("keyboard continuation checks visibility and coverage before waiting for focus", () => {
  for (const type of ["focus", "press", "select"]) {
    for (const change of ["covered", "disabled", "hidden"]) {
      const element = type === "select" ? selectControl() : textControl();
      const doc = documentFor(element);
      const action =
        type === "select"
          ? { type, value: "two" }
          : type === "press"
            ? { type, key: "Enter" }
            : { type };
      const request = keyboardPayload(element, action);
      assert.equal(resolveAction(doc, request).status, "ready", type);
      doc.hasFocus = () => false;
      let status;
      if (change === "covered") {
        doc.elementFromPoint = () => button();
        status = "target_obscured";
      } else if (change === "disabled") {
        element.disabled = true;
        status = "invalid_value";
      } else {
        element.computedStyle = { display: "none" };
        status = "stale_target";
      }
      assert.equal(
        resolveAction(
          doc,
          { ...request, focusStage: type === "focus" ? "verify" : "required" },
          { registerTarget: false },
        ).status,
        status,
        type + ":" + change,
      );
    }
  }
});

test("keyboard continuation checks nested frame coverage before waiting for focus", () => {
  for (const type of ["focus", "press", "select"]) {
    const element = type === "select" ? selectControl() : textControl();
    const { doc, documents, framePath } = nestedFocusFixture(element);
    const action =
      type === "select"
        ? { type, value: "two" }
        : type === "press"
          ? { type, key: "Enter" }
          : { type };
    const request = { ...keyboardPayload(element, action), framePath };
    const registered = resolveAction(doc, request);
    assert.equal(registered.status, type === "focus" ? "no_op" : "ready", type);
    documents[1].hasFocus = () => false;
    doc.elementFromPoint = () => button();
    const result = resolveAction(
      doc,
      { ...request, focusStage: type === "focus" ? "verify" : "required" },
      { registerTarget: false },
    );
    assert.equal(result.status, "target_obscured", type);
    assert.match(result.message, /frame/);
  }
});

test("select waits for the preceding key to advance its index before sending another", () => {
  const element = selectControl();
  element.options.push({ disabled: false, value: "three" });
  const doc = documentFor(element);
  doc.activeElement = element;
  const request = {
    ...keyboardPayload(element, { type: "select", value: "three" }, "required"),
    previousSelectedIndex: 0,
  };
  assert.equal(resolveAction(doc, request).status, "pending_native_input");
  assert.equal(element.selectedIndex, 0);
  assert.equal(element.value, "one");

  element.selectedIndex = 1;
  element.value = "two";
  const advanced = resolveAction(doc, request, { registerTarget: false });
  assert.equal(advanced.status, "ready");
  assert.equal(advanced.selectedIndex, 1);
  assert.equal(advanced.optionIndex, 2);
  const nextStep = { ...request, previousSelectedIndex: 1 };
  assert.equal(
    resolveAction(doc, nextStep, { registerTarget: false }).status,
    "pending_native_input",
  );

  element.selectedIndex = 2;
  element.value = "three";
  assert.equal(resolveAction(doc, nextStep, { registerTarget: false }).status, "no_op");
});

test("select progress polling preserves focus, identity, and coverage checks", () => {
  for (const change of ["focus", "replacement", "covered"]) {
    const element = selectControl();
    element.options.push({ disabled: false, value: "three" });
    const doc = documentFor(element);
    doc.activeElement = element;
    const request = {
      ...keyboardPayload(element, { type: "select", value: "three" }, "required"),
      previousSelectedIndex: 0,
    };
    assert.equal(resolveAction(doc, request).status, "pending_native_input", change);
    let status;
    if (change === "focus") {
      element.selectedIndex = 1;
      element.value = "two";
      doc.hasFocus = () => false;
      status = "pending_native_input";
    } else if (change === "replacement") {
      const replacement = selectControl();
      replacement.options.push({ disabled: false, value: "three" });
      replacement.ownerDocument = doc;
      doc.querySelector = () => replacement;
      doc.activeElement = replacement;
      status = "stale_target";
    } else {
      doc.elementFromPoint = () => button();
      status = "target_obscured";
    }
    assert.equal(resolveAction(doc, request, { registerTarget: false }).status, status, change);
  }
});
