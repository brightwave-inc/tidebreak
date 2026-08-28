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

const targetIdentityStoreKey = Symbol.for(
  "io.brightwave.tidebreak.browser.target-identities",
);
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

function selectControl({ marker = "@e1" } = {}) {
  const attributes = new Map([["aria-label", "Mode"]]);
  const element = Object.assign(new FixtureSelect(), {
    id: "mode",
    localName: "select",
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

function documentFor(
  element,
  {
    hit = element,
    title = "Fixture",
    frame = null,
    scrollingElement = null,
  } = {},
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
      return (
        candidate === this ||
        candidate === containerChild ||
        candidate === target
      );
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

function resolveAction(doc, request) {
  let targetDoc = doc;
  for (const selector of request.framePath) {
    targetDoc = targetDoc.querySelector(selector).contentDocument;
  }
  const target = targetDoc.querySelector(request.selector);
  targetIdentityStore.set(target, {
    snapshotMarker: request.marker,
    targetRef: fixtureTargetRefs.get(target),
  });
  const script = actionMatch[1]
    .replace("__TARGET_IDENTITY_STORE__", identityStoreMatch[1])
    .replace("__SENSITIVE_FIELD_POLICY__", policyMatch[1])
    .replace("__PAYLOAD__", JSON.stringify(request));
  const execute = new Function(
    "document",
    "window",
    "location",
    `return (${script});`,
  );
  return JSON.parse(
    execute(doc, doc.defaultView, { href: "https://fixture.invalid/" }),
  );
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

  const result = resolveAction(
    parent,
    payload({ framePath: ["iframe:nth-of-type(1)"] }),
  );
  assert.equal(result.status, "target_obscured");
  assert.match(result.message, /frame/);
});

test("select stays pending until the requested value is confirmed", () => {
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
  const result = resolveAction(
    doc,
    payload({ action: { type: "scroll_into_view" } }),
  );

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
  const result = resolveAction(
    doc,
    payload({ action: { type: "scroll_into_view" } }),
  );

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
