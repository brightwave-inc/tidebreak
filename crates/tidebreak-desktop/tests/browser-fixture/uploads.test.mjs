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
const uploadMatch = semanticsSource.match(
  /const BROWSER_UPLOAD_SCRIPT: &str = r#"([\s\S]*?)"#;/,
);
const identityStoreMatch = semanticsSource.match(
  /const TARGET_IDENTITY_STORE_SCRIPT: &str = r#"([\s\S]*?)"#;/,
);

assert.ok(policyMatch, "the upload resolver must use the shared field policy");
assert.ok(uploadMatch, "the isolated browser upload script must be present");
assert.ok(identityStoreMatch, "the upload resolver must use private target identities");

const targetIdentityStoreKey = Symbol.for(
  "io.brightwave.tidebreak.browser.target-identities",
);
const targetIdentityStore = new WeakMap();
Object.defineProperty(globalThis, targetIdentityStoreKey, {
  value: targetIdentityStore,
  configurable: false,
  enumerable: false,
  writable: false,
});

class FixtureInput {
  constructor({ marker = "@e1", type = "file", disabled = false } = {}) {
    this.localName = "input";
    this.nodeType = 1;
    this.childNodes = [];
    this.isConnected = true;
    this.type = type;
    this.disabled = disabled;
    this.labels = [];
    this.form = null;
    this.events = [];
    this._files = [];
    this.attributes = new Map([["type", type]]);
    targetIdentityStore.set(this, {
      snapshotMarker: "__tidebreak_marker__",
      targetRef: marker,
    });
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  closest() {
    return null;
  }

  getRootNode() {
    return this.ownerDocument;
  }

  matches() {
    return true;
  }

  dispatchEvent(event) {
    this.events.push(event.type);
    return true;
  }
}

Object.defineProperty(FixtureInput.prototype, "files", {
  configurable: true,
  get() {
    return this._files;
  },
  set(files) {
    this._files = files;
  },
});

class FixtureFile {
  constructor(chunks, name, options) {
    this.bytes = Uint8Array.from(chunks[0]);
    this.name = name;
    this.size = this.bytes.length;
    this.type = options.type;
    this.lastModified = options.lastModified;
  }
}

class FixtureDataTransfer {
  constructor() {
    this.files = [];
    this.items = {
      add: (file) => {
        this.files.push(file);
      },
    };
  }
}

class FixtureEvent {
  constructor(type, options) {
    this.type = type;
    this.options = options;
  }
}

function documentFor(input) {
  const defaultView = {
    HTMLInputElement: FixtureInput,
    File: FixtureFile,
    DataTransfer: FixtureDataTransfer,
    Event: FixtureEvent,
    atob,
  };
  const doc = {
    defaultView,
    getElementById() {
      return null;
    },
    querySelector() {
      return input;
    },
  };
  input.ownerDocument = doc;
  return doc;
}

function payload(overrides = {}) {
  return {
    framePath: [],
    selector: "#upload-file",
    marker: "__tidebreak_marker__",
    markerValue: "@e1",
    fingerprint: {
      tag: "input",
      role: "textbox",
      name: "Sensitive field",
      inputType: "file",
      href: null,
      sensitive: true,
    },
    file: {
      name: "report.txt",
      mediaType: "text/plain",
      byteLen: 13,
      contentBase64: Buffer.from("fixture bytes").toString("base64"),
    },
    ...overrides,
  };
}

function executeUpload(doc, request) {
  const script = uploadMatch[1]
    .replace("__TARGET_IDENTITY_STORE__", identityStoreMatch[1])
    .replace("__SENSITIVE_FIELD_POLICY__", policyMatch[1])
    .replace("__PAYLOAD__", JSON.stringify(request));
  const execute = new Function("document", "window", `return (${script});`);
  return JSON.parse(execute(doc, doc.defaultView));
}

test("the isolated upload script attaches the exact file and dispatches both events", () => {
  const input = new FixtureInput();
  const result = executeUpload(documentFor(input), payload());

  assert.equal(result.status, "uploaded");
  assert.equal(input.files.length, 1);
  assert.equal(input.files[0].name, "report.txt");
  assert.equal(input.files[0].type, "text/plain");
  assert.deepEqual(Buffer.from(input.files[0].bytes).toString(), "fixture bytes");
  assert.deepEqual(input.events, ["input", "change"]);
});

test("the upload script refuses a replaced semantic target", () => {
  const input = new FixtureInput({ marker: "@replacement" });
  const result = executeUpload(documentFor(input), payload());

  assert.equal(result.status, "stale_target");
  assert.equal(input.files.length, 0);
  assert.deepEqual(input.events, []);
});

test("the upload script refuses non-file and disabled inputs", () => {
  const text = new FixtureInput({ type: "text" });
  const textResult = executeUpload(
    documentFor(text),
    payload({
      fingerprint: {
        tag: "input",
        role: "textbox",
        name: "",
        inputType: "text",
        href: null,
        sensitive: false,
      },
    }),
  );
  assert.equal(textResult.status, "invalid_target");

  const disabled = new FixtureInput({ disabled: true });
  const disabledResult = executeUpload(documentFor(disabled), payload());
  assert.equal(disabledResult.status, "invalid_target");
  assert.equal(disabled.files.length, 0);
});
