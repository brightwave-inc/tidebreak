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

function view() {
  return {
    HTMLInputElement: class {},
    HTMLSelectElement: class {},
    HTMLTextAreaElement: class {},
    innerHeight: 600,
    innerWidth: 800,
    getComputedStyle() {
      return { display: "block", visibility: "visible", opacity: "1" };
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

function documentFor(element, { hit = element, title = "Fixture", frame = null } = {}) {
  const doc = {
    activeElement: null,
    defaultView: view(),
    title,
    getElementById() {
      return null;
    },
    querySelector(selector) {
      return frame && selector === "iframe:nth-of-type(1)" ? frame : element;
    },
    elementFromPoint() {
      return hit;
    },
  };
  element.ownerDocument = doc;
  return doc;
}

function payload({ framePath = [], action = { type: "hover" } } = {}) {
  return {
    framePath,
    selector: "button:nth-of-type(1)",
    marker: "__tidebreak_marker__",
    markerValue: "@e1",
    fingerprint: {
      href: null,
      inputType: null,
      name: "Continue",
      role: "button",
      sensitive: false,
      tag: "button",
    },
    action,
  };
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
