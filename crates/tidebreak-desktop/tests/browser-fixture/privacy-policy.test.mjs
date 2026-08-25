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

assert.ok(policyMatch, "the browser semantics source must expose one shared policy");

const buildClassifier = new Function(`
  const clean = (value, limit = 240) => String(value || "")
    .replace(/\\s+/g, " ")
    .trim()
    .slice(0, limit);
  ${policyMatch[1]}
  return tidebreakIsSensitiveField;
`);
const classify = buildClassifier();

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
    labels: label ? [{ textContent: label }] : [],
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

  for (const element of sensitive) {
    assert.equal(classify(element, documentStub), true);
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
    "INSPECT_OVERLAY_SCRIPT",
    "SCREENSHOT_PRIVACY_SCRIPT",
    "ACTION_SCRIPT",
  ]) {
    const start = semanticsSource.indexOf(`const ${constant}: &str`);
    assert.notEqual(start, -1, `${constant} must exist`);
    const next = semanticsSource.indexOf("\nconst ", start + 6);
    const body = semanticsSource.slice(start, next === -1 ? undefined : next);
    assert.match(body, /__SENSITIVE_FIELD_POLICY__/);
  }
});
