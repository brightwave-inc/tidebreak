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
const snapshotMatch = semanticsSource.match(
  /const SNAPSHOT_SCRIPT: &str = r#"([\s\S]*?)"#;/,
);
const identityStoreMatch = semanticsSource.match(
  /const TARGET_IDENTITY_STORE_SCRIPT: &str = r#"([\s\S]*?)"#;/,
);

assert.ok(policyMatch, "the browser semantics source must expose one shared policy");
assert.ok(snapshotMatch, "the browser semantics source must expose the snapshot script");
assert.ok(identityStoreMatch, "the snapshot must use private target identities");

const buildClassifier = new Function(`
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\\s+/g, " ")
    .trim()
    .slice(0, limit);
  ${policyMatch[1]}
  return tidebreakIsSensitiveField;
`);
const classify = buildClassifier();
const projectText = new Function(`
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\\s+/g, " ")
    .trim()
    .slice(0, limit);
  ${policyMatch[1]}
  return tidebreakTextWithoutFieldDescendants;
`)();

function runSensitiveSnapshot({ tag, attrs, secret }) {
  const normalized = new Map(
    Object.entries(attrs).map(([name, value]) => [name.toLowerCase(), String(value)]),
  );
  const reads = { href: 0, text: 0, value: 0 };
  const element = {
    id: "",
    localName: tag,
    nodeType: 1,
    isContentEditable: normalized.has("contenteditable"),
    labels: [],
    form: null,
    parentElement: null,
    previousElementSibling: null,
    disabled: false,
    getAttribute(name) {
      return normalized.get(name.toLowerCase()) ?? null;
    },
    getRootNode() {
      return document;
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
    getBoundingClientRect() {
      return { x: 10, y: 20, width: 120, height: 32 };
    },
  };
  Object.defineProperties(element, {
    href: {
      get() {
        reads.href += 1;
        return `https://fixture.invalid/${secret}`;
      },
    },
    innerText: {
      get() {
        reads.text += 1;
        return secret;
      },
    },
    textContent: {
      get() {
        reads.text += 1;
        return secret;
      },
    },
    value: {
      get() {
        reads.value += 1;
        return secret;
      },
    },
  });

  const document = {
    title: "Fixture",
    getElementById() {
      return null;
    },
    querySelectorAll(selector) {
      return selector === "iframe" || selector.startsWith("#") ? [] : [element];
    },
  };
  const window = {
    innerHeight: 768,
    innerWidth: 1024,
    scrollX: 0,
    scrollY: 0,
    getComputedStyle() {
      return { display: "block", visibility: "visible", opacity: "1" };
    },
  };
  const script = snapshotMatch[1]
    .replace("__MAX_NODES__", "25")
    .replace("__MARKER__", "__fixture_marker__")
    .replace("__TARGET_IDENTITY_STORE__", identityStoreMatch[1])
    .replace("__SENSITIVE_FIELD_POLICY__", policyMatch[1]);
  const execute = new Function(
    "document",
    "window",
    "Node",
    "location",
    `return (${script});`,
  );
  const result = JSON.parse(
    execute(document, window, { ELEMENT_NODE: 1 }, { href: "https://fixture.invalid" }),
  );
  return { reads, result };
}

function field({
  tag = "input",
  id = "",
  label = "",
  legend = "",
  groupLabel = "",
  peerCount = 0,
  text = "",
  attrs = {},
} = {}) {
  const normalized = new Map(
    Object.entries(attrs).map(([name, value]) => [name.toLowerCase(), String(value)]),
  );
  const element = {
    id,
    localName: tag,
    isContentEditable: normalized.has("contenteditable"),
    innerText: text,
    textContent: text,
    labels: label
      ? [{
          nodeType: 1,
          childNodes: [{ nodeType: 3, nodeValue: label }],
          matches() {
            return false;
          },
        }]
      : [],
    form: null,
    getAttribute(name) {
      if (name === "id") return id || null;
      return normalized.get(name.toLowerCase()) ?? null;
    },
    closest(selector) {
      if (selector === "fieldset" && legend) {
        return { querySelector: () => ({ textContent: legend }) };
      }
      if (selector === "[role='group']" && groupLabel) {
        return { getAttribute: () => groupLabel };
      }
      if (selector === "fieldset, [role='group'], form, div" && peerCount) {
        return {
          querySelectorAll: () =>
            Array.from({ length: peerCount }, () =>
              field({ attrs: { inputmode: "numeric", maxlength: "1" } }),
            ),
        };
      }
      return null;
    },
  };
  element.type = normalized.get("type") ?? "text";
  element.inputMode = normalized.get("inputmode") ?? "";
  element.maxLength = Number(normalized.get("maxlength") ?? -1);
  element.size = Number(normalized.get("size") ?? 20);
  return element;
}

const documentStub = { getElementById: () => null };

test("the shared policy requires human takeover for unannotated verification fields", () => {
  const sensitive = [
    field({ attrs: { name: "code", inputmode: "numeric" } }),
    field({ id: "verification-code", attrs: { inputmode: "numeric" } }),
    field({ id: "verificationCode", attrs: { inputmode: "numeric" } }),
    field({ label: "Enter the digits from your authenticator", attrs: { inputmode: "numeric" } }),
    field({ attrs: { placeholder: "Security code", inputmode: "numeric" } }),
    field({ attrs: { name: "recovery-value", placeholder: "Recovery code" } }),
    field({ legend: "Two-factor authentication", attrs: { name: "digits", inputmode: "numeric" } }),
    field({ peerCount: 6, attrs: { name: "digit-1", inputmode: "numeric", maxlength: "1" } }),
    field({ label: "Phone verification code", attrs: { name: "phone", inputmode: "numeric" } }),
    field({ attrs: { name: "challenge-response", inputmode: "numeric", pattern: "[0-9]{6}" } }),
    field({ attrs: { name: "challenge-response", inputmode: "numeric", pattern: "[0-9]{4,8}" } }),
    field({ attrs: { inputmode: "numeric", placeholder: "123 456" } }),
    field({ tag: "div", text: "314159", attrs: { role: "textbox", contenteditable: "", inputmode: "numeric" } }),
  ];

  for (const [index, element] of sensitive.entries()) {
    assert.equal(classify(element, documentStub), true, `sensitive fixture ${index + 1}`);
  }
});

test("the shared policy preserves ordinary numeric controls", () => {
  const ordinary = [
    field({ label: "Quantity", attrs: { name: "quantity", type: "number" } }),
    field({ label: "ZIP code", attrs: { name: "zipCode", inputmode: "numeric", maxlength: "5", pattern: "[0-9]{5}" } }),
    field({ label: "Year", attrs: { name: "year", type: "number" } }),
    field({ label: "Numeric search", attrs: { name: "search", inputmode: "numeric" } }),
  ];

  for (const element of ordinary) {
    assert.equal(classify(element, documentStub), false);
  }
});

test("every browser surface injects the shared policy", () => {
  for (const constant of [
    "SNAPSHOT_SCRIPT",
    "WAIT_TEXT_SCRIPT",
    "INSPECT_OVERLAY_SCRIPT",
    "SCREENSHOT_PRIVACY_SCRIPT",
    "NATIVE_ACTION_RESOLUTION_SCRIPT",
  ]) {
    const start = semanticsSource.indexOf(`const ${constant}: &str`);
    assert.notEqual(start, -1, `${constant} must exist`);
    const next = semanticsSource.indexOf("\nconst ", start + 6);
    const body = semanticsSource.slice(start, next === -1 ? undefined : next);
    assert.match(body, /__SENSITIVE_FIELD_POLICY__/);
  }
});

test("snapshot text strips editable descendants before serialization", () => {
  assert.match(semanticsSource, /const tidebreakTextWithoutFieldDescendants/);
  assert.match(semanticsSource, /node\.nodeType === 3/);
  assert.match(semanticsSource, /node\.matches\?\.\(tidebreakFieldSelector\)/);
  assert.match(semanticsSource, /const text = sensitive \? "" : contentText\(element\)/);
});

test("parent text never reads or serializes editable descendant values", () => {
  let valueReads = 0;
  const text = (value) => ({ nodeType: 3, nodeValue: value });
  const sensitiveInput = {
    nodeType: 1,
    childNodes: [],
    matches: () => true,
    get value() {
      valueReads += 1;
      return "831204";
    },
  };
  const sensitiveEditable = {
    nodeType: 1,
    childNodes: [text("314159")],
    matches: () => true,
  };
  const parent = {
    nodeType: 1,
    childNodes: [text("Public status"), sensitiveInput, sensitiveEditable],
    matches: () => false,
  };

  assert.equal(projectText(parent), "Public status");
  assert.equal(valueReads, 0);
});

test("sensitive textarea and contenteditable values never reach snapshot fields", () => {
  for (const scenario of [
    {
      tag: "textarea",
      attrs: { "aria-label": "Verification code", inputmode: "numeric" },
      secret: "831204",
    },
    {
      tag: "div",
      attrs: {
        "aria-label": "Security code",
        contenteditable: "",
        inputmode: "numeric",
        role: "textbox",
      },
      secret: "314159",
    },
  ]) {
    const { reads, result } = runSensitiveSnapshot(scenario);
    assert.equal(reads.text, 0);
    assert.equal(reads.value, 0);
    assert.equal(reads.href, 0);
    assert.equal(JSON.stringify(result).includes(scenario.secret), false);
    assert.equal(result.nodes.length, 1);
    assert.deepEqual(result.nodes[0].actions, ["human_takeover"]);
    assert.equal(result.nodes[0].name, "Sensitive field");
    assert.equal(result.nodes[0].text, null);
    assert.equal(result.nodes[0].value, null);
    assert.equal(result.nodes[0].href, null);
  }
});


test("the non-auth fixture marker remains writable under the native privacy policy", async () => {
  const source = await readFile(resolve(fixtureRoot, "recovery.html"), "utf8");
  const input = source.match(/<input\b([^>]+)>/);
  const label = source.match(/<label for="([^"]+)">([^<]+)<\/label>/);
  assert.ok(input);
  assert.ok(label);
  const attrs = Object.fromEntries(
    Array.from(input[1].matchAll(/([a-z-]+)="([^"]*)"/g), (match) => [match[1], match[2]]),
  );
  assert.equal(attrs.id, label[1]);
  assert.equal(classify(field({ id: attrs.id, label: label[2], attrs }), documentStub), false);
});
